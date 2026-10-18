#!/usr/bin/env python3
"""Owner-approved, exact-source Arch stable promotion. Run under the publication lock."""
import argparse
import base64
import datetime
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import tempfile

import catalog

spec = importlib.util.spec_from_file_location('arch_publish', Path(__file__).resolve().parents[1] / 'arch/publish.py')
arch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(arch)

PREFIX = 'okilum/arch/stable/x86_64'
FEEDS = ('okilum-stable.db.sig', 'okilum-stable.db', 'latest.json')
ACTIVE = 'okilum/arch/stable/promotion.json'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def snapshot(data):
    return None if data is None else base64.b64encode(data).decode('ascii')


def restore(value):
    return None if value is None else base64.b64decode(value, validate=True)


def candidate(store, source, build):
    name = arch.package_name(build)
    archive = f'okilum/arch/builds/{build}'
    raw = store.call('GET', f'{catalog.PREFIX}/{source}/linux.json')
    meta = store.call('GET', f'{archive}/manifest.json')
    if raw is None or meta is None:
        raise ValueError('Missing exact-source Linux catalog or archived manifest')
    descriptor, manifest = json.loads(raw), json.loads(meta)
    if (descriptor.get('platform'), descriptor.get('source'), descriptor.get('build')) != ('linux', source, build):
        raise ValueError('Linux catalog source/build mismatch')
    if (manifest.get('source'), manifest.get('build'), manifest.get('filename')) != (source, build, name):
        raise ValueError('Arch manifest source/build/filename mismatch')
    assets = descriptor.get('assets', [])
    if len(assets) != 2 or {a['name'] for a in assets} != {name, name + '.sig'}:
        raise ValueError('Expected exactly the Arch package and detached signature')
    files = {}
    for asset in assets:
        if asset['key'] != f'{archive}/{asset["name"]}':
            raise ValueError('Unexpected Linux archive key')
        data = store.call('GET', asset['key'])
        if data is None or len(data) != asset['size'] or digest(data) != asset['sha256']:
            raise ValueError('Linux asset checksum/size mismatch')
        files[asset['name']] = data
    if digest(files[name]) != manifest.get('sha256'):
        raise ValueError('Arch manifest checksum mismatch')
    return manifest, files[name], files[name + '.sig']


def verify_pair(signing, tmp, feeds):
    db, sig = feeds['okilum-stable.db'], feeds['okilum-stable.db.sig']
    if all(value is None for value in feeds.values()):
        return
    if any(value is None for value in feeds.values()):
        raise ValueError('Incomplete signed stable feed; reconcile before promotion')
    path = tmp / 'verify.db'
    path.write_bytes(db)
    Path(str(path) + '.sig').write_bytes(sig)
    signing.verify(path)


class Plan:
    """Use the real Arch validation/repo-add path without publishing its planned PUTs."""
    def __init__(self, store):
        self.store, self.writes = store, {}

    def call(self, method, key):
        return self.store.call(method, key)

    def put(self, key, data, content_type, cache=None):
        if not key.startswith(PREFIX + '/'):
            raise ValueError('Linux promotion attempted a write outside stable Arch')
        self.writes[key] = (data, content_type, cache)


def execute(store, signing, tmp, source, build, rollback=False):
    if not re.fullmatch('[0-9a-f]{40}', source):
        raise ValueError('Expected full lowercase source SHA')
    arch.package_name(build)
    receipt_key = f'okilum/arch/promotions/{build}-{source}.json'
    active = store.call('GET', ACTIVE)
    if active is not None and json.loads(active)['receipt'] != receipt_key:
        prior = json.loads(store.call('GET', json.loads(active)['receipt']))
        if prior['state'] not in ('complete', 'rolled_back'):
            raise ValueError('Finish or roll back the interrupted Linux promotion first')
    raw = store.call('GET', receipt_key)
    receipt = json.loads(raw) if raw is not None else None
    current = {name: store.call('GET', f'{PREFIX}/{name}') for name in FEEDS}
    if receipt is None:
        if rollback:
            raise ValueError('No Linux promotion receipt to roll back')
        manifest, package, signature = candidate(store, source, build)
        old = json.loads(current['latest.json']) if current['latest.json'] is not None else None
        if old and (old['build'] > build or (old['build'] == build and old != manifest)):
            raise ValueError('Refusing stable rollback or conflicting build identity')
        verify_pair(signing, tmp, current)
        plan = Plan(store)
        arch.publish_channel(plan, signing, tmp, 'stable', manifest, package, signature)
        # An identical immutable package URL must never acquire different bytes.
        for name in (manifest['filename'], manifest['filename'] + '.sig'):
            key = f'{PREFIX}/{name}'
            previous = store.call('GET', key)
            if previous is not None and previous != plan.writes[key][0]:
                raise ValueError('Conflicting immutable stable payload')
        receipt = {'source': source, 'build': build, 'state': 'prepared', 'started_at': now(),
                   'manifest': manifest,
                   'before': {name: snapshot(data) for name, data in current.items()},
                   'after': {name: snapshot(plan.writes[f'{PREFIX}/{name}'][0]) for name in FEEDS}}
        # Small durable receipt pins the signed DB and preimage, not the package bytes.
        store.put(receipt_key, catalog.encode(receipt), 'application/json', 'no-cache')
    if (receipt['source'], receipt['build']) != (source, build):
        raise ValueError('Linux receipt identity mismatch')
    before = {name: restore(value) for name, value in receipt['before'].items()}
    after = {name: restore(value) for name, value in receipt['after'].items()}
    if set(before) != set(FEEDS) or set(after) != set(FEEDS):
        raise ValueError('Invalid Linux receipt feed set')
    for name in FEEDS:
        if current[name] not in (before[name], after[name]):
            raise ValueError('Stable changed outside this receipt; refusing recovery/rollback')
    if receipt['state'] == 'rolled_back':
        if not rollback or current != before:
            raise ValueError('Promotion was rolled back; select a new accepted build')
        return receipt
    if receipt['state'] == 'rolling_back' and not rollback:
        raise ValueError('Finish the requested Linux rollback first')
    target = before if rollback else after
    verify_pair(signing, tmp, target)
    # A retry uses the original archived package/signature and the saved signed DB.
    # Rollback can recover even if the candidate archive is no longer available.
    if not rollback:
        manifest, package, signature = candidate(store, source, build)
        if manifest != receipt['manifest'] or json.loads(after['latest.json']) != manifest:
            raise ValueError('Archived identity changed since Linux promotion started')
        path = tmp / manifest['filename']
        path.write_bytes(package)
        Path(str(path) + '.sig').write_bytes(signature)
        signing.verify(path)
        arch.validate_package(path, build)
        if receipt['state'] == 'complete' and current == after:
            return receipt
    store.put(ACTIVE, catalog.encode({'receipt': receipt_key}), 'application/json', 'no-cache')
    receipt['state'] = 'rolling_back' if rollback else 'applying'
    store.put(receipt_key, catalog.encode(receipt), 'application/json', 'no-cache')
    if not rollback:
        for name, data in [(manifest['filename'], package), (manifest['filename'] + '.sig', signature)]:
            key = f'{PREFIX}/{name}'
            existing = store.call('GET', key)
            if existing is not None and existing != data:
                raise ValueError('Conflicting immutable stable payload')
            if existing is None:
                store.put(key, data, 'application/octet-stream', 'public, max-age=31536000, immutable')
            if store.call('GET', key) != data:
                raise ValueError('Linux payload readback mismatch; retry the same inputs')
    # Each R2 object is replaced atomically; DB + detached signature are NOT a transaction.
    # Keep signature checking enabled: pacman fails closed across the replacement window.
    for name in FEEDS:
        key, data = f'{PREFIX}/{name}', target[name]
        if store.call('GET', key) != data:
            if data is None:
                store.call('DELETE', key)
            else:
                store.put(key, data, 'application/json' if name == 'latest.json' else 'application/octet-stream', 'no-cache')
        if store.call('GET', key) != data:
            raise ValueError('Linux publication readback mismatch; retry the same inputs')
    receipt['state'] = 'rolled_back' if rollback else 'complete'
    receipt['finished_at'] = now()
    store.put(receipt_key, catalog.encode(receipt), 'application/json', 'no-cache')
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True)
    parser.add_argument('--build', type=int, required=True)
    parser.add_argument('--rollback', action='store_true', help='Restore only this receipt\'s exact previous Linux feed')
    args = parser.parse_args()
    if not re.fullmatch('[0-9a-f]{40}', args.source):
        parser.error('--source must be a full lowercase SHA')
    arch.package_name(args.build)
    subprocess.run(['git', 'merge-base', '--is-ancestor', args.source, 'origin/main'], check=True)
    with arch.SigningKey() as signing, tempfile.TemporaryDirectory() as tmp:
        receipt = execute(arch.R2(), signing, Path(tmp), args.source, args.build, args.rollback)
    print(json.dumps(receipt, sort_keys=True))


if __name__ == '__main__':
    main()
