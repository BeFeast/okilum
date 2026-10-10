#!/usr/bin/env python3
"""Rebuild an unsigned Velopack payload from a published beta package, for the signing
dry run (#1104): the real pipeline runs without a 20-minute cross-build.

    payload-from-release.py DIR [VERSION]    # prints the version; default: current beta

DIR receives what scripts/build-windows-ci.sh would have staged: the package's lib/app
without the files Velopack adds itself (Squirrel.exe, the execution stub, sq.version).
"""
import json
import sys
import urllib.request
import zipfile
from io import BytesIO
from pathlib import Path

CDN = 'https://updates.befeast.com/okilum/windows/beta/'
VELOPACK_FILES = {'Squirrel.exe', 'Okilum_ExecutionStub.exe', 'sq.version'}


def get(url):
    request = urllib.request.Request(url, headers={'User-Agent': 'curl/8'})
    with urllib.request.urlopen(request, timeout=300) as response:
        return response.read()


def main():
    output = Path(sys.argv[1])
    if len(sys.argv) > 2:
        version = sys.argv[2]
    else:
        feed = json.loads(get(CDN + 'releases.beta.json'))
        version = max((a['Version'] for a in feed['Assets'] if a['Type'] == 'Full'),
                      key=lambda v: [int(p) for p in v.split('.')])
    package = zipfile.ZipFile(BytesIO(get(f'{CDN}BeFeast.Okilum-{version}-beta-full.nupkg')))
    for name in package.namelist():
        if not name.startswith('lib/app/') or name.endswith('/'):
            continue
        relative = name.removeprefix('lib/app/')
        if relative in VELOPACK_FILES:
            continue
        target = output / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(package.read(name))
    for required in ('okilum.exe', 'okilum-sync-supervisor.exe', 'okilum.ico'):
        if not (output / required).is_file():
            sys.exit(f'payload-from-release: {required} is missing from {version}')
    print(version)


if __name__ == '__main__':
    main()
