"""Completed platform artifacts, joined by source commit rather than CI run number."""
import hashlib
import json
import re

PREFIX = 'okilum/releases'
PLATFORMS = ('macos', 'windows', 'linux')


def encode(value):
    return json.dumps(value, sort_keys=True).encode()


def asset(key, name, data):
    return {'key': key, 'name': name, 'sha256': hashlib.sha256(data).hexdigest(), 'size': len(data)}


def record(store, platform, build, source, assets):
    if platform not in PLATFORMS or not re.fullmatch('[0-9a-f]{40}', source):
        raise ValueError('Invalid platform or source commit')
    value = {'platform': platform, 'build': build, 'source': source, 'assets': assets}
    key = f'{PREFIX}/{source}/{platform}.json'
    old = store.call('GET', key)
    if old:
        previous = json.loads(old)
        if previous['build'] > build or (previous['build'] == build and previous != value):
            raise ValueError('Conflicting platform publication')
    store.put(key, encode(value), 'application/json', 'no-cache')


def bundle(store, source, build):
    result = {}
    for platform in PLATFORMS:
        raw = store.call('GET', f'{PREFIX}/{source}/{platform}.json')
        if raw is None:
            return None
        item = json.loads(raw)
        if item['source'] != source or item['platform'] != platform:
            raise ValueError('Mixed source commits in release')
        result[platform] = item
    if result['macos']['build'] != build:
        raise ValueError('Selected macOS build was superseded for this commit')
    return {'build': build, 'source': source, 'platforms': result}


def download(store, release):
    """Verify every asset before a caller can change any public channel."""
    files = {}
    for platform in PLATFORMS:
        for item in release['platforms'][platform]['assets']:
            name = item['name']
            if name in files or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', name):
                raise ValueError('Duplicate or unsafe asset name')
            data = store.call('GET', item['key'])
            if data is None or len(data) != item['size'] or hashlib.sha256(data).hexdigest() != item['sha256']:
                raise ValueError(f'Artifact verification failed: {name}')
            files[name] = data
    files['SHA256SUMS'] = ''.join(f'{hashlib.sha256(data).hexdigest()}  {name}\n'
                                  for name, data in sorted(files.items())).encode()
    return files
