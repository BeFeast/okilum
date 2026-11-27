"""Unit tests for the native-check binary guard (#1043)."""
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import native_binary_guard as guard  # noqa: E402


def fake(directory, body):
    path = Path(directory) / 'okilum'
    path.write_text('#!/bin/sh\n' + body)
    path.chmod(path.stat().st_mode | stat.S_IEXEC)
    return path


class Guard(unittest.TestCase):
    def run_guard(self, binary):
        code = f'import sys; sys.path.insert(0, {str(SCRIPTS)!r}); ' \
               f'import native_binary_guard as g; g.require_app_binary({str(binary)!r}); print("ok")'
        return subprocess.run([sys.executable, '-c', code], capture_output=True, text=True)

    def test_test_support_binary_is_an_invalid_run(self):
        with tempfile.TemporaryDirectory() as d:
            result = self.run_guard(fake(d, 'echo "version: 1"; echo "gpui-test-support: yes"\n'))
            self.assertEqual(result.returncode, guard.INVALID)
            self.assertIn('INVALID run', result.stderr)

    def test_kit_test_support_binary_is_an_invalid_run(self):
        # #3233: the okilum-shell-kit test seams leak like GPUI's.
        with tempfile.TemporaryDirectory() as d:
            result = self.run_guard(fake(d, 'echo "gpui-test-support: no"; echo "shell-kit-test-support: yes"\n'))
            self.assertEqual(result.returncode, guard.INVALID)
            self.assertIn('okilum-shell-kit test-support', result.stderr)

    def test_platform_test_support_binary_is_an_invalid_run(self):
        # #3234: the okilum-shell-platform test seams leak like GPUI's.
        with tempfile.TemporaryDirectory() as d:
            result = self.run_guard(fake(d, 'echo "gpui-test-support: no"; echo "shell-platform-test-support: yes"\n'))
            self.assertEqual(result.returncode, guard.INVALID)
            self.assertIn('okilum-shell-platform test-support', result.stderr)

    def test_text_test_support_binary_is_an_invalid_run(self):
        # #3235: the okilum-shell-text test seams leak like GPUI's.
        with tempfile.TemporaryDirectory() as d:
            result = self.run_guard(fake(d, 'echo "gpui-test-support: no"; echo "shell-text-test-support: yes"\n'))
            self.assertEqual(result.returncode, guard.INVALID)
            self.assertIn('okilum-shell-text test-support', result.stderr)

    def test_app_binary_passes(self):
        # Positive control: the same guard lets a normal build through.
        with tempfile.TemporaryDirectory() as d:
            result = self.run_guard(fake(d, 'echo "gpui-test-support: no"; echo "shell-kit-test-support: no"; echo "shell-platform-test-support: no"; echo "shell-text-test-support: no"\n'))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('ok', result.stdout)

    def test_binary_without_build_info_warns_and_continues(self):
        with tempfile.TemporaryDirectory() as d:
            result = self.run_guard(fake(d, 'echo "Unknown option: --build-info" >&2; exit 2\n'))
            self.assertEqual(result.returncode, 0)
            self.assertIn('predates --build-info', result.stderr)


if __name__ == '__main__':
    unittest.main()
