#!/usr/bin/env python3
"""Explicit final signed pacman release followed by durable publication freeze."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import tempfile

import linux

MARKER = 'okilum/arch/frozen.json'
NOTICE = b'Okilum Linux is transitioning to the signed AppImage'


def execute(store, signing, tmp, source, build):
    if not re.fullmatch('[0-9a-f]{40}', source) or build < 1:
        raise ValueError('Expected full source SHA and positive final pacman build')
    frozen = store.call('GET', MARKER)
    identity = {'state': 'frozen', 'source': source, 'build': build}
    if frozen is not None:
        if json.loads(frozen) != identity:
            raise ValueError('Pacman already frozen at another final release')
        return
    manifest, package, signature = linux.candidate(store, source, build)
    path = tmp / manifest['filename']; path.write_bytes(package)
    Path(str(path) + '.sig').write_bytes(signature); signing.verify(path)
    install = subprocess.run(['bsdtar', '-xOf', str(path), '.INSTALL'], capture_output=True, check=True).stdout
    if NOTICE not in install or b'https://updates.befeast.com/okilum/linux/stable/Okilum-x86_64.AppImage' not in install:
        raise ValueError('Final pacman package lacks AppImage migration notice')
    linux.execute(store, signing, tmp, source, build)
    # Freeze only after the signed final Stable transaction completed.
    store.put(MARKER, linux.catalog.encode(identity),
              'application/json', 'no-cache')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True)
    parser.add_argument('--build', type=int, required=True)
    args = parser.parse_args(argv)
    if not re.fullmatch('[0-9a-f]{40}', args.source) or args.build < 1:
        parser.error('Expected full source SHA and positive final pacman build')
    subprocess.run(['git', 'merge-base', '--is-ancestor', args.source, 'origin/main'], check=True)
    with tempfile.TemporaryDirectory() as directory, linux.arch.SigningKey() as signing:
        execute(linux.arch.R2(), signing, Path(directory), args.source, args.build)


if __name__ == '__main__':
    main()
