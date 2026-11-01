"""Real Git checkout/mtime controls; no Cargo, runner or service mutations."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('fast-source.sh').resolve()


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()


class StableSlotSource(unittest.TestCase):
    def commit(self, root, text):
        (root / 'library.rs').write_text(text)
        git(root, 'add', 'library.rs')
        git(root, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
            'commit', '-qm', 'fixture')
        return git(root, 'rev-parse', 'HEAD')

    def run_source(self, root, target, sha):
        return subprocess.run(['bash', str(SCRIPT), str(target)], cwd=root,
                              env={**os.environ, 'GITHUB_SHA': sha}, capture_output=True,
                              text=True, timeout=5)

    def test_different_runner_paths_reuse_exact_source_and_vendor_mtimes(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp); first = base / 'runner-first'; first.mkdir()
            git(first, 'init', '-q'); sha = self.commit(first, 'pub fn original() {}\n')
            target = base / 'slot-source'
            self.assertEqual(self.run_source(first, target, sha).returncode, 0)
            # Sentinel timestamps make accidental rewriting observable without sleeps.
            os.utime(target / 'library.rs', ns=(1_600_000_000_000_000_000,) * 2)
            vendor = target / 'vendor/patched'; vendor.mkdir(parents=True)
            (vendor / 'lib.rs').write_text('verified patch fixture')
            os.utime(vendor / 'lib.rs', ns=(1_600_000_001_000_000_000,) * 2)
            before = [(p.read_bytes(), p.stat().st_mtime_ns)
                      for p in [target / 'library.rs', vendor / 'lib.rs']]
            (target / 'stray').write_text('remove only disposable source residue')
            second = base / 'runner-second'
            subprocess.run(['git', 'clone', '-q', str(first), str(second)], check=True)
            result = self.run_source(second, target, sha)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(before, [(p.read_bytes(), p.stat().st_mtime_ns)
                                     for p in [target / 'library.rs', vendor / 'lib.rs']])
            self.assertFalse((target / 'stray').exists())
            changed = self.commit(second, 'pub fn changed() {}\n')
            self.assertEqual(self.run_source(second, target, changed).returncode, 0)
            self.assertEqual((target / 'library.rs').read_text(), 'pub fn changed() {}\n')
            self.assertEqual(git(target, 'rev-parse', 'HEAD'), changed)

    def test_wrong_head_dirty_source_and_aliased_destination_reject(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / 'input'; root.mkdir(); git(root, 'init', '-q')
            sha = self.commit(root, 'original')
            target = Path(temp) / 'slot'
            self.assertNotEqual(self.run_source(root, target, 'a' * 40).returncode, 0)
            self.assertFalse(target.exists())
            (root / 'library.rs').write_text('dirty')
            self.assertNotEqual(self.run_source(root, target, sha).returncode, 0)
            self.assertFalse(target.exists())
            git(root, 'checkout', '--', 'library.rs')
            outside = Path(temp) / 'outside'; outside.mkdir(); target.symlink_to(outside)
            self.assertNotEqual(self.run_source(root, target, sha).returncode, 0)
            self.assertEqual(list(outside.iterdir()), [])


if __name__ == '__main__':
    unittest.main()
