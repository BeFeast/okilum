"""Stable notes follow the existing full/split promotion gates, never a beta head."""
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import xml.etree.ElementTree as ET

import publish as promotion
import release_notes as notes
from test_publish import Store, fixture, SOURCE
from test_release_notes import feed

ITEMS = [{'pr': 1, 'kind': 'feature', 'text': 'Changes since Stable'}]
OLD = 'b' * 40


class WindowsStable(unittest.TestCase):
    def candidate(self, store):
        fixture(store)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            package = b'original full package'
            name = 'BeFeast.Okilum-0.1.705-beta-full.nupkg'
            (root / name).write_bytes(package)
            (root / 'releases.beta.json').write_text(json.dumps({'Assets': [{
                'FileName': name, 'PackageId': 'BeFeast.Okilum', 'Type': 'Full',
                'Version': '0.1.705', 'Size': len(package),
                'SHA256': promotion.windows.hashlib.sha256(package).hexdigest()}]}))
            (root / 'BeFeast.Okilum-beta-Setup.exe').write_bytes(b'MZsigned setup')
            (root / 'signing.json').write_text(json.dumps({'signed': True, 'certificate_sha256': 'd' * 64}))
            portable = root / 'portable.zip'; portable.write_bytes(b'PKoriginal portable')
            promotion.windows.publish(root, 705, SOURCE, store, portable)
        current = promotion.choose(store, 700)
        old_windows = {'platform': 'windows', 'build': 650, 'source': OLD, 'assets': []}
        previous = {**current, 'platforms': {**current['platforms'], 'windows': old_windows}}
        store.data['okilum/releases/stable.json'] = json.dumps(previous).encode()
        store.data['okilum/windows/stable/releases.stable.json'] = b'{"Assets":[{"Version":"0.1.650"}]}'
        store.data['okilum/windows/builds/650/release.json'] = json.dumps({'build': 650, 'source': OLD}).encode()
        store.writes.clear()

    def test_real_signed_only_guard_precedes_notes_and_windows_only_preserves_mac_linux(self):
        store = Store(); self.candidate(store)
        key = 'okilum/windows/builds/705/release.json'
        metadata = json.loads(store.data[key]); metadata['signed'] = False
        store.data[key] = json.dumps(metadata).encode()
        before = dict(store.data)
        with patch.dict(os.environ, OKILUM_WINDOWS_STABLE_REQUIRE_SIGNED='1'), \
                patch.object(notes, 'merged_items', return_value=ITEMS) as select:
            with self.assertRaisesRegex(ValueError, 'not signed'):
                promotion.complete_windows(store, None, 700, Mock())
            self.assertEqual(store.data, before)
            self.assertEqual(store.writes, [])
            select.assert_not_called()
            # Positive control: SAME fixture with signed metadata passes the real
            # archived feed/hash/signature gate and emits only Windows notes.
            metadata['signed'] = True; store.data[key] = json.dumps(metadata).encode()
            github = Mock(); github.call.return_value = {'id': 9, 'body': 'Original release body'}
            with patch.object(promotion, 'github_release') as publish:
                promotion.complete_windows(store, github, 700, Mock())
            result = json.loads(store.data[notes.key(SOURCE, 'windows', 'stable')])
            self.assertEqual((result['build'], result['previous_build'], result['previous_source']), (705, 650, OLD))
            self.assertEqual(result['items'], ITEMS)
            select.assert_called_once_with(unittest.mock.ANY, OLD, SOURCE)
            self.assertEqual(publish.call_args.args[4], 'Original release body')
        self.assertFalse(any('/macos/' in key or '/linux/' in key or '/arch/' in key for key in store.writes))
        self.assertNotIn('okilum/appcast.xml', store.writes)
        self.assertFalse(any('/beta/' in key for key in store.writes))


class StableRetry(unittest.TestCase):
    def test_retry_after_feeds_moved_keeps_original_stable_range(self):
        store = Store(); fixture(store)
        # Each platform previously published a different Stable source. Beta is
        # already SOURCE; using it would silently omit the cumulative changes.
        store.data['okilum/appcast.xml'] = feed((600, OLD, 'stable'), (700, SOURCE, 'beta'))
        store.data['okilum/arch/stable/x86_64/latest.json'] = json.dumps({'build': 610, 'source': 'c' * 40}).encode()
        store.data['okilum/windows/stable/releases.stable.json'] = b'{"Assets":[{"Version":"0.1.620"}]}'
        store.data['okilum/windows/builds/620/release.json'] = json.dumps({'build': 620, 'source': 'd' * 40}).encode()
        def macos(args):
            tree = ET.ElementTree(ET.fromstring(store.data['okilum/appcast.xml']))
            promotion.appcast.promote(tree, SimpleNamespace(build=args.build, notes_json=json.dumps(args.release_notes)))
            store.put('okilum/appcast.xml', ET.tostring(tree.getroot()))
        def linux(*args):
            store.put('okilum/arch/stable/x86_64/latest.json', json.dumps({'build': 702, 'source': SOURCE}).encode())
        def windows(*args):
            store.put('okilum/windows/stable/releases.stable.json', b'{"Assets":[{"Version":"0.1.701"}]}')
        with patch.object(promotion, 'notes', return_value='Original body'), \
                patch.object(promotion, 'preflight_stable'), \
                patch.object(promotion.arch, 'SigningKey'), \
                patch.object(promotion.arch, 'validate_package'), \
                patch.object(promotion.arch, 'execute', side_effect=linux), \
                patch.object(promotion.windows, 'promote', side_effect=windows), \
                patch.object(promotion, 'promote_macos', side_effect=macos), \
                patch.object(promotion, 'mirror_tag', return_value='v0.1.700'), \
                patch.object(notes, 'merged_items', return_value=ITEMS) as select, \
                patch.object(promotion, 'github_release', side_effect=[OSError('upload interrupted'), None]):
            with self.assertRaises(OSError):
                promotion.execute(store, None, Mock(), 700)
            saved = {platform: store.data[notes.key(SOURCE, platform, 'stable')]
                     for platform in ['macos', 'linux', 'windows']}
            self.assertNotIn('okilum/releases/stable.json', store.data)
            promotion.execute(store, None, Mock(), 700)
            self.assertEqual(select.call_count, 3, 'retry must not recompute against moved feeds')
        expected = {'macos': (600, OLD), 'linux': (610, 'c' * 40), 'windows': (620, 'd' * 40)}
        for platform, previous in expected.items():
            raw = store.data[notes.key(SOURCE, platform, 'stable')]
            self.assertEqual(raw, saved[platform])
            payload = json.loads(raw)
            self.assertEqual((payload['previous_build'], payload['previous_source']), previous)
            self.assertEqual(payload['items'], ITEMS)
        self.assertEqual(store.writes[-1], 'okilum/releases/stable.json')
        item = promotion.appcast.find(ET.ElementTree(ET.fromstring(store.data['okilum/appcast.xml'])), 700)
        self.assertIn('Changes since Stable', item.findtext('description'))

    def test_identity_conflict_fails_before_stable_notes_write(self):
        store = Store()
        store.data[notes.key(SOURCE, 'macos', 'stable')] = json.dumps({
            'build': 700, 'source': OLD, 'platform': 'macos', 'channel': 'stable'}).encode()
        with self.assertRaisesRegex(ValueError, 'identity mismatch'):
            notes.prepare_stable(store, Mock(), 'macos', 700, SOURCE)
        self.assertEqual(store.writes, [])
