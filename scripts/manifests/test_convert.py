"""Conversion is explicit, loss-checked and never rewrites legacy files."""
import datetime
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import tomlkit

SCRIPT = Path(__file__).with_name('convert.py')
spec = importlib.util.spec_from_file_location('converter', SCRIPT)
converter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(converter)


class ConvertTests(unittest.TestCase):
    def test_cli_rejects_missing_or_nondirectory_root_instead_of_empty_success(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            file = root / 'file'
            file.write_text('not a directory')
            for path in (root / 'missing', file):
                result = subprocess.run([sys.executable, str(SCRIPT), str(path)], capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('existing directory', result.stderr)
            if os.name == 'posix':
                linked = root / 'linked'
                linked.symlink_to(root, target_is_directory=True)
                result = subprocess.run([sys.executable, str(SCRIPT), str(linked)], capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('not a symlink', result.stderr)

    def test_unreadable_subtree_is_an_error_instead_of_silently_missing_manifests(self):
        def unreadable(root, *, followlinks, onerror):
            onerror(PermissionError('fixture subtree is unreadable'))
            return iter(())

        with mock.patch.object(converter.os, 'walk', unreadable), self.assertRaises(PermissionError):
            list(converter.manifests(Path('fixture')))

    def test_comments_nulls_dates_and_numeric_keys_are_reported_and_preserved(self):
        source = '''# Identity belongs to the human
name: Example # display name
description: |
  # this is actual description text
  Keep it here.
unused: null # not configured
overrides: {4: disabled}
day: 2026-10-29
'''
        output, changes, comments = converter.translate(source)
        value = tomlkit.parse(output).unwrap()
        self.assertEqual(value['description'], '# this is actual description text\nKeep it here.\n')
        self.assertEqual(value['overrides'], {'4': 'disabled'})
        self.assertEqual(value['day'], '2026-10-29')
        self.assertNotIn('unused', value)
        for text in ('Identity belongs to the human', 'display name', 'not configured'):
            self.assertIn(text, output)
        self.assertEqual(comments, 3)
        self.assertEqual({c['kind'] for c in changes}, {'omitted_null', 'string_key', 'iso_date_string'})

    def test_unsupported_values_are_refused_without_silent_loss(self):
        for source in ('a: [null]', 'a: .inf', 'a: &cycle {self: *cycle}', 'a: 1\na: 2',
                       'a: {4: null, "4": enabled}'):
            with self.subTest(source=source), self.assertRaises(Exception):
                converter.translate(source)

    def test_dry_run_write_and_second_run_leave_yaml_bytes_and_mtime_intact(self):
        with tempfile.TemporaryDirectory() as root:
            area = Path(root) / 'Example' / 'ops'
            area.mkdir(parents=True)
            source = area / 'project.yaml'
            source.write_text('name: Example\nkind: knowledge\nt3: {manager: {thread_id: null}}\n')
            original = (source.read_bytes(), source.stat().st_mtime_ns)
            dry = subprocess.run([sys.executable, str(SCRIPT), root], capture_output=True, text=True, check=True)
            self.assertIn('would_create', dry.stdout)
            target = source.with_suffix('.toml')
            self.assertFalse(target.exists())
            self.assertEqual(original, (source.read_bytes(), source.stat().st_mtime_ns))
            self.assertEqual(converter.convert_file(source, True)['status'], 'created')
            saved = (target.read_bytes(), target.stat().st_mtime_ns)
            self.assertEqual(converter.convert_file(source, True)['status'], 'unchanged')
            self.assertEqual(saved, (target.read_bytes(), target.stat().st_mtime_ns))
            self.assertEqual(original, (source.read_bytes(), source.stat().st_mtime_ns))
            self.assertEqual(tomlkit.parse(saved[0].decode()).unwrap()['t3'], {'manager': {}})
            target.write_text('name = "Different"\n')
            with self.assertRaisesRegex(ValueError, 'differs'):
                converter.convert_file(source, True)
            self.assertEqual(target.read_text(), 'name = "Different"\n')

    def test_atomic_publication_cannot_replace_a_concurrent_writer(self):
        with tempfile.TemporaryDirectory() as root:
            target = Path(root) / 'project.toml'
            target.write_bytes(b'human data')
            with self.assertRaises(FileExistsError):
                converter.atomic_create(target, b'replacement', 0o600)
            self.assertEqual(target.read_bytes(), b'human data')
            self.assertEqual(list(Path(root).iterdir()), [target])

    @unittest.skipUnless(os.name == 'posix', 'Unix symlink semantics')
    def test_symlink_sources_targets_and_subtrees_are_never_followed(self):
        with tempfile.TemporaryDirectory() as root:
            area = Path(root) / 'ops'
            area.mkdir()
            outside = Path(root) / 'outside.yaml'
            outside.write_text('name: Private\n')
            source = area / 'project.yaml'
            source.symlink_to(outside)
            with self.assertRaisesRegex(ValueError, 'symlink'):
                converter.convert_file(source, True)
            source.unlink()
            source.write_text('name: Public\n')
            source.with_suffix('.toml').symlink_to(outside)
            with self.assertRaisesRegex(ValueError, 'symlink'):
                converter.convert_file(source, True)
            (Path(root) / 'linked').symlink_to(area, target_is_directory=True)
            self.assertEqual(list(converter.manifests(Path(root))), [source])


if __name__ == '__main__':
    unittest.main()
