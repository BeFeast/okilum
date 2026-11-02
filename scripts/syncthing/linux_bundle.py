#!/usr/bin/env python3
"""Stage the signed-upstream pinned Linux runtime; never execute it."""
import argparse
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import struct
import tarfile
import urllib.request
import bundle

ARCHIVE_SHA = '524ef4e1df1850b719e2378c8369956f4e5f2ab9a46fdf7f45f2b925012147aa'
BINARY_SHA = 'd72663e4f9efe0b8de5e1dacb13c1c8decc82e76623cf2fa47fd306f3e1d3a3e'
NAME = 'syncthing-linux-amd64-v2.1.6.tar.gz'
PREFIX = 'syncthing-linux-amd64-v2.1.6/'

def payload(data, expected_archive=ARCHIVE_SHA, expected_binary=BINARY_SHA):
    bundle.require(len(data) <= 128 * 1024 * 1024 and bundle.digest(data) == expected_archive,
                   'Linux archive pin mismatch')
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
        members = archive.getmembers()
        # The pinned release has 45 entries, including service examples. Bound
        # the complete inventory even though only four files are staged.
        bundle.require(len(members) <= 64 and len({m.name for m in members}) == len(members), 'ambiguous archive')
        bundle.require(sum(m.size for m in members) <= 128 * 1024 * 1024,
                       'expanded Linux archive limit')
        for member in members:
            path = PurePosixPath(member.name)
            bundle.require(not path.is_absolute() and '..' not in path.parts
                           and (member.name == PREFIX.rstrip('/') or member.name.startswith(PREFIX))
                           and (member.isfile() or member.isdir()), 'unsafe Linux archive entry')
        result = {}
        for name in ['syncthing', 'LICENSE.txt', 'AUTHORS.txt', 'README.txt']:
            member = archive.getmember(PREFIX + name)
            bundle.require(member.isfile() and 0 < member.size <= 100_000_000, 'invalid Linux payload')
            result[name] = archive.extractfile(member).read(member.size + 1)
            bundle.require(len(result[name]) == member.size, 'truncated Linux payload')
    binary = result['syncthing']
    bundle.require(len(binary) >= 64 and binary[:6] == b'\x7fELF\x02\x01'
                   and struct.unpack_from('<H', binary, 18)[0] == 62, 'expected Linux amd64 ELF')
    bundle.require(bundle.digest(binary) == expected_binary, 'Linux runtime pin mismatch')
    return result

def stage(appdir, archive):
    bundle.verify_upstream()
    signed = (bundle.ROOT / 'sha256sum-v2.1.6.txt.asc').read_text()
    bundle.require(ARCHIVE_SHA + '  ' + NAME in signed, 'Linux pin absent from signed checksums')
    if archive is None:
        with urllib.request.urlopen(bundle.BASE + '/' + NAME, timeout=60) as response:
            data = response.read(128 * 1024 * 1024 + 1)
    else:
        data = archive.read_bytes()
    files = payload(data)
    destination = appdir / 'usr/lib/okilum/syncthing'
    bundle.require(not destination.exists(), 'refuse existing Linux runtime payload')
    destination.mkdir(parents=True, mode=0o755)
    for name, content in files.items():
        target = destination / name
        with target.open('xb') as stream:
            stream.write(content)
        target.chmod(0o555 if name == 'syncthing' else 0o444)
    (destination / 'provenance.json').write_text(json.dumps({
        'version': '2.1.6', 'architecture': 'amd64', 'archive_sha256': ARCHIVE_SHA,
        'binary_sha256': BINARY_SHA, 'upstream_fingerprint': bundle.FINGERPRINT,
    }, sort_keys=True) + '\n')
    (destination / 'provenance.json').chmod(0o444)
    return destination

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--appdir', type=Path, required=True)
    parser.add_argument('--archive', type=Path)
    args = parser.parse_args()
    print(stage(args.appdir, args.archive))
