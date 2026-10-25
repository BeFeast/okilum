"""Calibration includes vendor/build inputs and refuses unknown source drift."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('build_ab', Path(__file__).with_name('build-ab.py'))
build_ab = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build_ab)


class BuildInputsTests(unittest.TestCase):
    def test_vendor_assets_and_unknown_build_inputs_change_guard(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            def git(*args):
                return subprocess.check_output(['git', '-C', folder, *args], text=True).strip()
            git('init', '-q')
            git('config', 'user.name', 'Test')
            git('config', 'user.email', 'test@example.invalid')
            def commit(path):
                target = root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text('changed')
                git('add', '.')
                git('commit', '-qm', path)
                return build_ab.product_inputs(root, git('rev-parse', 'HEAD'))
            previous = commit('Cargo.toml')
            for path in ('scripts/vendor-setup.sh', 'scripts/patches/fix.diff',
                         'crates/app/assets/font.ttf', 'new-build-input.sh'):
                current = commit(path)
                self.assertNotEqual(previous, current, path)
                previous = current
            for path in ('docs/readme.md', 'scripts/perf/editor/ab.py', '.github/workflows/perf.yml'):
                self.assertEqual(previous, commit(path), path)
