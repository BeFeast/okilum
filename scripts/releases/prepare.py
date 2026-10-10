#!/usr/bin/env python3
"""Promote an explicit main commit (#1040): build what is missing, then promote it.

    python3 scripts/releases/prepare.py request <full sha>

Stable promotion needs all three platforms built from one source. For a commit that
main has already moved past, the hourly and half-hourly ticks never build it. A
request records the source, dispatches the Linux and Windows release workflows with
that source (they publish to the build archive only, never to the beta feed), and
returns. Each completed publication calls `complete`, which dispatches the ordinary
`releases.yml build=<mac>` promotion once the last missing build has landed. Nothing
waits on a runner, so publication and promotion keep their single lock.

macOS is not built here: its lane signs on the M4 under separate rules. A source
without a macOS build is refused with that reason.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'updater'))
import catalog

REQUESTS = f'{catalog.PREFIX}/requests'
WORKFLOWS = {'linux': 'linux-release.yml', 'windows': 'windows-diagnostic.yml'}


def descriptor(store, source, platform):
    raw = store.call('GET', f'{catalog.PREFIX}/{source}/{platform}.json')
    return json.loads(raw) if raw else None


def missing(store, source):
    return [platform for platform in WORKFLOWS if descriptor(store, source, platform) is None]


def check_source(source, ancestor_of='origin/main'):
    if not re.fullmatch('[0-9a-f]{40}', source or ''):
        raise ValueError('The source must be a full 40-character commit SHA')
    result = subprocess.run(['git', 'merge-base', '--is-ancestor', source, ancestor_of],
                            capture_output=True, text=True, timeout=30)
    if result.returncode != 0:
        raise ValueError(f'{source} is not a commit on main')


def promote(client, build):
    client.call('POST', '/actions/workflows/releases.yml/dispatches',
                {'ref': 'main', 'inputs': {'build': str(build)}})


def request(store, client, source, ancestor_of='origin/main'):
    """Start the missing builds for `source`, or promote at once if none is missing."""
    check_source(source, ancestor_of)
    mac = descriptor(store, source, 'macos')
    if mac is None:
        raise ValueError(f'No macOS build for {source}. Build it on the M4 lane first; '
                         'Linux and Windows are then built here automatically.')
    gaps = missing(store, source)
    if not gaps:
        print(f'All three platforms exist for {source}: promoting macOS build {mac["build"]}')
        promote(client, mac['build'])
        return []
    store.put(f'{REQUESTS}/{source}.json', catalog.encode(
        {'source': source, 'macos': mac['build'], 'state': 'waiting', 'missing': gaps,
         'requested': int(time.time())}), 'application/json', 'no-cache')
    for platform in gaps:
        client.call('POST', f'/actions/workflows/{WORKFLOWS[platform]}/dispatches',
                    {'ref': 'main', 'inputs': {'source': source}})
        print(f'Dispatched {platform} build for {source}')
    return gaps


def complete(store, client, source):
    """Called after each publication: promote a waiting request once nothing is missing."""
    raw = store.call('GET', f'{REQUESTS}/{source}.json')
    if raw is None:
        return False
    pending = json.loads(raw)
    if pending.get('state') != 'waiting' or missing(store, source):
        return False
    mac = descriptor(store, source, 'macos')
    if mac is None:
        return False
    # Mark first: a retry of this publication must not promote twice.
    store.put(f'{REQUESTS}/{source}.json', catalog.encode(
        {**pending, 'state': 'promoting', 'build': mac['build'], 'missing': []}),
        'application/json', 'no-cache')
    promote(client, mac['build'])
    print(f'All three platforms exist for {source}: promoting macOS build {mac["build"]}')
    return True


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    sub.add_parser('request').add_argument('source')
    args = parser.parse_args()
    from release import Forgejo, R2
    request(R2(), Forgejo(), args.source)
