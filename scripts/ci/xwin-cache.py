#!/usr/bin/env python3
"""Independent, checked cargo-xwin SDK cache; a bad cache is only a miss."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import urllib.request

VERSION = '0.23.1'
CHANNEL = 'https://aka.ms/vs/17/release/channel'
RECEIPT = '.tessera-integrity.json'


def manifest_hash():
    with urllib.request.urlopen(CHANNEL, timeout=45) as response:
        channel = json.load(response)
    item = next(x for x in channel['channelItems']
                if x['id'] == 'Microsoft.VisualStudio.Manifests.VisualStudio')
    digest = item['payloads'][0]['sha256']
    if len(digest) != 64 or any(c not in '0123456789abcdef' for c in digest):
        raise ValueError('Invalid Microsoft manifest digest')
    return digest


def inventory(root):
    result = {}
    for path in sorted((root / 'xwin').rglob('*')):
        name = path.relative_to(root).as_posix()
        if name == RECEIPT:
            continue
        if path.is_symlink():
            path.resolve(strict=True).relative_to(root.resolve())
            result[name] = ['link', os.readlink(path)]
        elif path.is_file():
            with path.open('rb') as stream:
                digest = hashlib.sha256()
                for chunk in iter(lambda: stream.read(1024 * 1024), b''):
                    digest.update(chunk)
                result[name] = ['sha256', digest.hexdigest()]
    if 'xwin/DONE' not in result or not any(n.endswith('.lib') for n in result):
        raise ValueError('Incomplete xwin SDK tree')
    return result


def valid(root, key):
    try:
        saved = json.loads((root / RECEIPT).read_text())
        return saved['key'] == key and saved['files'] == inventory(root)
    except (OSError, ValueError, KeyError):
        return False


def prepare(root, key, download):
    if valid(root, key):
        print('xwin-cache: integrity verified; hit', flush=True)
        return 'hit'
    print('xwin-cache: missing/invalid receipt; download fallback', flush=True)
    # Only the caller-owned dedicated SDK tree is discarded, never CARGO_HOME.
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    download()
    (root / RECEIPT).write_text(json.dumps({'key': key, 'files': inventory(root)}, sort_keys=True))
    return 'miss'


def cache_root(directory, workspace):
    root = Path(directory).resolve()
    expected = Path(workspace).resolve() / 'target/xwin-sdk'
    if root != expected:
        raise ValueError('Refuse to clear anything except workspace/target/xwin-sdk')
    return root


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['key', 'prepare', 'benchmark'])
    args = parser.parse_args()
    if args.command == 'key':
        # No broad restore-prefix: different Microsoft manifests never mix.
        key = f'xwin-v2-linux-x86_64-cargo-{VERSION}-' + manifest_hash()
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write(f'key={key}\n')
        return
    root = cache_root(os.environ['XWIN_CACHE_DIR'], os.environ['GITHUB_WORKSPACE'])
    key = os.environ['XWIN_CACHE_KEY']
    env = {**os.environ, 'XWIN_ARCH': 'x86_64', 'XWIN_VARIANT': 'desktop', 'XWIN_VERSION': '17'}
    def download():
        subprocess.run(['cargo', 'xwin', 'cache', 'xwin'], env=env, check=True)
    if args.command == 'benchmark':
        # Same runner/session, SDK setup only. Cold download is the positive control.
        shutil.rmtree(root, ignore_errors=True)
        measurements = {}
        for label in ['cold', 'warm']:
            start = time.monotonic()
            result = prepare(root, key, download)
            # Exercise upstream DONE consumption as the real build does.
            if label == 'warm':
                download()
            measurements[label] = {'seconds': time.monotonic() - start, 'result': result}
        print('XWIN_BENCHMARK=' + json.dumps(measurements), flush=True)
    else:
        prepare(root, key, download)


if __name__ == '__main__':
    main()
