#!/usr/bin/env python3
"""Publish unsigned Velopack artifacts; publish feeds last, promote without rebuild."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'updater'))
from release import R2
import catalog

CONTRACT = json.loads(Path(__file__).with_name('channel.json').read_text())
PREFIX = CONTRACT['prefix']
assert CONTRACT['default_channel'] == 'beta'


def validate_feed(feed, read):
    assets = feed['Assets']
    if not assets:
        raise ValueError('Empty release feed')
    for asset in assets:
        name = asset['FileName']
        if not re.fullmatch(r'BeFeast\.Okilum-[0-9][A-Za-z0-9.+-]*-beta-(full|delta)\.nupkg', name):
            raise ValueError('Invalid package name')
        if asset['PackageId'] != 'BeFeast.Okilum':
            raise ValueError('Unexpected package identity')
        data = read(name)
        if data is None or len(data) != asset['Size'] or hashlib.sha256(data).hexdigest().upper() != asset['SHA256'].upper():
            raise ValueError(f'Package verification failed: {name}')
    return assets


def version_key(value):
    if not re.fullmatch(r'0\.1\.[0-9]+', value):
        raise ValueError(f'Unexpected release version: {value}')
    return tuple(map(int, value.split('.')))


def publish(root, build, source, store, portable=None, archive_only=False):
    portable_data = portable.read_bytes() if portable else None
    feed = json.loads((root / 'releases.beta.json').read_bytes())
    assets = validate_feed(feed, lambda name: (root / name).read_bytes())
    version = f'0.1.{build}'
    if max(version_key(a['Version']) for a in assets) != version_key(version):
        raise ValueError('Build does not match latest package')
    base = f'{PREFIX}/beta'
    previous_build = store.call('GET', f'{PREFIX}/builds/{build}/release.json')
    if previous_build and json.loads(previous_build)['source'] != source:
        raise ValueError('Build number already belongs to another source')
    current = store.call('GET', f'{base}/releases.beta.json')
    if (not archive_only and current
            and max(version_key(a['Version']) for a in json.loads(current)['Assets']) > version_key(version)):
        raise ValueError('Refusing to roll back the beta feed')
    for asset in assets:
        store.put(f"{base}/{asset['FileName']}", (root / asset['FileName']).read_bytes(), 'application/octet-stream')
    setup = (root / 'BeFeast.Okilum-beta-Setup.exe').read_bytes()
    if not setup.startswith(b'MZ'):
        raise ValueError('Setup is not a PE executable')
    metadata = {'build': build, 'version': version, 'source': source,
                'setup_sha256': hashlib.sha256(setup).hexdigest(),
                'feed': feed}
    # Retain an immutable installer and complete feed for exact-build promotion.
    store.put(f'{PREFIX}/builds/{build}/Setup.exe', setup, 'application/octet-stream')
    store.put(f'{PREFIX}/builds/{build}/release.json', json.dumps(metadata).encode(), 'application/json')
    # An explicit older commit (#1040) is archived for promotion only: packages stay
    # addressable by their unique names, the beta feed and installer keep the newer build.
    if not archive_only:
        store.put(f'{base}/Setup.exe', setup, 'application/octet-stream', 'no-cache')
        store.put(f'{base}/releases.beta.json', json.dumps(feed).encode(), 'application/json', 'no-cache')

    if portable_data is not None:
        archive = f'{PREFIX}/builds/{build}'
        store.put(f'{archive}/Okilum-windows-portable.zip', portable_data, 'application/zip')
        catalog.record(store, 'windows', build, source, [
            catalog.asset(f'{archive}/Setup.exe', 'Setup.exe', setup),
            catalog.asset(f'{archive}/Okilum-windows-portable.zip', 'Okilum-windows-portable.zip', portable_data)])


def prepare(root, store):
    root.mkdir(parents=True, exist_ok=True)
    data = store.call('GET', f'{PREFIX}/beta/releases.beta.json')
    if data is None:
        return
    feed = json.loads(data)
    full = [a for a in feed['Assets'] if a['Type'] == 'Full']
    latest = max(full, key=lambda a: version_key(a['Version']))
    selected = {'Assets': [latest]}
    blobs = {}
    def read(name):
        blobs[name] = store.call('GET', f'{PREFIX}/beta/{name}')
        return blobs[name]
    validate_feed(selected, read)
    for name, blob in blobs.items():
        (root / name).write_bytes(blob)
    (root / 'releases.beta.json').write_text(json.dumps(selected))


def promote(build, store):
    data = store.call('GET', f'{PREFIX}/builds/{build}/release.json')
    if data is None:
        raise ValueError('Unknown build')
    metadata = json.loads(data)
    if metadata['build'] != build or metadata['version'] != f'0.1.{build}':
        raise ValueError('Promotion metadata mismatch')
    # Stable needs only the selected full package. No cross-channel delta chain.
    feed = {'Assets': [a for a in metadata['feed']['Assets']
                       if a['Version'] == metadata['version'] and a['Type'] == 'Full']}
    blobs = {}
    def read(name):
        blobs[name] = store.call('GET', f'{PREFIX}/beta/{name}')
        return blobs[name]
    validate_feed(feed, read)
    old = store.call('GET', f'{PREFIX}/stable/releases.stable.json')
    if old and max(version_key(a['Version']) for a in json.loads(old)['Assets']) > version_key(metadata['version']):
        raise ValueError('Refusing to roll back stable')
    setup = store.call('GET', f'{PREFIX}/builds/{build}/Setup.exe')
    if setup is None or hashlib.sha256(setup).hexdigest() != metadata['setup_sha256']:
        raise ValueError('Installer verification failed')
    for name, blob in blobs.items():
        store.put(f'{PREFIX}/stable/{name}', blob, 'application/octet-stream')
    store.put(f'{PREFIX}/stable/Setup.exe', setup, 'application/octet-stream', 'no-cache')
    store.put(f'{PREFIX}/stable/releases.stable.json', json.dumps(feed).encode(), 'application/json', 'no-cache')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    p = commands.add_parser('prepare')
    p.add_argument('--directory', type=Path, required=True)
    p = commands.add_parser('publish')
    p.add_argument('--directory', type=Path, required=True)
    p.add_argument('--build', type=int, required=True)
    p.add_argument('--source', required=True)
    p.add_argument('--portable', type=Path, required=True)
    p.add_argument('--archive-only', action='store_true',
                   help='record the build for promotion without changing the beta feed')
    p = commands.add_parser('promote')
    p.add_argument('--build', type=int, required=True)
    a = parser.parse_args()
    store = R2()
    if a.command == 'prepare': prepare(a.directory, store)
    elif a.command == 'publish': publish(a.directory, a.build, a.source, store, a.portable, a.archive_only)
    else: promote(a.build, store)
