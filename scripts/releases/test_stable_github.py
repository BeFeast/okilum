"""Independent Stable promotions cannot leave GitHub Latest on an old bundle."""
import copy
import hashlib
import json
from pathlib import Path
import unittest
from unittest.mock import patch

import catalog
import stable_github as s
from test_publish import Store


def fixture():
    store = Store()
    for platform, build, source in [('macos', 18977, 'a' * 40), ('windows', 19179, 'b' * 40),
                                    ('linux-appimage', 7198, 'c' * 40)]:
        head = dict(build=build, source=source)
        items = [dict(pr=build, kind='fix', text=f'Existing {platform} item <&>')]
        store.data[s.release_notes.key(source, platform, 'stable')] = catalog.encode(
            dict(head, platform=platform, channel='stable', previous_build=None, previous_source=None, items=items))
        if platform == 'macos':
            store.data['okilum/appcast.xml'] = f'<rss xmlns:s="{s.release_notes.SPARKLE[1:-1]}" xmlns:o="{s.release_notes.OKILUM[1:-1]}"><channel><item><s:channel>stable</s:channel><s:version>{build}</s:version><o:source>{source}</o:source></item></channel></rss>'.encode()
            names = ['Okilum-macos.zip', f'Okilum-0.1.{build}.dmg']
        elif platform == 'windows':
            store.data['okilum/windows/stable/releases.stable.json'] = catalog.encode({'Assets': [{'Version': f'0.1.{build}'}]})
            store.data[f'okilum/windows/builds/{build}/release.json'] = catalog.encode(dict(head, signed=True))
            names = ['Setup.exe', 'Okilum-windows-portable.zip']
        else:
            names = ['Okilum-x86_64.AppImage']
        assets = []
        for name in names:
            data = (name + str(build)).encode(); key = f'okilum/archive/{name}'
            store.data[key] = data; assets.append(catalog.asset(key, name, data))
        if platform == 'linux-appimage':
            store.data['okilum/linux/stable/latest.json'] = catalog.encode(dict(
                head, platform=platform, channel='stable', filename=names[0],
                fingerprint='7FFE9F27ECC8E253E45BAEA08AACBFB2C9E47882', **{k: assets[0][k] for k in ('key', 'sha256', 'size')}))
        else:
            store.data[f'{catalog.PREFIX}/{source}/{platform}.json'] = catalog.encode(dict(head, platform=platform, assets=assets))
    return store


class GitHub:
    def __init__(self):
        self.release = None; self.writes = []; self.payloads = {}; self.latest = {'tag_name': 'v0.1.13890'}

    def call(self, method, path, body=None, binary=False):
        if method != 'GET': self.writes.append((method, path, body))
        if path.startswith('/git/ref/tags/'): return None
        if path.startswith('/commits/'): return {'sha': path.rsplit('/', 1)[-1]}
        if path.startswith('/releases?'): return [self.release] if self.release else []
        if path == '/releases/latest': return self.latest
        if path == '/releases' and method == 'POST':
            self.release = dict(body, id=7, assets=[]); return self.release
        if path == '/releases/7' and method == 'PATCH':
            self.release.update(body)
            if body.get('make_latest') == 'true': self.latest = copy.deepcopy(self.release)
            return self.release
        if path.startswith('/releases/7/assets?'):
            from urllib.parse import unquote
            name = unquote(path.split('name=', 1)[1]); self.payloads[name] = body
            self.release['assets'].append({'id': len(self.payloads), 'name': name, 'size': len(body), 'digest': 'sha256:' + hashlib.sha256(body).hexdigest()}); return {}
        if path.startswith('/releases/tags/'): return self.release
        raise AssertionError((method, path))


class StableMirror(unittest.TestCase):
    def test_actual_publisher_replaces_old_latest_with_all_current_platforms_and_notes(self):
        store, github = fixture(), GitHub(); before = dict(store.data)
        receipt = s.execute(store, github)
        self.assertEqual(github.latest['tag_name'], 'stable-m18977-w19179-l7198')
        self.assertFalse(github.latest['prerelease']); self.assertFalse(github.latest['draft'])
        for platform, head in receipt['heads'].items():
            self.assertIn(str(head['build']), github.latest['name'])
            self.assertIn('Existing ' + platform + ' item <&>', github.latest['body'])
            self.assertIn(head['source'], github.latest['body'])
        for line in github.payloads['SHA256SUMS'].decode().splitlines():
            digest, name = line.split('  '); self.assertEqual(digest, hashlib.sha256(github.payloads[name]).hexdigest())
        self.assertIn('Setup.exe', github.payloads); self.assertIn('Okilum-x86_64.AppImage', github.payloads)
        self.assertEqual(store.data, before); self.assertEqual(store.writes, [])

    def test_corruption_missing_notes_wrong_identity_and_unsigned_never_write_github(self):
        for defect in ('corrupt', 'notes', 'identity', 'unsigned'):
            store, github = fixture(), GitHub()
            if defect == 'corrupt': store.data['okilum/archive/Setup.exe'] += b'corrupt'
            elif defect == 'notes': del store.data[s.release_notes.key('b' * 40, 'windows', 'stable')]
            elif defect == 'identity':
                key = f"{catalog.PREFIX}/{'b' * 40}/windows.json"
                d = json.loads(store.data[key]); d['build'] += 1; store.data[key] = catalog.encode(d)
            else:
                key = 'okilum/windows/builds/19179/release.json'
                d = json.loads(store.data[key]); d['signed'] = False; store.data[key] = catalog.encode(d)
            with self.subTest(defect=defect), self.assertRaises(ValueError): s.execute(store, github)
            self.assertEqual(github.writes, [])

    def test_changed_head_during_download_never_writes(self):
        store, github = fixture(), GitHub(); original = s.snapshot; count = 0
        def snap(store):
            nonlocal count
            value = original(store); count += 1
            if count == 2: value['heads']['windows']['build'] += 1
            return value
        with patch.object(s, 'snapshot', side_effect=snap), self.assertRaisesRegex(ValueError, 'heads changed'):
            s.execute(store, github)
        self.assertEqual(github.writes, [])

    def test_public_get_has_explicit_cdn_headers_and_does_not_write(self):
        with patch.object(s.urllib.request, 'urlopen') as opener:
            opener.return_value.__enter__.return_value.read.return_value = b'feed'
            self.assertEqual(s.PublicStore().call('GET', 'okilum/appcast.xml'), b'feed')
            request = opener.call_args.args[0]
            self.assertEqual(request.get_header('User-agent'), 'curl/8')
            self.assertEqual(request.get_header('Cache-control'), 'no-cache')
        with self.assertRaises(ValueError): s.PublicStore().call('PUT', 'okilum/appcast.xml')

    def test_changelog_is_exactly_generated_from_audited_existing_text(self):
        history = json.loads(s.HISTORY.read_text())
        self.assertEqual((s.HISTORY.parents[2] / 'CHANGELOG.md').read_text(), s.changelog(history))
        for row in history['releases']:
            self.assertRegex(row['evidence']['sha256'], r'^[0-9a-f]{64}$')
            for item in row['notes']['items']: self.assertIn(item['text'], s.changelog(history))

    def test_workflow_reconciles_after_split_promoters_only_on_success_and_never_on_beta_tick(self):
        text = (Path(__file__).resolve().parents[2] / '.forgejo/workflows/releases.yml').read_text()
        reconcile = text.split('      - name: Reconcile GitHub Latest', 1)[1]
        self.assertIn("if: ${{ success() && (inputs.build != '' || inputs.sync_stable) && !inputs.pacman_final }}", reconcile)
        self.assertIn('scripts/releases/stable_github.py --receipt', reconcile)
        promotion = text.split('      - name: Reconcile GitHub Latest', 1)[0]
        for route in ('publish.py --build "$PROMOTE_BUILD" --mac-only', 'windows_stable.py', 'appimage_stable.py'):
            self.assertIn(route, promotion)
        self.assertIn("(inputs.build != '' || inputs.sync_stable) && 'okilum-macos-release'", text)
