"""Stable consumes original signed AppImage bytes; other platforms remain untouched."""
import base64
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

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
        self.client = Mock()
        stub = patch.object(publication.release_notes, 'merged_items', return_value=[])
        stub.start(); self.addCleanup(stub.stop)
        publication.publish(self.store, self.key, self.root, self.input, SOURCE, receipt, 9999, client=self.client)
        for key in ['okilum/appcast.xml', 'okilum/windows/stable/releases.stable.json',
                    'okilum/arch/stable/x86_64/latest.json', 'okilum/releases/stable.json']:
            self.store.objects[key] = b'preserve exact current bytes'
        self.store.writes.clear()

    def promote(self):
        stable.execute(self.store, self.key, self.root, SOURCE, 5023, client=self.client)

    def test_signed_stable_reuses_payload_and_original_identity_other_feeds_unchanged(self):
        before = dict(self.store.objects); self.promote()
        self.assertTrue(all(key.startswith('okilum/linux/stable/') or key.startswith('okilum/linux/promotions/') or key.startswith('okilum/releases/')
                            for key, _ in self.store.writes))
        for key, value in before.items(): self.assertEqual(self.store.objects[key], value, key)
        raw = self.store.objects['okilum/linux/stable/latest.json']; d = json.loads(raw)
        self.assertEqual((d['channel'], d['source'], d['build']), ('stable', SOURCE, 5023))
        self.assertEqual(self.store.objects['okilum/linux/stable/' + stable.IMAGE], before['okilum/linux/builds/5023/' + stable.IMAGE])
        path = self.root / 'public.json'; path.write_bytes(raw)
        Path(str(path) + '.sig').write_bytes(self.store.objects['okilum/linux/stable/latest.json.sig']); self.key.verify(path)
        with patch.object(self.key, 'sign', side_effect=AssertionError('retry must retain signed bytes')):
            self.promote()

    def test_stable_notes_are_independent_and_written_before_feed(self):
        self.promote()
        path = publication.release_notes.key(SOURCE, 'linux-appimage', 'stable')
        notes = json.loads(self.store.objects[path])
        self.assertEqual((notes['platform'], notes['channel'], notes['build'], notes['previous_build']),
                         ('linux-appimage', 'stable', 5023, None))
        names = [key for key, _ in self.store.writes]
        self.assertLess(names.index(path), names.index('okilum/linux/stable/latest.json'))
        self.promote()
        self.assertEqual(json.loads(self.store.objects[path]), notes)

    def test_stable_notes_failure_cannot_move_feed(self):
        path = publication.release_notes.key(SOURCE, 'linux-appimage', 'stable')
        self.store.fail = path
        with self.assertRaises(RuntimeError):
            self.promote()
        self.assertNotIn('okilum/linux/stable/latest.json', self.store.objects)
        self.store.fail = None
        self.promote()
        self.assertIn(path, self.store.objects)

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
                    'channel': 'stable', 'build': 5022, 'version': '0.1.5022', 'source': 'b' * 40}
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
             patch.object(pacman_final.linux.arch, 'validate_package'), \
             patch.object(pacman_final, 'stable_target'), \
             patch.object(pacman_final.linux, 'execute') as promote:
            run.return_value.stdout = b'missing notice'
            with self.assertRaisesRegex(ValueError, 'notice'):
                pacman_final.execute(self.store, self.key, self.root, SOURCE, 9000)
            promote.assert_not_called(); self.assertNotIn(pacman_final.MARKER, self.store.objects)
            run.return_value.stdout = pacman_final.NOTICE + b' https://updates.befeast.com/okilum/linux/stable/Okilum-x86_64.AppImage'
            self.store.objects.pop('okilum/arch/stable/x86_64/latest.json')
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

    def final_package(self):
        self.store.objects.pop('okilum/arch/stable/x86_64/latest.json', None)
        return ({'filename': 'okilum-0.1.9000-1-x86_64.pkg.tar.zst', 'build': 9000,
                 'source': SOURCE, 'sha256': 'fixture'}, b'package', b'signature')

    def test_final_pacman_missing_stable_target_never_promotes_or_freezes(self):
        with patch.object(pacman_final.linux, 'candidate', return_value=self.final_package()), \
             patch.object(pacman_final.linux.arch, 'validate_package'), \
             patch.object(self.key, 'verify'), \
             patch.object(pacman_final.subprocess, 'run') as run, \
             patch.object(pacman_final.linux, 'execute') as promote:
            run.return_value.stdout = pacman_final.NOTICE + b' ' + pacman_final.IMAGE_URL
            with self.assertRaisesRegex(ValueError, 'Missing signed AppImage'):
                pacman_final.execute(self.store, self.key, self.root, SOURCE, 9000)
            promote.assert_not_called()
            self.assertEqual(self.store.writes, [])

    def test_final_pacman_stable_target_signature_payload_and_provenance_fail_before_writes(self):
        self.promote()
        self.store.writes.clear()
        for name in ['latest.json', 'latest.json.sig', stable.IMAGE, stable.IMAGE + '.sig']:
            key = 'okilum/linux/stable/' + name
            original = self.store.objects[key]
            self.store.objects[key] = original + b'tampered'
            with self.subTest(name=name), self.assertRaises(ValueError):
                pacman_final.stable_target(self.store, self.key, self.root)
            self.assertEqual(self.store.writes, [])
            self.store.objects[key] = original
        key = 'okilum/linux/builds/5023/provenance.json'
        original = self.store.objects.pop(key)
        with self.assertRaisesRegex(ValueError, 'Missing signed AppImage'):
            pacman_final.stable_target(self.store, self.key, self.root)
        self.store.objects[key] = original
        result = pacman_final.stable_target(self.store, self.key, self.root)
        self.assertEqual((result['build'], result['source']), (5023, SOURCE))
        self.assertEqual(self.store.writes, [])

    def test_final_pacman_check_only_uses_no_private_key_signing_or_publication(self):
        self.promote()
        self.store.writes.clear()
        with patch.object(pacman_final, 'PublicStore', return_value=self.store), \
             patch.object(pacman_final, 'PublicKey') as public_key, \
             patch.object(pacman_final.linux.arch, 'SigningKey', side_effect=AssertionError('private key')), \
             patch.object(pacman_final.linux.arch, 'R2', side_effect=AssertionError('publication credentials')), \
             patch.object(pacman_final.linux, 'candidate', return_value=self.final_package()), \
             patch.object(pacman_final.linux.arch, 'validate_package'), \
             patch.object(pacman_final.subprocess, 'run') as run, \
             patch.object(self.key, 'sign', side_effect=AssertionError('signing')), \
             patch.object(pacman_final.linux, 'execute', side_effect=AssertionError('promotion')):
            public_key.return_value.__enter__.return_value = self.key
            run.return_value.stdout = pacman_final.NOTICE + b' ' + pacman_final.IMAGE_URL
            # Only the synthetic pacman signature is outside the test key's format.
            verify = self.key.verify
            with patch.object(self.key, 'verify', side_effect=lambda path:
                              None if str(path).endswith('.pkg.tar.zst') else verify(path)):
                pacman_final.main(['--source', SOURCE, '--build', '9000', '--check-only'])
            self.assertEqual(self.store.writes, [])
            self.assertNotIn(pacman_final.MARKER, self.store.objects)

    def test_public_preflight_store_refuses_mutation_and_path_escape(self):
        store = pacman_final.PublicStore()
        for method, key in [('PUT', 'okilum/arch/frozen.json'), ('DELETE', 'okilum/arch/frozen.json'),
                            ('GET', 'okilum/../private'), ('GET', 'https://example.invalid/')]:
            with self.subTest(method=method, key=key), self.assertRaises(ValueError):
                store.call(method, key)
        with self.assertRaises(ValueError):
            store.put('okilum/arch/frozen.json', b'no', 'application/json')
