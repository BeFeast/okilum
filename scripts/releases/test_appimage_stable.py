"""Stable consumes original signed AppImage bytes; other platforms remain untouched."""
import base64
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import appimage_stable as stable
import appimage_publish as publication
import pacman_final
from test_appimage_publication import Store, Key, evidence, SOURCE


class AppImageStableTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name); self.input = self.root / 'input'; self.input.mkdir()
        receipt, _, image, provenance = evidence()
        (self.input / stable.IMAGE).write_bytes(image)
        (self.input / 'provenance.json').write_text(json.dumps(provenance))
        self.store, self.key = Store(), Key()
        publication.publish(self.store, self.key, self.root, self.input, SOURCE, receipt, 9999)
        for key in ['okilum/appcast.xml', 'okilum/windows/stable/releases.stable.json',
                    'okilum/arch/stable/x86_64/latest.json', 'okilum/releases/stable.json']:
            self.store.objects[key] = b'preserve exact current bytes'
        self.store.writes.clear()

    def promote(self):
        stable.execute(self.store, self.key, self.root, SOURCE, 5023)

    def test_signed_stable_reuses_payload_and_original_identity_other_feeds_unchanged(self):
        before = dict(self.store.objects); self.promote()
        self.assertTrue(all(key.startswith('okilum/linux/stable/') or key.startswith('okilum/linux/promotions/')
                            for key, _ in self.store.writes))
        for key, value in before.items(): self.assertEqual(self.store.objects[key], value, key)
        raw = self.store.objects['okilum/linux/stable/latest.json']; d = json.loads(raw)
        self.assertEqual((d['channel'], d['source'], d['build']), ('stable', SOURCE, 5023))
        self.assertEqual(self.store.objects['okilum/linux/stable/' + stable.IMAGE], before['okilum/linux/builds/5023/' + stable.IMAGE])
        path = self.root / 'public.json'; path.write_bytes(raw)
        Path(str(path) + '.sig').write_bytes(self.store.objects['okilum/linux/stable/latest.json.sig']); self.key.verify(path)
        with patch.object(self.key, 'sign', side_effect=AssertionError('retry must retain signed bytes')):
            self.promote()

    def test_corrupt_image_signature_descriptor_or_provenance_has_zero_writes(self):
        archive = 'okilum/linux/builds/5023/'
        for name in [stable.IMAGE, stable.IMAGE + '.sig', 'release.json', 'release.json.sig', 'provenance.json']:
            original = self.store.objects[archive + name]
            self.store.objects[archive + name] = original + b'tampered'
            with self.subTest(name=name), self.assertRaises((ValueError, json.JSONDecodeError)):
                self.promote()
            self.assertEqual(self.store.writes, [])
            self.store.objects[archive + name] = original

    def test_rollback_and_conflicting_same_build_fail_before_writes(self):
        self.promote()
        key = 'okilum/linux/stable/latest.json'; original = self.store.objects[key]
        for changes in [{'build': 5024}, {'source': 'b' * 40}]:
            d = {**json.loads(original), **changes}; data = publication.encode(d)
            path = self.root / 'other.json'; path.write_bytes(data); self.key.sign(path)
            self.store.objects[key] = data
            self.store.objects[key + '.sig'] = Path(str(path) + '.sig').read_bytes(); self.store.writes.clear()
            with self.assertRaisesRegex(ValueError, 'rollback|conflicting'):
                self.promote()
            self.assertEqual(self.store.writes, [])

    def test_partial_feed_write_retries_same_signed_identity(self):
        self.store.fail = 'okilum/linux/stable/latest.json'
        with self.assertRaises(RuntimeError): self.promote()
        self.store.fail = None
        with patch.object(self.key, 'sign', side_effect=AssertionError('retry cannot sign another identity')):
            self.promote()
        receipt = json.loads(self.store.objects[f'okilum/linux/promotions/5023-{SOURCE}.json'])
        self.assertEqual(receipt['state'], 'complete')

    def test_retry_recovers_interrupted_detached_pair_over_previous_stable(self):
        previous = {**json.loads(self.store.objects['okilum/linux/builds/5023/release.json']),
                    'channel': 'stable', 'build': 5022, 'version': '0.1.5022'}
        raw = publication.encode(previous); path = self.root / 'previous.json'
        path.write_bytes(raw); self.key.sign(path)
        self.store.objects['okilum/linux/stable/latest.json'] = raw
        self.store.objects['okilum/linux/stable/latest.json.sig'] = Path(str(path) + '.sig').read_bytes()
        self.store.fail = 'okilum/linux/stable/latest.json'
        with self.assertRaises(RuntimeError): self.promote()
        self.store.fail = None
        self.promote()
        self.assertEqual(json.loads(self.store.objects['okilum/linux/stable/latest.json'])['build'], 5023)

    def test_final_pacman_requires_notice_and_freezes_only_after_success(self):
        manifest = {'filename': 'final.pkg'}
        with patch.object(pacman_final.linux, 'candidate', return_value=(manifest, b'package', b'signature')), \
             patch.object(self.key, 'verify'), patch.object(pacman_final.subprocess, 'run') as run, \
             patch.object(pacman_final.linux, 'execute') as promote:
            run.return_value.stdout = b'missing notice'
            with self.assertRaisesRegex(ValueError, 'notice'):
                pacman_final.execute(self.store, self.key, self.root, SOURCE, 9000)
            promote.assert_not_called(); self.assertNotIn(pacman_final.MARKER, self.store.objects)
            run.return_value.stdout = pacman_final.NOTICE + b' https://updates.befeast.com/okilum/linux/stable/Okilum-x86_64.AppImage'
            promote.side_effect = OSError('publication failed')
            with self.assertRaises(OSError): pacman_final.execute(self.store, self.key, self.root, SOURCE, 9000)
            self.assertNotIn(pacman_final.MARKER, self.store.objects)
            promote.side_effect = None
            pacman_final.execute(self.store, self.key, self.root, SOURCE, 9000)
            self.assertEqual(json.loads(self.store.objects[pacman_final.MARKER])['state'], 'frozen')
            with self.assertRaisesRegex(ValueError, 'already frozen'):
                pacman_final.execute(self.store, self.key, self.root, SOURCE, 9001)

    def test_frozen_marker_blocks_arch_writes(self):
        self.store.objects[pacman_final.MARKER] = b'{"state":"frozen"}'
        with self.assertRaisesRegex(ValueError, 'frozen'):
            publication.arch.execute(None, self.store, self.key, self.root)
        with self.assertRaisesRegex(ValueError, 'frozen'):
            pacman_final.linux.execute(self.store, self.key, self.root, SOURCE, 9000)
        self.assertEqual(self.store.writes, [])
