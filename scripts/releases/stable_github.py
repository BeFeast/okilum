#!/usr/bin/env python3
"""Mirror the actual independent Stable heads to one GitHub Latest release."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import urllib.error
import urllib.request

import catalog
import publish
import release_notes

PUBLIC = 'https://updates.befeast.com/'
HISTORY = Path(__file__).with_name('stable-history.json')


class PublicStore:
    def call(self, method, key):
        if method != 'GET' or not key.startswith('okilum/') or '..' in key.split('/'):
            raise ValueError('Public Stable mirror is read-only')
        # The public CDN rejects Python's default User-Agent with HTTP 403.
        request = urllib.request.Request(PUBLIC + key, headers={
            'User-Agent': 'curl/8', 'Cache-Control': 'no-cache'})
        try:
            with urllib.request.urlopen(request, timeout=300) as response:
                return response.read()
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return None
            raise RuntimeError(f'Stable public GET: HTTP {error.code}') from None


def required(store, key):
    raw = store.call('GET', key)
    if raw is None:
        raise ValueError('Missing Stable object: ' + key)
    return raw


def canonical_notes(store, platform, head):
    key = release_notes.key(head['source'], platform, 'stable')
    raw = store.call('GET', key)
    if raw is not None:
        value = json.loads(raw)
    else:
        # Historical split promoters omitted sidecars. Only audited existing
        # release text in the committed backfill may fill those exact identities.
        matches = [v['notes'] for v in json.loads(HISTORY.read_text())['releases']
                   if (v['notes']['platform'], v['notes']['source'], v['notes']['build']) ==
                   (platform, head['source'], head['build'])]
        if len(matches) != 1:
            raise ValueError('Missing canonical Stable notes: ' + key)
        value = matches[0]
    if any(value.get(k) != v for k, v in dict(platform=platform, channel='stable', **head).items()):
        raise ValueError('Stable notes identity mismatch')
    for item in value['items']:
        if type(item['pr']) is not int or item['pr'] <= 0 or not isinstance(item['text'], str):
            raise ValueError('Invalid Stable note item')
    return value


def snapshot(store):
    heads = {p: release_notes.predecessor(store, p, 'stable')
             for p in ('macos', 'windows', 'linux-appimage')}
    if any(v is None for v in heads.values()):
        raise ValueError('All three Stable heads are required')
    platforms, notes = {}, {}
    for platform, head in heads.items():
        notes[platform] = canonical_notes(store, platform, head)
        if platform == 'linux-appimage':
            descriptor = json.loads(required(store, 'okilum/linux/stable/latest.json'))
            if (descriptor.get('platform'), descriptor.get('channel'), descriptor.get('fingerprint')) != (
                    platform, 'stable', '7FFE9F27ECC8E253E45BAEA08AACBFB2C9E47882'):
                raise ValueError('Unexpected Stable AppImage authority')
            assets = [{k: descriptor[k] for k in ('key', 'sha256', 'size')} | {'name': descriptor['filename']}]
        else:
            descriptor = json.loads(required(store, f"{catalog.PREFIX}/{head['source']}/{platform}.json"))
            if descriptor.get('platform') != platform:
                raise ValueError('Stable catalog platform mismatch')
            assets = descriptor['assets']
            if platform == 'windows':
                metadata = json.loads(required(store, f"okilum/windows/builds/{head['build']}/release.json"))
                if (metadata.get('signed') is not True or metadata.get('build') != head['build']
                        or metadata.get('source') != head['source']):
                    raise ValueError('Stable Windows must retain its signed identity')
        if any(descriptor.get(k) != v for k, v in head.items()):
            raise ValueError('Stable feed/catalog identity mismatch')
        platforms[platform] = dict(head, assets=assets)
    return {'heads': heads, 'platforms': platforms, 'notes': notes}


def body(value):
    lines = ['Current Stable builds (platform build numbers are independent).', '']
    for platform, head in value['heads'].items():
        lines += [f"## {platform} — 0.1.{head['build']}", '', f"Source: `{head['source']}`", '']
        items = value['notes'][platform]['items']
        lines += [f"- {i['text']} ([#{i['pr']}](https://git.oklabs.uk/BeFeast/okilum/pulls/{i['pr']}))"
                  for i in items] or ['- ' + release_notes.MAINTENANCE]
        lines += ['']
    return '\n'.join(lines)


def execute(store, github):
    value = snapshot(store)
    heads = value['heads']
    tag = 'stable-m{}-w{}-l{}'.format(*(heads[p]['build'] for p in ('macos', 'windows', 'linux-appimage')))
    files = {}
    for platform in value['platforms'].values():
        for asset in platform['assets']:
            name = asset['name']
            if name in files or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', name):
                raise ValueError('Duplicate or unsafe Stable asset')
            raw = required(store, asset['key'])
            if len(raw) != asset['size'] or hashlib.sha256(raw).hexdigest() != asset['sha256']:
                raise ValueError('Stable asset checksum/size mismatch: ' + name)
            files[name] = raw
    # A changing feed must never produce a mixed publication. No writes yet.
    if snapshot(store) != value:
        raise ValueError('Stable heads changed while preparing GitHub mirror')
    files['SHA256SUMS'] = ''.join(f'{hashlib.sha256(raw).hexdigest()}  {name}\n'
                                for name, raw in sorted(files.items())).encode()
    receipt = {'tag': tag, 'heads': heads, 'assets': {n: {'size': len(v), 'sha256': hashlib.sha256(v).hexdigest()}
                                                   for n, v in files.items()}}
    files['stable-receipt.json'] = catalog.encode(receipt)
    source = heads['macos']['source']
    if github.call('GET', '/commits/' + source) is None:
        raise ValueError('Stable source is not mirrored to GitHub')
    existing_tag = github.call('GET', '/git/ref/tags/' + tag)
    if existing_tag is not None and existing_tag.get('object', {}).get('sha') != source:
        raise ValueError('Stable matrix tag already names another source')
    title = 'Stable — ' + ', '.join(f"{p} 0.1.{h['build']}" for p, h in heads.items())
    release = {'build': heads['macos']['build'], 'source': source, 'name': title}
    publish.github_release(github, tag, release, files, body(value), True)
    latest = github.call('GET', '/releases/latest')
    if latest is None or latest['tag_name'] != tag or latest.get('body') != body(value):
        raise ValueError('GitHub Latest readback mismatch')
    uploaded = {a['name']: a for a in latest.get('assets', [])}
    for name, raw in files.items():
        asset = uploaded.get(name, {})
        if asset.get('size') != len(raw) or asset.get('digest') != 'sha256:' + hashlib.sha256(raw).hexdigest():
            raise ValueError('GitHub Stable asset readback mismatch: ' + name)
    return receipt


def changelog(history):
    lines = ['# Changelog', '',
             'Stable releases use independent build numbers for each platform. '
             'Entries below retain existing published release text.', '',
             history['coverage'], '']
    for row in history['releases']:
        n = row['notes']
        lines += [f"## {n['platform']} 0.1.{n['build']} — Stable", '',
                  f"Source: `{n['source']}`. [Release data]({row['evidence']['url']}).", '']
        lines += [f"- {i['text']} ([#{i['pr']}](https://git.oklabs.uk/BeFeast/okilum/pulls/{i['pr']}))"
                  for i in n['items']] or ['No user-visible items were recorded in the existing build notes.']
        lines += ['']
    baseline = history['baseline']
    lines += ['## 0.1.13890 — published cross-platform release (2026-10-18)', '',
              f"[Original release]({baseline['url']})", '', baseline['body'], '']
    return '\n'.join(lines)


def refresh_history(store, history):
    value = snapshot(store)
    records = history['releases']
    for platform, head in value['heads'].items():
        if any((r['notes']['platform'], r['notes']['build'], r['notes']['source']) ==
               (platform, head['build'], head['source']) for r in records):
            continue
        key = release_notes.key(head['source'], platform, 'stable')
        raw = required(store, key)
        records.insert(0, {'notes': value['notes'][platform],
                          'evidence': {'url': PUBLIC + key, 'sha256': hashlib.sha256(raw).hexdigest()}})
    return history


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--receipt', type=Path)
    parser.add_argument('--changelog', action='store_true', help='Refresh local history/CHANGELOG for review; no GitHub writes')
    args = parser.parse_args()
    if args.changelog:
        history = refresh_history(PublicStore(), json.loads(HISTORY.read_text()))
        HISTORY.write_text(json.dumps(history, ensure_ascii=False, indent=2) + '\n')
        (HISTORY.parents[2] / 'CHANGELOG.md').write_text(changelog(history))
        return
    receipt = execute(PublicStore(), publish.GitHub())
    if args.receipt:
        args.receipt.write_bytes(catalog.encode(receipt))
    print(json.dumps(receipt, sort_keys=True))


if __name__ == '__main__':
    main()
