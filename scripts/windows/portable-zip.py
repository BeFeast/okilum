#!/usr/bin/env python3
"""Build the portable Windows ZIP and its .sha256 from a staged payload.

    portable-zip.py PAYLOAD_DIR RELEASE_VERSION

Called by scripts/build-windows-ci.sh after the cross-build, and again on the signer
runner after okilum.exe is signed (#1104), so the ZIP always holds the shipped binary.
"""
import hashlib
import sys
import zipfile
from pathlib import Path

output, version = Path(sys.argv[1]), sys.argv[2]
archive = output / f'okilum-{version}-x86_64.zip'
with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as bundle:
    # Portable is «run and leave no trace»: no background helper, no login
    # task, so the sync helper stays out of the ZIP (#1037; Sync UI #1029).
    for name in ('okilum.exe', 'README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.md'):
        bundle.write(output / name, name)
    for path in sorted((output / 'gpui-shaders').rglob('*')):
        if path.is_file():
            bundle.write(path, path.relative_to(output))
if any(n.endswith('okilum-sync-supervisor.exe') for n in zipfile.ZipFile(archive).namelist()):
    sys.exit('portable-zip.py: the portable ZIP must not contain the sync helper')
(output / (archive.name + '.sha256')).write_text(
    hashlib.sha256(archive.read_bytes()).hexdigest() + '  ' + archive.name + '\n')
print(archive)
