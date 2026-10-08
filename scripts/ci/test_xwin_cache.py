import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('xwin_cache', Path(__file__).with_name('xwin-cache.py'))
xwin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(xwin)


class CacheTests(unittest.TestCase):
    def test_cold_warm_corrupt_and_manifest_changed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / 'sdk'
            downloads = []
            def download():
                downloads.append(1)
                (root / 'xwin').mkdir()
                (root / 'xwin/DONE').write_text('x86_64\n')
                (root / 'xwin/kernel32.lib').write_bytes(b'library')
            self.assertEqual(xwin.prepare(root, 'manifest-a', download), 'miss')
            self.assertEqual(xwin.prepare(root, 'manifest-a', download), 'hit')
            self.assertEqual(len(downloads), 1)
            (root / 'xwin/kernel32.lib').write_bytes(b'corrupt')
            self.assertEqual(xwin.prepare(root, 'manifest-a', download), 'miss')
            self.assertEqual(xwin.prepare(root, 'manifest-b', download), 'miss')
            self.assertEqual(len(downloads), 3)
            (root / 'xwin/kernel32.lib').unlink()
            self.assertFalse(xwin.valid(root, 'manifest-b'))

    def test_done_alone_or_escaping_link_is_not_valid_sdk(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'xwin').mkdir()
            (root / 'xwin/DONE').write_text('x86_64\n')
            with self.assertRaises(ValueError):
                xwin.inventory(root)
            (root / 'xwin/a.lib').write_text('lib')
            (root / 'xwin/escape').symlink_to('/etc/passwd')
            with self.assertRaises(ValueError):
                xwin.inventory(root)

    def test_interrupted_download_never_gets_receipt(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / 'sdk'
            def failed():
                raise RuntimeError('network interrupted')
            with self.assertRaises(RuntimeError):
                xwin.prepare(root, 'key', failed)
            self.assertFalse((root / xwin.RECEIPT).exists())


class CacheRootTests(unittest.TestCase):
    def test_workspace_alias_is_allowed_but_sdk_escape_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            workspace = base / 'workspace'
            (workspace / 'target').mkdir(parents=True)
            alias = base / 'alias'
            alias.symlink_to(workspace, target_is_directory=True)
            self.assertEqual(xwin.cache_root(alias / 'target/xwin-sdk', alias), workspace / 'target/xwin-sdk')
            outside = base / 'outside'
            outside.mkdir()
            (workspace / 'target/xwin-sdk').symlink_to(outside, target_is_directory=True)
            with self.assertRaises(ValueError):
                xwin.cache_root(workspace / 'target/xwin-sdk', workspace)
