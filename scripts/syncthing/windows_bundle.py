#!/usr/bin/env python3
"""Stage pinned upstream Windows Syncthing and retain proof across Authenticode signing."""
import argparse
import io
import json
import os
from pathlib import Path, PurePosixPath
import runpy
import struct
import tempfile
import zipfile
import bundle

ARCHIVE_SHA = '589098269cabb9a5a1703a934305b4153609d7623e978e1c94f0f49d77ae5e9f'
BINARY_SHA = 'e33d934db704c9a3c7c46488d34ba655d676ac17234f01f3025740ffa4655dae'
BODY_SHA = '3e348ea195ff81fc3b23f6a261afcada8a8e007992ddbab88629344273ae89fc'
BODY_SIZE = 27432960
NAME = 'syncthing-windows-amd64-v2.1.6.zip'
PREFIX = 'syncthing-windows-amd64-v2.1.6/'
RESOURCES = Path('Syncthing')


def normalized_body(binary, expected_size=BODY_SIZE):
    bundle.require(256 <= len(binary) <= 100_000_000 and binary[:2] == b'MZ', 'expected Windows PE')
    pe = struct.unpack_from('<I', binary, 0x3c)[0]
    bundle.require(pe + 264 <= len(binary) and binary[pe:pe + 4] == b'PE\0\0'
                   and struct.unpack_from('<H', binary, pe + 4)[0] == 0x8664, 'expected Windows amd64 PE')
    optional = pe + 24
    bundle.require(struct.unpack_from('<H', binary, optional)[0] == 0x20b
                   and struct.unpack_from('<H', binary, pe + 20)[0] >= 240, 'expected PE32+ header')
    certificate, size = struct.unpack_from('<II', binary, optional + 144)
    bundle.require(certificate == expected_size and certificate >= optional + 240
                   and size >= 8 and certificate + size == len(binary), 'unexpected PE certificate layout')
    # The upstream executable is already signed. Re-signing may replace only
    # that certificate, the checksum and its directory entry. All executable
    # sections and the complete preceding overlay remain byte-identical.
    body = bytearray(binary[:certificate])
    body[optional + 64:optional + 68] = bytes(4)
    body[optional + 144:optional + 152] = bytes(8)
    return bytes(body)


def payload(data, expected_archive=ARCHIVE_SHA, expected_binary=BINARY_SHA, expected_body=BODY_SHA, expected_size=BODY_SIZE):
    bundle.require(len(data) <= 128 * 1024 * 1024 and bundle.digest(data) == expected_archive, 'Windows archive pin mismatch')
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        entries = archive.infolist()
        bundle.require(len(entries) <= 64 and len({e.filename for e in entries}) == len(entries), 'ambiguous Windows archive')
        bundle.require(sum(e.file_size for e in entries) <= 128 * 1024 * 1024, 'expanded Windows archive limit')
        for entry in entries:
            path = PurePosixPath(entry.filename)
            bundle.require(not path.is_absolute() and '..' not in path.parts and '\\' not in entry.filename
                           and entry.filename.startswith(PREFIX)
                           and (entry.external_attr >> 16) & 0o170000 != 0o120000, 'unsafe Windows archive entry')
        files = {}
        for name in ['syncthing.exe', 'LICENSE.txt', 'AUTHORS.txt', 'README.txt']:
            entry = archive.getinfo(PREFIX + name)
            bundle.require(not entry.is_dir() and 0 < entry.file_size <= 100_000_000, 'invalid Windows payload')
            files[name] = archive.read(entry)
    bundle.require(bundle.digest(files['syncthing.exe']) == expected_binary, 'Windows runtime pin mismatch')
    bundle.require(bundle.digest(normalized_body(files['syncthing.exe'], expected_size)) == expected_body, 'Windows runtime executable body changed')
    return files


def stage(destination, archive=None):
    proof = bundle.verify_upstream()
    signed = (bundle.ROOT / 'sha256sum-v2.1.6.txt.asc').read_text()
    bundle.require(ARCHIVE_SHA + '  ' + NAME in signed, 'Windows pin absent from signed checksums')
    with tempfile.TemporaryDirectory() as temporary:
        if archive is None:
            archive = Path(temporary) / NAME
            fetch = runpy.run_path(str(bundle.ROOT.parent / 'pinned_download.py'))['fetch']
            fetch(bundle.BASE + '/' + NAME, ARCHIVE_SHA, archive,
                  'https://api.github.com/repos/syncthing/syncthing/releases/assets/614663060')
        files = payload(archive.read_bytes())
    executable = destination / 'syncthing.exe'
    resources = destination / RESOURCES
    bundle.require(not executable.exists() and not resources.exists(), 'refuse existing Windows runtime payload')
    resources.mkdir(parents=True)
    executable.write_bytes(files.pop('syncthing.exe'))
    for name, data in files.items():
        (resources / name).write_bytes(data)
    for name in ['release-key.asc', 'sha256sum-v2.1.6.txt.asc']:
        (resources / name).write_bytes((bundle.ROOT / name).read_bytes())
    (resources / 'provenance.json').write_text(json.dumps({
        'schema': 1, 'version': '2.1.6', 'architecture': 'amd64',
        'archive_url': bundle.BASE + '/' + NAME, 'archive_sha256': ARCHIVE_SHA,
        'upstream_binary_sha256': BINARY_SHA, 'executable_body_sha256': BODY_SHA,
        'checksum_signature': proof, 'license': 'MPL-2.0',
        'source_archive_url': bundle.BASE + '/syncthing-source-v2.1.6.tar.gz',
    }, indent=2) + '\n')
    return executable


def seal(destination):
    """Record post-signing bytes only when the pinned upstream body is unchanged."""
    executable = (destination / 'syncthing.exe').read_bytes()
    bundle.require(bundle.digest(normalized_body(executable)) == BODY_SHA, 'signed Windows executable body changed')
    path = destination / RESOURCES / 'provenance.json'
    provenance = json.loads(path.read_text())
    bundle.require(provenance['archive_sha256'] == ARCHIVE_SHA and provenance['upstream_binary_sha256'] == BINARY_SHA
                   and provenance['executable_body_sha256'] == BODY_SHA, 'Windows runtime provenance changed')
    provenance['packaged_binary_sha256'] = bundle.digest(executable)
    path.write_text(json.dumps(provenance, indent=2) + '\n')


def signing_input(binary, expected_binary=BINARY_SHA, expected_body=BODY_SHA, expected_size=BODY_SIZE):
    """The pinned signer accepts unsigned PE only; retain the complete pinned body."""
    bundle.require(bundle.digest(binary) == expected_binary, 'Windows runtime pin mismatch before signing')
    body = normalized_body(binary, expected_size)
    bundle.require(bundle.digest(body) == expected_body, 'Windows runtime executable body changed before signing')
    return body


def prepare_signing(destination):
    executable = destination / 'syncthing.exe'
    body = signing_input(executable.read_bytes())
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(prefix='.syncthing-sign-', dir=destination, delete=False) as output:
            temporary = Path(output.name)
            output.write(body)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temporary, executable.stat().st_mode & 0o777)
        os.replace(temporary, executable)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['stage', 'prepare-signing', 'seal'])
    parser.add_argument('--payload', type=Path, required=True)
    parser.add_argument('--archive', type=Path)
    args = parser.parse_args()
    if args.action == 'stage':
        print(stage(args.payload, args.archive))
    elif args.action == 'prepare-signing':
        prepare_signing(args.payload)
    else:
        seal(args.payload)
