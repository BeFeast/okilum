#!/usr/bin/env python3
"""Isolated, unsigned Arch fork validation. Never accepted hosted evidence.

The local required Linux gate remains in ci.yml. Only the packaging workflow
compiles this Arch release/default-brain path; makepkg consumes those exact bytes.
Checksums here prove consistency, not authenticity or publication eligibility.
"""
import argparse
from datetime import datetime, timezone
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('fork_payload', ROOT / 'scripts/ci/package-linux-binary.py')
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)
SCHEMA = 'okilum-untrusted-fork-v1'
WORKFLOW = '.forgejo/workflows/linux-release.yml'


def output(*args):
    return subprocess.check_output(args, text=True).strip()


def now():
    return datetime.now(timezone.utc).isoformat()


def context():
    p.require(os.environ.get('GITHUB_EVENT_NAME') == 'pull_request', 'Fork path requires a pull request')
    event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text())
    pr = event['pull_request']
    repository = os.environ['GITHUB_REPOSITORY']
    p.require(pr['base']['repo']['full_name'] == repository
              and pr['head']['repo']['full_name'] != repository, 'Fork path rejects trusted sources')
    source = output('git', 'rev-parse', 'HEAD')
    head, base = pr['head']['sha'], pr['base']['sha']
    p.require(all(re.fullmatch('[0-9a-f]{40}', s) for s in (source, head, base)), 'Expected exact source/head/base SHA')
    p.require(source == os.environ['GITHUB_SHA'], 'Checkout differs from event source')
    # Forgejo may check out a synthetic merge. Record it as the tested source;
    # never stamp head's identity on bytes compiled from that merge.
    if source == head:
        kind = 'head'
    else:
        parents = output('git', 'show', '-s', '--format=%P', source).split()
        p.require(parents == [base, head], 'Source is neither exact PR head nor its declared base/head merge')
        kind = 'merge'
    result = dict(repository=repository, head_repository=pr['head']['repo']['full_name'],
        workflow=WORKFLOW, event='pull_request', source_sha=source, source_kind=kind,
        head_sha=head, base_sha=base, pr_number=int(event['number']),
        run_id=int(os.environ['GITHUB_RUN_ID']), run_number=int(os.environ['GITHUB_RUN_NUMBER']))
    p.require(all(result[k] > 0 for k in ('pr_number', 'run_id', 'run_number')), 'Invalid fork run identity')
    return result


def configuration():
    # Includes the canonical compiler inputs, but has a separate Arch/fork domain.
    paths = ('scripts/arch/fork-production.py', 'scripts/arch/PKGBUILD',
             'scripts/arch/build.sh', WORKFLOW)
    return p.digest(p.encode(dict(domain=SCHEMA, platform='archlinux-x86_64',
        compiler_inputs=p.config(), tooling={name: p.digest((ROOT / name).read_bytes()) for name in paths})))


def artifact_name(source, config):
    p.artifact_name(source, config)  # Validate identities, retain strict archive rules.
    return f'linux-untrusted-fork-v1-{source}-{p.TARGET}-release-{config}'


def validate_manifest(m, source, config, producer=None):
    p.require(m.get('schema') == SCHEMA and m.get('trust') == 'untrusted-fork'
              and m.get('source_sha') == source and m.get('config') == config, 'Wrong fork schema/source/config')
    artifact_name(source, config)
    p.require(m.get('target') == p.TARGET and m.get('profile') == 'release'
              and m.get('features') == ['brain'] and m.get('harness') is False
              and m.get('gpui_test_support') is False, 'Wrong fork production configuration')
    identity = m.get('producer', {})
    p.require(identity.get('workflow') == WORKFLOW and identity.get('event') == 'pull_request'
              and identity.get('repository') != identity.get('head_repository')
              and identity.get('source_sha') == source
              and all(type(identity.get(k)) is int and identity[k] > 0
                      for k in ('run_id', 'run_number', 'pr_number'))
              and type(m.get('build')) is int and m['build'] == 5000 + identity['run_number'], 'Invalid fork producer')
    p.require(all(re.fullmatch('[0-9a-f]{40}', identity.get(k, '')) for k in ('head_sha', 'base_sha')),
              'Missing fork head/base identity')
    p.require(identity.get('source_kind') in ('head', 'merge')
              and ((source == identity['head_sha']) == (identity['source_kind'] == 'head')), 'Wrong fork source kind')
    if producer is not None:
        p.require(identity == producer, 'Fork producer differs from current request')
    p.require(re.fullmatch('[0-9a-f]{64}', m.get('binary_sha256', '')), 'Missing fork binary checksum')
    provenance = m.get('provenance', {})
    p.require(all(provenance.get(k) for k in ('rustc', 'cargo', 'toolchain', 'cargo_lock', 'vendor',
                                            'os_release', 'rustflags')), 'Incomplete fork provenance')
    p.require(provenance['rustflags'] == p.RUSTFLAGS, 'Wrong fork compiler flags')
    p.require(p.timestamp(m['timing']['product_build_started']) <= p.timestamp(m['timing']['product_build_completed']),
              'Invalid fork build timing')
    abi = m.get('elf', {})
    p.require(abi.get('machine') == 'x86_64' and abi.get('interpreter') == '/lib64/ld-linux-x86-64.so.2'
              and abi.get('needed') and abi.get('symbol_versions') and abi.get('runtime_libraries'), 'Incomplete fork ELF facts')
    p.build_info('\n'.join(f'{k}: {v}' for k, v in abi.get('build_info', {}).items()), m)
    return m


def validate_receipt(receipt, source, config):
    p.require(receipt.get('schema') == SCHEMA and receipt.get('trust') == 'untrusted-fork'
              and receipt.get('publication_eligible') is False, 'Not an unsigned fork validation receipt')
    p.require(not any(k in receipt for k in ('artifact_id', 'artifact_api_digest', 'artifact_zip_sha256', 'expires_at')),
              'Fork receipt cannot claim hosted artifact acceptance')
    m = validate_manifest(receipt.get('manifest', {}), source, config)
    p.require(re.fullmatch('[0-9a-f]{64}', receipt.get('archive_sha256', '')), 'Missing fork archive checksum')
    return m


def package_receipt(path, source, build):
    # Intentionally separate from the canonical authenticated package consumer.
    # This reader can prove only unsigned package consistency, never authority.
    raw = subprocess.check_output(['zstd', '-q', '-d', '-c', str(path)])
    with tarfile.open(fileobj=io.BytesIO(raw), mode='r:') as archive:
        members = archive.getmembers()
        names = [m.name.removeprefix('./').rstrip('/') for m in members]
        p.require(len(names) == len(set(names)), 'Duplicate fork package member')
        p.require(all(not Path(n).is_absolute() and '..' not in Path(n).parts and '\\' not in n
                      and (m.isfile() or m.isdir()) and not m.issym() and not m.islnk()
                      for m, n in zip(members, names)), 'Unsafe fork package member')
        contents = {}
        for name in ('usr/bin/okilum', 'usr/share/okilum/production-receipt.json'):
            entries = [m for m, n in zip(members, names) if n == name]
            p.require(len(entries) == 1 and entries[0].isfile(), 'Missing/unsafe fork package member')
            contents[name] = archive.extractfile(entries[0]).read()
    receipt = json.loads(contents['usr/share/okilum/production-receipt.json'])
    m = validate_receipt(receipt, source, receipt.get('manifest', {}).get('config', ''))
    binary = contents['usr/bin/okilum']
    p.require(m['build'] == build and p.digest(binary) == m['binary_sha256'],
              'Package/source/build/binary drift (including stripping)')
    return receipt, binary


def verify(root, identity, config, inspector=None):
    inspector = inspector or p.inspect
    saved = root / 'receipt.json'
    p.require(saved.is_file() and not saved.is_symlink(), 'Missing/unsafe fork receipt')
    receipt = json.loads(saved.read_text())
    m = validate_receipt(receipt, identity['source_sha'], config)
    p.require(m['producer'] == identity, 'Fork receipt differs from current event')
    name = artifact_name(identity['source_sha'], config)
    archive = root / (name + '.tar.zst')
    checksum = archive.with_suffix('.zst.sha256')
    p.require(not archive.is_symlink() and not checksum.is_symlink()
              and p.digest(archive.read_bytes()) == receipt['archive_sha256'], 'Fork archive changed')
    # Derive authoritative local manifest/bytes from the same checked archive,
    # not from another editable receipt hash. This still conveys no trust.
    with tempfile.TemporaryDirectory() as tmp:
        extracted = Path(tmp) / 'payload'
        actual = p.unpack(archive.read_bytes(), checksum.read_text(), extracted,
            identity['source_sha'], config, identity, inspector,
            manifest_validator=validate_manifest, name_factory=artifact_name)
        p.require(actual == m, 'Fork receipt differs from archived manifest')
        for name in p.FILES | {'SHA256SUMS'}:
            local = root / 'payload' / name
            p.require(not local.is_symlink() and local.read_bytes() == (extracted / name).read_bytes(),
                      'Fork consumed payload differs from archive')
    return receipt


def provenance():
    compiler = output('rustc', '-vV')
    pin = re.search(r'^channel = "([0-9.]+)"$', (ROOT / 'rust-toolchain.toml').read_text(), re.M)[1]
    p.require(f'release: {pin}' in compiler.splitlines(), 'Compiler differs from pinned toolchain')
    cargo_home = Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo')))
    p.require(not any((cargo_home / name).exists() for name in ('config', 'config.toml')), 'Untracked Cargo configuration')
    return dict(rustc=compiler, cargo=output('cargo', '-V'), linker=output('mold', '--version'),
        toolchain=(ROOT / 'rust-toolchain.toml').read_text(), cargo_lock=p.digest((ROOT / 'Cargo.lock').read_bytes()),
        vendor=output('git', '-C', 'vendor/gpui-component', 'rev-parse', 'HEAD'),
        patches={f.name: p.digest(f.read_bytes()) for f in sorted((ROOT / 'scripts/patches').glob('*.diff'))},
        os_release=Path('/etc/os-release').read_text(), rustflags=p.RUSTFLAGS,
        cargo_incremental='0', cargo_build_jobs='4', profile_overrides='repository defaults')


def produce(root, identity, config, inspector=None):
    inspector = inspector or p.inspect
    if (root / 'receipt.json').exists():
        verify(root, identity, config, inspector)
        return False
    p.require(int(os.environ.get('GITHUB_RUN_ATTEMPT', '1')) == 1,
              'Fork rerun without saved bytes requires a new validation run')
    env = dict(os.environ, OKILUM_SOURCE_COMMIT=identity['source_sha'],
        OKILUM_BUILD_VERSION=str(5000 + identity['run_number']),
        OKILUM_RELEASE_VERSION=f"0.1.{5000 + identity['run_number']}",
        RUSTFLAGS=p.RUSTFLAGS, CARGO_INCREMENTAL='0', CARGO_BUILD_JOBS='4')
    p.require(not any(k.startswith('CARGO_PROFILE_RELEASE_') or k in
              ('CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET', 'CARGO_BUILD_RUSTFLAGS',
               'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RUSTC_BOOTSTRAP') for k in env),
              'Unexpected fork shipping configuration override')
    m = dict(schema=SCHEMA, trust='untrusted-fork', source_sha=identity['source_sha'], config=config,
        target=p.TARGET, profile='release', features=['brain'], harness=False, gpui_test_support=False,
        build=5000 + identity['run_number'], producer=identity, provenance=provenance(), timing={})
    root.mkdir(parents=True, exist_ok=True)
    # Exclusive attempt marker: an interrupted/failed compiler is never an excuse
    # for the consumer (or another local invocation) to compile a second payload.
    with (root / 'attempt.json').open('x') as marker:
        marker.write(p.encode(dict(producer=identity, config=config)).decode())
    m['timing']['product_build_started'] = now()
    subprocess.run(['cargo', 'build', '--release', '--locked', '-p', 'okilum-shell', '--bin', 'okilum'],
                   env=env, check=True)
    m['timing']['product_build_completed'] = now()
    archive = p.package(ROOT / 'target/release/okilum', root, m, inspector,
        manifest_validator=validate_manifest, name_factory=artifact_name)
    m = p.unpack(archive.read_bytes(), archive.with_suffix('.zst.sha256').read_text(), root / 'payload',
        identity['source_sha'], config, identity, inspector,
        manifest_validator=validate_manifest, name_factory=artifact_name)
    receipt = dict(schema=SCHEMA, trust='untrusted-fork', publication_eligible=False,
        manifest=m, archive_sha256=p.digest(archive.read_bytes()))
    (root / 'receipt.json').write_bytes(p.encode(receipt))
    return True


def package(root, identity, config):
    receipt = verify(root, identity, config)
    build = receipt['manifest']['build']
    dest = ROOT / 'dist/arch'
    dest.mkdir(parents=True, exist_ok=True)
    # This path never queries/reuses the signed archive or invokes a signer.
    for name in ('PKGBUILD', 'okilum.install'):
        shutil.copyfile(ROOT / 'scripts/arch' / name, dest / name)
    pkgbuild = dest / 'PKGBUILD'
    pkgbuild.write_text(re.sub(r'^pkgver=.*$', f'pkgver=0.1.{build}', pkgbuild.read_text(), flags=re.M))
    shutil.copyfile(root / 'receipt.json', dest / 'production-receipt.json')
    env = dict(os.environ, OKILUM_SOURCE=str(ROOT), OKILUM_PREBUILT_DIR=str(root.resolve()),
               OKILUM_BUILD_VERSION=str(build), OKILUM_RELEASE_VERSION=f'0.1.{build}')
    subprocess.run(['makepkg', '--cleanbuild', '--noconfirm'], cwd=dest, env=env, check=True)
    packages = list(dest.glob('okilum-*.pkg.tar.zst'))
    p.require(len(packages) == 1, 'Missing/ambiguous fork package')
    embedded, binary = package_receipt(packages[0], identity['source_sha'], build)
    p.require(embedded == receipt and binary == (root / 'payload/okilum').read_bytes(), 'Fork package bytes/receipt changed')
    (dest / 'SHA256SUMS').write_text(p.digest(packages[0].read_bytes()) + '  ' + packages[0].name + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('produce', 'package'))
    parser.add_argument('--prebuilt', type=Path, default=ROOT / 'dist/linux-fork')
    args = parser.parse_args()
    identity = context()
    config = configuration()
    if args.command == 'produce':
        produce(args.prebuilt.resolve(), identity, config)
    else:
        package(args.prebuilt.resolve(), identity, config)
