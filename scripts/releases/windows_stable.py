#!/usr/bin/env python3
"""Promote one signed Windows build/source, independent of Mac and Linux."""
import argparse
import hashlib
import json
import re
import subprocess

import catalog
from publish import windows

PREFIX = 'okilum/windows'


class Plan:
    """Pin all read bytes and validate every planned write before publication."""
    def __init__(self, store):
        self.store, self.reads, self.writes = store, {}, []

    def call(self, method, key):
        if method != 'GET':
            raise ValueError('Windows plan only supports reads')
        if key not in self.reads:
            self.reads[key] = self.store.call(method, key)
        return self.reads[key]

    def put(self, key, data, content_type, cache=None):
        if not key.startswith(PREFIX + '/stable/'):
            raise ValueError('Windows promotion attempted a cross-platform write')
        self.writes.append((key, data, content_type, cache))


def execute(store, source, build):
    if not re.fullmatch('[0-9a-f]{40}', source) or build < 1:
        raise ValueError('Expected a full lowercase source SHA and positive Windows build')
    plan = Plan(store)
    descriptor_raw = plan.call('GET', f'{catalog.PREFIX}/{source}/windows.json')
    metadata_raw = plan.call('GET', f'{PREFIX}/builds/{build}/release.json')
    if descriptor_raw is None or metadata_raw is None:
        raise ValueError('Missing exact-source Windows catalog or archived metadata')
    descriptor, metadata = json.loads(descriptor_raw), json.loads(metadata_raw)
    if (descriptor.get('platform'), descriptor.get('source'), descriptor.get('build')) != ('windows', source, build):
        raise ValueError('Windows catalog source/build mismatch')
    if (metadata.get('source'), metadata.get('build')) != (source, build):
        raise ValueError('Windows archived source/build mismatch')
    # This independent path is signed-only even when a legacy variable is unset.
    if metadata.get('signed') is not True or not re.fullmatch('[0-9a-f]{64}', metadata.get('signer_sha256', '')):
        raise ValueError('Windows Stable requires signed=true and a signer fingerprint')
    assets = descriptor.get('assets', [])
    if len(assets) != 2 or {a['name'] for a in assets} != {'Setup.exe', 'Okilum-windows-portable.zip'}:
        raise ValueError('Expected exact Windows installer and portable catalog')
    for asset in assets:
        if asset['key'] != f"{PREFIX}/builds/{build}/{asset['name']}":
            raise ValueError('Unexpected Windows archive key')
        data = plan.call('GET', asset['key'])
        if data is None or len(data) != asset['size'] or hashlib.sha256(data).hexdigest() != asset['sha256']:
            raise ValueError('Windows archived asset checksum/size mismatch')
    # Existing Velopack validation checks package hashes, installer and monotonic
    # Stable version. Cache makes these checks use the exact metadata above.
    windows.promote(build, plan)
    receipt_key = f'{PREFIX}/promotions/{build}-{source}.json'
    identity = {'source': source, 'build': build, 'descriptor': descriptor,
                'metadata_sha256': hashlib.sha256(metadata_raw).hexdigest()}
    prior = store.call('GET', receipt_key)
    if prior:
        receipt = json.loads(prior)
        if not isinstance(receipt, dict) or receipt.get('identity') != identity:
            raise ValueError('Windows promotion identity changed during retry')
    store.put(receipt_key, catalog.encode({'identity': identity, 'state': 'prepared'}), 'application/json', 'no-cache')
    # Real promoter emits the feed last. All validations happened before this loop.
    for key, data, content_type, cache in plan.writes:
        store.put(key, data, content_type, cache)
    store.put(receipt_key, catalog.encode({'identity': identity, 'state': 'complete'}), 'application/json', 'no-cache')
    print(f'Published Windows Stable only: {build} source {source}')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True)
    parser.add_argument('--build', required=True, type=int)
    args = parser.parse_args(argv)
    if not re.fullmatch('[0-9a-f]{40}', args.source) or args.build < 1:
        parser.error('Expected full source SHA and positive Windows build')
    subprocess.run(['git', 'merge-base', '--is-ancestor', args.source, 'origin/main'], check=True)
    execute(windows.R2(), args.source, args.build)


if __name__ == '__main__':
    main()
