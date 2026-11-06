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
    def test_remove_yaml_requires_explicit_write_and_external_backup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for flags in (["--remove-yaml"], ["--write", "--remove-yaml"],
                          ["--write", "--remove-yaml", "--backup-dir", str(root / "backup")]):
                result = subprocess.run([sys.executable, str(SCRIPT), str(root), *flags],
                                        capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
            self.assertFalse((root / "backup").exists())

    def test_remove_yaml_backs_up_bytes_permissions_mtime_and_keeps_equivalent_toml(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "Areas" / "Example" / "ops" / "project.yaml"
            source.parent.mkdir(parents=True)
            original = b"# human comment\nname: Example\nkind: knowledge\nunused: null\n"
            source.write_bytes(original)
            source.chmod(0o600)
            os.utime(source, ns=(1_700_000_000_000_000_000, 1_700_000_000_000_000_000))
            metadata = source.stat()
            result = subprocess.run([sys.executable, str(SCRIPT), str(root / "Areas"),
                                     "--write", "--remove-yaml", "--backup-dir", str(root / "backup")],
                                    capture_output=True, text=True, check=True)
            self.assertIn('"yaml": "removed"', result.stdout)
            backup = root / "backup" / "Example" / "ops" / "project.yaml"
            self.assertEqual(backup.read_bytes(), original)
            self.assertEqual(backup.stat().st_mtime_ns, metadata.st_mtime_ns)
            if os.name == "posix":
                self.assertEqual(backup.stat().st_mode, metadata.st_mode)
            self.assertFalse(source.exists())
            target = source.with_suffix(".toml")
            self.assertEqual(tomlkit.parse(target.read_text()).unwrap(),
                             {"name": "Example", "kind": "knowledge"})
            self.assertIn("human comment", target.read_text())

    def test_differing_backup_or_toml_and_failed_backup_never_delete_yaml(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, backup = root / "project.yaml", root / "backup" / "project.yaml"
            source.write_text("name: Example\n")
            converter.convert_file(source, True)
            backup.parent.mkdir()
            backup.write_bytes(b"older human data")
            with self.assertRaisesRegex(ValueError, "backup differs"):
                converter.convert_file(source, True, backup)
            self.assertEqual(source.read_text(), "name: Example\n")
            self.assertEqual(backup.read_bytes(), b"older human data")
            backup.unlink()
            with mock.patch.object(converter, "atomic_create", side_effect=OSError("disk full")):
                with self.assertRaisesRegex(OSError, "disk full"):
                    converter.convert_file(source, True, backup)
            self.assertTrue(source.exists())
            source.with_suffix(".toml").write_text('name = "Different"\n')
            with self.assertRaisesRegex(ValueError, "differs"):
                converter.convert_file(source, True, backup)
            self.assertTrue(source.exists())
            self.assertFalse(backup.exists())

    def test_replacement_during_removal_is_restored_instead_of_deleted(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, backup = root / "project.yaml", root / "backup" / "project.yaml"
            source.write_text("name: Example\n")
            replace = converter.os.replace

            def concurrent_writer(old, new):
                Path(old).write_text("name: Human changed it\n")
                replace(old, new)

            with mock.patch.object(converter.os, "replace", concurrent_writer):
                with self.assertRaisesRegex(ValueError, "YAML changed"):
                    converter.convert_file(source, True, backup)
            self.assertEqual(source.read_text(), "name: Human changed it\n")
            self.assertEqual(backup.read_text(), "name: Example\n")
            self.assertFalse(list(root.glob(".project.yaml.recovery-*")))

    def test_concurrent_creator_is_never_overwritten_and_recovery_is_reported(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, backup = root / "project.yaml", root / "backup" / "project.yaml"
            source.write_text("name: Example\n")
            replace = converter.os.replace

            def concurrent_writer(old, new):
                replace(old, new)
                Path(old).write_text("name: New entry\n")

            with mock.patch.object(converter.os, "replace", concurrent_writer):
                with self.assertRaisesRegex(ValueError, "retained for recovery"):
                    converter.convert_file(source, True, backup)
            self.assertEqual(source.read_text(), "name: New entry\n")
            self.assertEqual(backup.read_text(), "name: Example\n")
            recovery, = root.glob(".project.yaml.recovery-*")
            self.assertEqual(recovery.read_text(), "name: Example\n")

    @unittest.skipUnless(os.name == "posix", "Unix symlink semantics")
    def test_symlinked_backup_parent_is_refused_before_creating_directories(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "project.yaml"
            source.write_text("name: Example\n")
            outside = root / "outside"
            outside.mkdir()
            (root / "backup").symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symlinks"):
                converter.convert_file(source, True, root / "backup" / "Example" / "project.yaml")
            self.assertTrue(source.exists())
            self.assertFalse(list(outside.iterdir()))

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
