"""Unchanged manifests must be saved before a cached graph receipt reads them."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('cached_graph', Path(__file__).with_name('cached_graph.py'))
graph = importlib.util.module_from_spec(spec)
spec.loader.exec_module(graph)


class CachedGraphTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'checkout'
        self.out = Path(self.temp.name) / 'packet'
        self.out.mkdir()
        for name in ['Cargo.toml', 'Cargo.lock', 'vendor/gpui-component/Cargo.lock',
                     'crates/okilum-shell/Cargo.toml', 'crates/okilum-t3/Cargo.toml',
                     'crates/okilum-t3/src/lib.rs', 'vendor/gpui-component/crates/base/src/lib.rs']:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('original ' + name)
            os.utime(path, ns=(1234567890000000000, 1234567890000000000))
        # The t3 manifest is intentionally absent: it was NOT changed by the PR.
        self.contract = {'files': {'crates/okilum-t3/src/lib.rs': {
            'sha256': graph.digest(self.root / 'crates/okilum-t3/src/lib.rs')}}}

    def test_unchanged_manifest_is_snapshotted_and_receipt_succeeds(self):
        graph.save(self.root, self.out, self.contract)
        graph.verify(self.root, self.out)
        manifest = 'crates/okilum-t3/Cargo.toml'
        self.assertNotIn(manifest, self.contract['files'])
        self.assertIn(manifest, json.loads((self.out / 'protected.json').read_text()))
        original, saved = self.root / manifest, self.out / 'originals' / manifest
        self.assertEqual(saved.read_bytes(), original.read_bytes())
        self.assertEqual(saved.stat().st_mtime_ns, original.stat().st_mtime_ns)
        # Positive control: the receipt really detects a graph mutation.
        original.write_text('changed graph')
        with self.assertRaisesRegex(ValueError, 'Graph input changed: crates/okilum-t3/Cargo.toml'):
            graph.verify(self.root, self.out)

    def test_new_manifest_is_not_silently_outside_the_receipt(self):
        graph.save(self.root, self.out, self.contract)
        path = self.root / 'crates/added/Cargo.toml'
        path.parent.mkdir()
        path.write_text('new member')
        with self.assertRaisesRegex(ValueError, 'Manifest inventory changed'):
            graph.verify(self.root, self.out)

    def test_bad_contract_or_missing_required_input_fails_before_snapshot(self):
        self.contract['files']['crates/okilum-t3/src/lib.rs']['sha256'] = 'bad'
        with self.assertRaisesRegex(ValueError, 'Contract hash mismatch'):
            graph.save(self.root, self.out, self.contract)
        self.assertFalse((self.out / 'originals').exists())
        (self.root / 'Cargo.lock').unlink()
        with self.assertRaisesRegex(ValueError, 'Invalid or missing protected input'):
            graph.save(self.root, self.out, self.contract)

    def test_snapshot_is_never_overwritten_and_target_sources_are_excluded(self):
        path = self.root / 'vendor/gpui-component/target/generated.rs'
        path.parent.mkdir()
        path.write_text('build output')
        graph.save(self.root, self.out, self.contract)
        saved = self.out / 'originals/Cargo.lock'
        before = saved.read_bytes()
        with self.assertRaises(FileExistsError):
            graph.save(self.root, self.out, self.contract)
        self.assertEqual(saved.read_bytes(), before)
        self.assertFalse((self.out / 'originals/vendor/gpui-component/target').exists())
