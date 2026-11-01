import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class StaplerRetry(unittest.TestCase):
    def run_stapler(self, failures, operation='validate'):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'xcrun').write_text('''#!/bin/bash
echo "$*" >> "$TEST_ROOT/calls"
n=$(wc -l < "$TEST_ROOT/calls")
if (( n <= FAILURES )); then exit 68; fi
''')
            (root / 'sleep').write_text('#!/bin/bash\necho "$1" >> "$TEST_ROOT/delays"\n')
            for name in ['xcrun', 'sleep']:
                (root / name).chmod(0o700)
            env = {**os.environ, 'PATH': tmp + ':' + os.environ['PATH'],
                   'TEST_ROOT': tmp, 'FAILURES': str(failures)}
            result = subprocess.run(['bash', '-euc',
                                     'source "$1"; okilum_stapler "$2" "app with spaces.app"',
                                     'test', str(ROOT / 'scripts/updater/stapler.sh'), operation],
                                    env=env, capture_output=True, text=True, timeout=5)
            calls = (root / 'calls').read_text().splitlines()
            delays = (root / 'delays').read_text().splitlines() if (root / 'delays').exists() else []
            return result, calls, delays

    def test_success_does_not_retry(self):
        result, calls, delays = self.run_stapler(0)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(calls, ['stapler validate app with spaces.app'])
        self.assertEqual(delays, [])

    def test_transient_staple_and_validate_recover(self):
        for operation in ['staple', 'validate']:
            with self.subTest(operation=operation):
                result, calls, delays = self.run_stapler(2, operation)
                self.assertEqual(result.returncode, 0)
                self.assertEqual(len(calls), 3)
                self.assertEqual(delays, ['5', '15'])

    def test_persistent_failure_stops_after_three(self):
        result, calls, delays = self.run_stapler(10)
        self.assertEqual(result.returncode, 68)
        self.assertEqual(len(calls), 3)
        self.assertEqual(delays, ['5', '15'])

    def test_upload_precedes_warning_only_cleanup(self):
        text = (ROOT / '.github/workflows/forgejo-macos-shipping.yml').read_text()
        self.assertLess(text.index('actions/upload-artifact@v4'),
                        text.index('- name: Remove ephemeral signing material'))
        cleanup = text.split('- name: Remove ephemeral signing material')[1]
        self.assertIn('if: always()', cleanup)
        self.assertIn('continue-on-error: true', cleanup)
        self.assertIn('::warning::', cleanup)


if __name__ == '__main__':
    unittest.main()
