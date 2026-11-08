#!/usr/bin/env python3
"""Stage pinned CI archives, retaining checksum verification on API fallback."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import runpy
import tempfile
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]


def dependency(name):
    # Asset IDs identify the exact releases in the existing checksum locks.
    # A lock upgrade must update its ID too; a mismatch fails closed below.
    if name == 'sparkle':
        lock = json.loads((ROOT / 'scripts/updater/sparkle-lock.json').read_text())
        return (lock['url'],
                'https://api.github.com/repos/sparkle-project/Sparkle/releases/assets/562081725',
                lock['archive_sha256'])
    lock = runpy.run_path(str(ROOT / 'scripts/ci/sync-fixture-stage.py'))
    return (lock['URL'],
            'https://api.github.com/repos/syncthing/syncthing/releases/assets/614662906',
            lock['ARCHIVE_SHA256'])


def verified(data, checksum):
    if hashlib.sha256(data).hexdigest() != checksum:
        raise ValueError('CI dependency SHA256 mismatch')
    return data


def fetch(url, asset_api, checksum, destination):
    destination = Path(destination)
    if destination.exists():
        verified(destination.read_bytes(), checksum)
        return
    try:
        with urllib.request.urlopen(url, timeout=60) as response:
            data = response.read()
    except urllib.error.URLError as error:
        if isinstance(error, urllib.error.HTTPError) and not 500 <= error.code < 600:
            raise
        # The official asset API redirects to the same public release bytes.
        # Never fall back after a checksum failure or accept metadata as an archive.
        print(f'CI dependency primary transport failed: {error}; trying official asset API', flush=True)
        if isinstance(error, urllib.error.HTTPError):
            error.close()
        request = urllib.request.Request(asset_api, headers={'Accept': 'application/octet-stream'})
        with urllib.request.urlopen(request, timeout=60) as response:
            data = response.read()
    verified(data, checksum)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=destination.parent, prefix='.dependency-', delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        except BaseException:
            temporary.unlink()
            raise
    try:
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('dependency', choices=['sparkle', 'syncthing'])
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    fetch(*dependency(args.dependency), args.destination)
