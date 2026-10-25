#!/usr/bin/env python3
"""Promote an existing signed AppImage to Stable without rebuilding or other OS writes."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

import appimage_publish as publication

PREFIX = publication.PREFIX
IMAGE = publication.artifact.IMAGE


def required(store, key):
    value = store.call('GET', key)
    if value is None:
        raise ValueError('Missing signed AppImage object: ' + key)
    return value


def verify(store, signing, tmp, prefix, name):
    data = required(store, f'{prefix}/{name}')
    path = tmp / name
    path.write_bytes(data)
    Path(str(path) + '.sig').write_bytes(required(store, f'{prefix}/{name}.sig'))
    signing.verify(path)
    return data


def execute(store, signing, tmp, source, build):
    if not re.fullmatch('[0-9a-f]{40}', source) or build <= 5000:
        raise ValueError('Expected full source SHA and original AppImage build')
    if signing.fingerprint != publication.FINGERPRINT:
        raise ValueError('AppImage requires the pinned Linux signing authority')
    archive = f'{PREFIX}/builds/{build}'
    raw = verify(store, signing, tmp, archive, 'release.json')
    release = json.loads(raw)
    if (release.get('schema'), release.get('platform'), release.get('channel'), release.get('source'), release.get('build'),
        release.get('version'), release.get('filename'), release.get('key'), release.get('fingerprint')) != (
            1, 'linux-appimage', 'beta', source, build, f'0.1.{build}', IMAGE, f'{archive}/{IMAGE}', publication.FINGERPRINT):
        raise ValueError('Signed AppImage descriptor identity mismatch')
    image = verify(store, signing, tmp, archive, IMAGE)
    provenance_raw = required(store, f'{archive}/provenance.json')
    provenance = json.loads(provenance_raw)
    if (len(image) != release['size'] or hashlib.sha256(image).hexdigest() != release['sha256']
            or hashlib.sha256(provenance_raw).hexdigest() != release['provenance_sha256']
            or provenance.get('source_sha') != source or provenance.get('build_version') != str(build)
            or provenance.get('release_version') != f'0.1.{build}' or provenance.get('sha256') != release['sha256']
            or provenance.get('acceptance_harness', False) is not False):
        raise ValueError('AppImage payload/provenance mismatch or acceptance-only payload')
    stable = publication.encode({**release, 'channel': 'stable'})
    image_signature = Path(str(tmp / IMAGE) + '.sig').read_bytes()
    receipt_key = f'{PREFIX}/promotions/{build}-{source}.json'
    prior_raw = store.call('GET', receipt_key)
    prior = json.loads(prior_raw) if prior_raw else None
    if prior is not None and (not isinstance(prior, dict) or prior.get('descriptor') != base64.b64encode(stable).decode()):
        raise ValueError('AppImage promotion identity changed during retry')
    current = store.call('GET', f'{PREFIX}/stable/latest.json')
    current_signature = store.call('GET', f'{PREFIX}/stable/latest.json.sig')
    if current:
        old_path = tmp / 'current.json'; old_path.write_bytes(current)
        signature_to_check = current_signature
        if prior and prior.get('before') == base64.b64encode(current).decode() and current_signature == base64.b64decode(prior['signature'], validate=True):
            # Interrupted detached-pair publication: only the pinned preimage
            # and exact prepared signature may cross. Validate the old signed
            # descriptor using the preimage signature retained before writes.
            signature_to_check = base64.b64decode(prior['before_signature'], validate=True)
        if signature_to_check is None:
            raise ValueError('Missing current Stable signature')
        Path(str(old_path) + '.sig').write_bytes(signature_to_check)
        signing.verify(old_path)
        old = json.loads(current)
        if old['build'] > build or (old['build'] == build and current != stable):
            raise ValueError('Refusing AppImage Stable rollback or conflicting identity')
    if prior:
        signature = base64.b64decode(prior['signature'], validate=True)
    else:
        path = tmp / 'stable.json'; path.write_bytes(stable); signing.sign(path)
        signature = Path(str(path) + '.sig').read_bytes()
    path = tmp / 'stable.json'; path.write_bytes(stable); Path(str(path) + '.sig').write_bytes(signature); signing.verify(path)
    receipt = {'source': source, 'build': build, 'descriptor': base64.b64encode(stable).decode(),
               'signature': base64.b64encode(signature).decode(), 'state': 'prepared',
               'before': prior.get('before') if prior else (base64.b64encode(current).decode() if current else None),
               'before_signature': prior.get('before_signature') if prior else (base64.b64encode(current_signature).decode() if current_signature else None)}
    store.put(receipt_key, publication.encode(receipt), 'application/json', 'no-cache')
    # Reuse the exact signed executable; never relabel the embedded version.
    for name, data in [(IMAGE, image), (IMAGE + '.sig', image_signature),
                       ('latest.json.sig', signature), ('latest.json', stable)]:
        store.put(f'{PREFIX}/stable/{name}', data,
                  'application/json' if name.endswith('.json') else 'application/octet-stream', 'no-cache')
    receipt['state'] = 'complete'
    store.put(receipt_key, publication.encode(receipt), 'application/json', 'no-cache')
    print(f'Published AppImage Stable only: {build} source {source}')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True)
    parser.add_argument('--build', required=True, type=int)
    args = parser.parse_args(argv)
    if not re.fullmatch('[0-9a-f]{40}', args.source) or args.build <= 5000:
        parser.error('Expected full source SHA and original AppImage build')
    subprocess.run(['git', 'merge-base', '--is-ancestor', args.source, 'origin/main'], check=True)
    with tempfile.TemporaryDirectory() as directory, publication.arch.SigningKey() as signing:
        execute(publication.arch.R2(), signing, Path(directory), args.source, args.build)


if __name__ == '__main__':
    main()
