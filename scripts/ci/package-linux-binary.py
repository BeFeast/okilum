#!/usr/bin/env python3
"""Canonical Linux production payload: strict identity, checksums and extraction."""
import argparse
from datetime import datetime
import hashlib
import io
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SCHEMA = 1
TARGET = 'x86_64-unknown-linux-gnu'
WORKFLOW = '.github/workflows/forgejo-linux.yml'
RUSTFLAGS = '-C link-arg=-fuse-ld=mold'
FILES = {'okilum', 'SOURCE_SHA', 'manifest.json', 'LICENSE', 'THIRD_PARTY_NOTICES.md'}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def encode(value):
    return (json.dumps(value, sort_keys=True, indent=2) + '\n').encode()


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def config(root=ROOT):
    paths = [root / p for p in ('Cargo.lock', 'Cargo.toml', 'rust-toolchain.toml',
             'crates/okilum-shell/Cargo.toml', 'scripts/vendor-setup.sh',
             'scripts/ci/package-linux-binary.py', 'scripts/ci/github-linux.py',
             '.github/workflows/forgejo-linux.yml')]
    paths += sorted((root / 'scripts/patches').glob('*.diff'))
    paths += sorted(p for p in (root / '.cargo').rglob('*') if p.is_file())
    return digest(encode({'files': {str(p.relative_to(root)): digest(p.read_bytes()) for p in paths},
        'target': TARGET, 'profile': 'release', 'features': ['brain'], 'rustflags': RUSTFLAGS,
        'command': ['cargo', 'build', '--release', '--locked', '-p', 'okilum-shell', '--bin', 'okilum']}))


def artifact_name(sha, configuration):
    require(bool(re.fullmatch('[0-9a-f]{40}', sha)), 'Expected full source SHA')
    require(bool(re.fullmatch('[0-9a-f]{64}', configuration)), 'Expected config digest')
    return f'linux-production-v1-{sha}-{TARGET}-release-{configuration}'


def timestamp(value):
    require(isinstance(value, str), 'Missing actual timestamp')
    parsed = datetime.fromisoformat(value.replace('Z', '+00:00'))
    require(parsed.tzinfo is not None, 'Timestamp needs timezone')
    return parsed


def build_info(text, manifest):
    lines = [line.split(': ', 1) for line in text.strip().splitlines()]
    require(all(len(line) == 2 for line in lines), 'Malformed build info')
    facts = dict(lines)
    require(len(facts) == len(lines), 'Duplicate build info')
    expected = {'source': manifest['source_sha'], 'build': str(manifest['build']),
        'version': f"0.1.{manifest['build']}", 'profile': 'release', 'target': TARGET,
        'brain': 'yes', 'settings-ui-harness': 'no', 'updater-ui-harness': 'no',
        'gpui-test-support': 'no'}
    require(all(facts.get(k) == v for k, v in expected.items()), 'Nonproduction or wrong-source build info')
    return facts


def verify_binary(binary, manifest):
    # Consumers on the bridge need no GUI/runtime libraries. The digest binds these
    # bytes to the producer's executed build-info/ELF validation; Arch re-executes it.
    header = binary.read_bytes()[:64]
    require(digest(binary.read_bytes()) == manifest['binary_sha256'] and len(header) == 64
            and header[:6] == b'\x7fELF\x02\x01' and struct.unpack_from('<H', header, 18)[0] == 62,
            'Wrong ELF or binary digest')
    return manifest['elf']


def inspect(binary, manifest):
    header = binary.read_bytes()[:64]
    require(len(header) == 64 and header[:6] == b'\x7fELF\x02\x01'
            and struct.unpack_from('<H', header, 18)[0] == 62, 'Expected Linux x86_64 ELF')
    program = subprocess.check_output(['readelf', '-lW', str(binary)], text=True)
    require('[Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]' in program,
            'Wrong Linux ELF interpreter')
    dynamic = subprocess.check_output(['readelf', '-dW', str(binary)], text=True)
    versions = subprocess.check_output(['readelf', '-VW', str(binary)], text=True)
    facts = build_info(subprocess.check_output([str(binary.resolve()), '--build-info'], text=True), manifest)
    subprocess.run([str(binary.resolve()), '--help'], check=True, stdout=subprocess.DEVNULL)
    libraries = subprocess.check_output(['ldd', str(binary)], text=True)
    require('not found' not in libraries, 'Missing runtime library')
    return {'machine': 'x86_64', 'interpreter': '/lib64/ld-linux-x86-64.so.2',
        'needed': sorted(re.findall(r'\(NEEDED\).*?\[(.*?)\]', dynamic)),
        'symbol_versions': sorted(set(re.findall(r'\b(?:GLIBC|GLIBCXX)_[0-9.]+', versions))),
        'build_info': facts, 'runtime_libraries': libraries}


def validate_manifest(m, sha, configuration, producer=None):
    require(m.get('schema') == SCHEMA and m.get('source_sha') == sha, 'Wrong schema/source')
    artifact_name(sha, configuration)
    require(m.get('config') == configuration and m.get('target') == TARGET
            and m.get('profile') == 'release' and m.get('features') == ['brain']
            and m.get('harness') is False and m.get('gpui_test_support') is False,
            'Wrong production configuration')
    require(type(m.get('build')) is int and m['build'] > 5000, 'Invalid original build')
    p = m.get('producer', {})
    require(p.get('repository') == 'BeFeast/okilum' and p.get('workflow') == WORKFLOW
            and p.get('event') == 'push' and p.get('source_sha') == sha
            and all(type(p.get(k)) is int and p[k] > 0 for k in ('run_id', 'attempt', 'forgejo_run_id', 'forgejo_run_attempt', 'forgejo_run_number'))
            and m['build'] == 5000 + p['forgejo_run_number'], 'Invalid producer identity')
    if producer is not None:
        require(p == producer, 'Wrong producer receipt')
    require(bool(re.fullmatch('[0-9a-f]{64}', m.get('binary_sha256', ''))), 'Missing binary digest')
    provenance = m.get('provenance', {})
    require(all(provenance.get(k) for k in ('rustc', 'cargo', 'linker', 'toolchain', 'cargo_lock', 'vendor', 'patches', 'environment')),
            'Incomplete build provenance')
    require(isinstance(provenance.get('cargo_config'), dict), 'Missing Cargo configuration record')
    require(re.fullmatch('[0-9a-f]{64}', provenance['cargo_lock'])
            and re.fullmatch('[0-9a-f]{40}', provenance['vendor'])
            and all(re.fullmatch('[0-9a-f]{64}', v) for v in provenance['patches'].values()),
            'Invalid lock/vendor/patch identity')
    require(all(provenance['environment'].get(k) for k in
            ('os_release', 'rustflags', 'cargo_incremental', 'cargo_build_jobs', 'profile_overrides')),
            'Incomplete compiler environment')
    timing = m.get('timing', {})
    require(all(timing.get(k) for k in ('hosted_created', 'hosted_job_started', 'product_build_started', 'product_build_completed')),
            'Missing original build timing')
    require(timestamp(timing['hosted_created']) <= timestamp(timing['hosted_job_started'])
            <= timestamp(timing['product_build_started']) <= timestamp(timing['product_build_completed']), 'Invalid build timing')
    abi = m.get('elf', {})
    require(abi.get('machine') == 'x86_64' and abi.get('interpreter') == '/lib64/ld-linux-x86-64.so.2'
            and abi.get('needed') and abi.get('symbol_versions') and abi.get('runtime_libraries'), 'Incomplete ELF provenance')
    build_info('\n'.join(f'{k}: {v}' for k, v in abi.get('build_info', {}).items()), m)
    return m


def package(binary, output, manifest, inspector=inspect):
    require(binary.is_file() and not binary.is_symlink(), 'Expected regular compiled binary')
    m = dict(manifest, binary_sha256=digest(binary.read_bytes()))
    m['elf'] = inspector(binary, m)
    validate_manifest(m, m['source_sha'], m['config'])
    output.mkdir(parents=True, exist_ok=True)
    name = artifact_name(m['source_sha'], m['config'])
    archive = output / (name + '.tar.zst')
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary) / name
        root.mkdir()
        shutil.copyfile(binary, root / 'okilum')
        (root / 'okilum').chmod(0o755)
        (root / 'SOURCE_SHA').write_text(m['source_sha'] + '\n')
        (root / 'manifest.json').write_bytes(encode(m))
        for notice in ('LICENSE', 'THIRD_PARTY_NOTICES.md'):
            shutil.copyfile(ROOT / notice, root / notice)
        (root / 'SHA256SUMS').write_text(''.join(
            digest((root / name).read_bytes()) + '  ' + name + '\n' for name in sorted(FILES)))
        subprocess.run(['tar', '--zstd', '-cf', str(archive.resolve()), '-C', temporary, name], check=True)
    archive.with_suffix('.zst.sha256').write_text(digest(archive.read_bytes()) + '  ' + archive.name + '\n')
    return archive


def unpack(data, checksum, output, sha, configuration, producer=None, inspector=inspect):
    name = artifact_name(sha, configuration)
    require(checksum == digest(data) + '  ' + name + '.tar.zst\n', 'Outer archive checksum mismatch')
    raw = subprocess.check_output(['zstd', '-q', '-d', '-c'], input=data)
    with tarfile.open(fileobj=io.BytesIO(raw), mode='r:') as archive:
        members = archive.getmembers()
        expected = {name} | {name + '/' + f for f in FILES | {'SHA256SUMS'}}
        names = [m.name.rstrip('/') for m in members]
        require(len(names) == len(set(names)) and set(names) == expected, 'Unexpected/missing/duplicate archive member')
        require(all((m.isdir() if m.name.rstrip('/') == name else m.isfile())
                    and not m.issym() and not m.islnk() for m in members), 'Unsafe archive member')
        require(all(m.size < 1024 ** 3 for m in members), 'Oversized archive member')
        payload = {f: archive.extractfile(name + '/' + f).read() for f in FILES | {'SHA256SUMS'}}
    sums = ''.join(digest(payload[f]) + '  ' + f + '\n' for f in sorted(FILES))
    require(payload['SHA256SUMS'].decode() == sums, 'Incomplete or incorrect internal checksums')
    m = validate_manifest(json.loads(payload['manifest.json']), sha, configuration, producer)
    require(payload['SOURCE_SHA'] == (sha + '\n').encode()
            and digest(payload['okilum']) == m['binary_sha256'], 'Source or executable digest mismatch')
    require(not output.exists(), 'Extraction destination must be new')
    output.mkdir(parents=True)
    try:
        for f, contents in payload.items():
            (output / f).write_bytes(contents)
        (output / 'okilum').chmod(0o755)
        actual = inspector(output / 'okilum', m)
        require(all(actual[k] == m['elf'][k] for k in actual if k != 'runtime_libraries'),
                'ELF/build facts changed')
    except Exception:
        shutil.rmtree(output)
        raise
    return m


def validate_receipt(receipt, source, configuration, *, trusted_envelope=False):
    """Validate fields and independently authenticate unsigned local receipts.

    Only the resolver's API-digest-bound ZIP or verified package signature can
    supply trusted_envelope=True. Local tar/checksum/receipt fields cannot.
    """
    m = validate_manifest(receipt.get('manifest', {}), source, configuration, receipt.get('producer'))
    require(receipt.get('schema') == 1 and receipt.get('producer') == m['producer']
            and type(receipt.get('artifact_id')) is int and receipt['artifact_id'] > 0
            and re.fullmatch('sha256:[0-9a-f]{64}', receipt.get('artifact_api_digest', ''))
            and all(re.fullmatch('[0-9a-f]{64}', receipt.get(k, '')) for k in ('archive_sha256', 'artifact_zip_sha256'))
            and all(receipt.get(k) for k in ('expires_at', 'hosted_created', 'hosted_job_started', 'verified_at')),
            'Incomplete accepted artifact receipt')
    require(receipt['artifact_api_digest'] == 'sha256:' + receipt['artifact_zip_sha256']
            and timestamp(receipt['hosted_created']) <= timestamp(receipt['hosted_job_started'])
            <= timestamp(receipt['verified_at']) < timestamp(receipt['expires_at']), 'Invalid accepted checksum/timing receipt')
    if not trusted_envelope:
        authenticate_receipt(receipt, source, configuration)
    return m


def receipt_authority():
    # Lazy import avoids a module initialization cycle with the transport. No
    # app execution: the resolver uses the static ELF/digest inspector here.
    spec = importlib.util.spec_from_file_location('receipt_authority', Path(__file__).with_name('github-linux.py'))
    authority = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(authority)
    return authority


def authenticate_receipt(receipt, source, configuration):
    authority = receipt_authority()
    api = authority.transport.GitHub(os.environ.get('MIRROR_TOKEN') or os.environ.get('GH_TOKEN') or '')
    with tempfile.TemporaryDirectory() as temporary:
        canonical = authority.resolve(api, source, Path(temporary), configuration=configuration)
    require(all(receipt.get(k) == canonical.get(k) for k in (
        'manifest', 'producer', 'artifact_id', 'artifact_api_digest', 'artifact_zip_sha256', 'archive_sha256')),
        'Unsigned receipt differs from independently authenticated artifact')


def package_receipt(path, source, build, *, trusted_envelope=False):
    """Read exact regular members without extracting package-controlled paths."""
    raw = subprocess.check_output(['zstd', '-q', '-d', '-c', str(path)])
    with tarfile.open(fileobj=io.BytesIO(raw), mode='r:') as archive:
        members = archive.getmembers()
        names = [m.name.removeprefix('./').rstrip('/') for m in members]
        require(len(names) == len(set(names)), 'Duplicate package member')
        require(all(not Path(n).is_absolute() and '..' not in Path(n).parts and '\\' not in n
                    and (m.isfile() or m.isdir()) and not m.issym() and not m.islnk()
                    for m, n in zip(members, names)), 'Unsafe package member')
        wanted = {}
        for name in ('usr/bin/okilum', 'usr/share/okilum/production-receipt.json'):
            entries = [m for m, n in zip(members, names) if n == name]
            require(len(entries) == 1 and entries[0].isfile(), 'Missing/unsafe production package member')
            wanted[name] = archive.extractfile(entries[0]).read()
    receipt = json.loads(wanted['usr/share/okilum/production-receipt.json'])
    m = validate_receipt(receipt, source, receipt.get('manifest', {}).get('config', ''), trusted_envelope=trusted_envelope)
    require(m['build'] == build and digest(wanted['usr/bin/okilum']) == m['binary_sha256'],
            'Package/source/build/binary drift (including stripping)')
    return receipt, wanted['usr/bin/okilum']


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    args = parser.parse_args()
    print(package(args.binary, args.output, json.loads(args.manifest.read_text())))
