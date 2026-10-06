import importlib.util
from pathlib import Path
import unittest
import os
import subprocess

spec = importlib.util.spec_from_file_location('scope', Path(__file__).with_name('macos-scope.py'))
scope = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scope)


class ScopeTests(unittest.TestCase):
    def test_docs_only(self):
        self.assertFalse(scope.needs_macos(['README.md', 'docs/design/reader.md']))
        self.assertFalse(scope.needs_macos([]))

    def test_other_platform_and_web_changes_do_not_occupy_mac(self):
        self.assertFalse(scope.needs_macos([
            'web/inbox/app.js', 'inbox/crates/tessera-inboxd/src/web.rs',
            'scripts/windows/pack.sh', 'scripts/arch/PKGBUILD',
            'docs/images/reader.png', '.forgejo/workflows/linux-release.yml']))
        for path in ['crates/tessera-core/src/file_editor.rs',
                     'crates/tessera-core/src/vault.rs',
                     'crates/tessera-shell/src/reader_replay.rs',
                     'scripts/ci/release-cache.sh', 'scripts/build-macos-ci.sh']:
            self.assertTrue(scope.needs_macos(['web/inbox/app.js', path]))

    def test_native_inputs_and_unknown_paths(self):
        for path in ['crates/tessera-shell/src/main.rs', 'crates/tessera-core/tests/vault.md',
                     'Cargo.lock', 'Cargo.toml', '.cargo/config.toml',
                     'rust-toolchain.toml', 'scripts/patches/change.diff',
                     '.forgejo/workflows/ci.yml', 'assets/font.ttf',
                     'docs/fixture.bin', 'new-input', 'filename\nwith-newline.rs']:
            with self.subTest(path=path):
                self.assertTrue(scope.needs_macos(['README.md', path]))

    def test_required_gate_fails_closed(self):
        workflow = Path(__file__).resolve().parents[2] / '.forgejo/workflows/ci.yml'
        gate = workflow.read_text().split(
            '      - name: Require Linux and applicable macOS checks', 1)[1]
        script = '\n'.join(line[10:] for line in gate.split('        run: |\n', 1)[1].splitlines())
        for event in ['pull_request', 'push']:
            for linux in ['success', 'failure', 'cancelled', 'skipped']:
                for needed in ['true', 'false', '']:
                    for native in ['success', 'failure', 'cancelled', 'skipped']:
                        expected = linux == 'success' and (
                            event == 'push' or needed == 'false' or
                            (needed == 'true' and native == 'success'))
                        with self.subTest(event=event, linux=linux, needed=needed, native=native):
                            result = subprocess.run(['bash', '-c', script], capture_output=True,
                                env=dict(os.environ, EVENT=event, LINUX_RESULT=linux,
                                         MACOS_REQUIRED=needed, MACOS_RESULT=native))
                            self.assertEqual(result.returncode == 0, expected)


if __name__ == '__main__':
    unittest.main()
