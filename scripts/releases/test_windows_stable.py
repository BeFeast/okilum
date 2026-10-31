import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import catalog
import windows_stable as w
from test_publish import Store

SOURCE = 'e' * 40
BUILD = 17175


def prepared():
    store = Store()
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        name = f'BeFeast.Okilum-0.1.{BUILD}-beta-full.nupkg'
        data = b'signed package fixture'
        (root / name).write_bytes(data)
        (root / 'releases.beta.json').write_text(json.dumps({'Assets': [{
            'PackageId': 'BeFeast.Okilum', 'Version': f'0.1.{BUILD}', 'Type': 'Full',
            'FileName': name, 'Size': len(data), 'SHA256': hashlib.sha256(data).hexdigest()}]}))
        (root / 'BeFeast.Okilum-beta-Setup.exe').write_bytes(b'MZsigned installer fixture')
        (root / 'signing.json').write_text(json.dumps({'signed': True, 'certificate_sha256': 'f' * 64}))
        portable = root / 'portable.zip'; portable.write_bytes(b'portable fixture')
        w.windows.publish(root, BUILD, SOURCE, store, portable)
    store.data['okilum/appcast.xml'] = b'Mac16684 different source'
    store.data['okilum/arch/stable/x86_64/latest.json'] = b'Linux14238'
    store.data[f'{catalog.PREFIX}/stable.json'] = b'legacy Mac13890 marker'
    store.writes.clear()
    return store


class ExactWindowsTests(unittest.TestCase):
    def setUp(self):
        for target, kwargs in [('windows_stable.Forgejo', {}), ('windows_stable.release_notes.merged_items', {'return_value': []})]:
            stub = patch(target, **kwargs); stub.start(); self.addCleanup(stub.stop)

    def test_signed_exact_windows_ignores_mac_source_and_preserves_other_feeds(self):
        store = prepared(); before = dict(store.data)
        with patch.dict(os.environ, {'OKILUM_WINDOWS_STABLE_REQUIRE_SIGNED': '1'}):
            w.execute(store, SOURCE, BUILD)
        allowed = {w.release_notes.key(SOURCE, 'windows', 'stable'), f'{w.PREFIX}/stable/Setup.exe', f'{w.PREFIX}/stable/releases.stable.json',
                   f'{w.PREFIX}/stable/BeFeast.Okilum-0.1.{BUILD}-beta-full.nupkg',
                   f'{w.PREFIX}/promotions/{BUILD}-{SOURCE}.json'}
        self.assertEqual(set(store.writes), allowed)
        for key, value in before.items():
            if key not in allowed:
                self.assertEqual(store.data[key], value, key)
        feed = json.loads(store.data[f'{w.PREFIX}/stable/releases.stable.json'])
        self.assertEqual(feed['Assets'][0]['Version'], f'0.1.{BUILD}')
        self.assertEqual(store.writes[-2], f'{w.PREFIX}/stable/releases.stable.json')
        self.assertEqual(json.loads(store.data[store.writes[-1]])['state'], 'complete')

    def test_unsigned_mismatch_corruption_and_rollback_fail_before_mutation(self):
        for defect in ('unsigned', 'signer', 'source', 'catalog', 'checksum', 'package', 'rollback'):
            with self.subTest(defect=defect):
                store = prepared(); key = f'{w.PREFIX}/builds/{BUILD}/release.json'
                metadata = json.loads(store.data[key])
                if defect == 'unsigned': metadata['signed'] = False
                if defect == 'signer': metadata['signer_sha256'] = ''
                if defect == 'source': metadata['source'] = 'b' * 40
                store.data[key] = catalog.encode(metadata)
                if defect == 'catalog':
                    key = f'{catalog.PREFIX}/{SOURCE}/windows.json'; d = json.loads(store.data[key])
                    d['build'] += 1; store.data[key] = catalog.encode(d)
                if defect == 'checksum': store.data[f'{w.PREFIX}/builds/{BUILD}/Setup.exe'] += b'corrupt'
                if defect == 'package':
                    name = metadata['feed']['Assets'][0]['FileName']; store.data[f'{w.PREFIX}/beta/{name}'] += b'corrupt'
                if defect == 'rollback':
                    store.data[f'{w.PREFIX}/stable/releases.stable.json'] = catalog.encode({'Assets': [{'Version': '0.1.99999'}]})
                # An unset legacy guard must not weaken independent signed-only promotion.
                with patch.dict(os.environ, {'OKILUM_WINDOWS_STABLE_REQUIRE_SIGNED': ''}):
                    with self.assertRaises(ValueError): w.execute(store, SOURCE, BUILD)
                self.assertEqual(store.writes, [])

    def test_retry_cannot_reselect_different_signed_metadata(self):
        store = prepared(); w.execute(store, SOURCE, BUILD); store.writes.clear()
        key = f'{w.PREFIX}/builds/{BUILD}/release.json'; d = json.loads(store.data[key])
        d['signer_sha256'] = 'a' * 64; store.data[key] = catalog.encode(d)
        with self.assertRaisesRegex(ValueError, 'identity changed'): w.execute(store, SOURCE, BUILD)
        self.assertEqual(store.writes, [])

    def test_malformed_receipt_fails_clearly_before_mutation(self):
        for receipt in ({}, [], {'state': 'prepared'}, {'identity': None}):
            with self.subTest(receipt=receipt):
                store = prepared()
                store.data[f'{w.PREFIX}/promotions/{BUILD}-{SOURCE}.json'] = catalog.encode(receipt)
                with self.assertRaisesRegex(ValueError, 'identity changed'):
                    w.execute(store, SOURCE, BUILD)
                self.assertEqual(store.writes, [])

    def test_non_main_source_never_constructs_store(self):
        with patch.object(w.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, 'git')), \
             patch.object(w.windows, 'R2') as store:
            with self.assertRaises(subprocess.CalledProcessError):
                w.main(['--source', SOURCE, '--build', str(BUILD)])
            store.assert_not_called()

    def test_workflow_routes_build_source_without_legacy_mac_lookup(self):
        text = (Path(__file__).resolve().parents[2] / '.forgejo/workflows/releases.yml').read_text()
        script = '\n'.join(line[10:] for line in text.split('        run: |\n', 1)[1].split('      - name:', 1)[0].splitlines())
        with tempfile.TemporaryDirectory() as directory:
            stub = Path(directory) / 'python'; stub.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n'); stub.chmod(0o755)
            env = {**os.environ, 'PATH': directory + os.pathsep + os.environ['PATH'],
                   'WINDOWS_ONLY': 'true', 'PROMOTE_SOURCE': SOURCE, 'PROMOTE_BUILD': str(BUILD),
                   'MAC_ONLY': 'false', 'LINUX_ONLY': 'false', 'LINUX_ROLLBACK': 'false',
                   'SKIP_WINDOWS': 'false', 'SUPERSEDE_PENDING': 'false'}
            result = subprocess.run(['bash', '-c', script], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.splitlines(), ['scripts/releases/windows_stable.py', '--source', SOURCE, '--build', str(BUILD)])
            for key in ('MAC_ONLY', 'LINUX_ONLY', 'LINUX_ROLLBACK', 'SKIP_WINDOWS', 'SUPERSEDE_PENDING'):
                result = subprocess.run(['bash', '-c', script], env={**env, key: 'true'}, capture_output=True)
                self.assertNotEqual(result.returncode, 0, key); self.assertEqual(result.stdout, b'')
            result = subprocess.run(['bash', '-c', script], env={**env, 'PROMOTE_BUILD': ''}, capture_output=True)
            self.assertNotEqual(result.returncode, 0); self.assertEqual(result.stdout, b'')
