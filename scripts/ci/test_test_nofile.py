"""A child test shell must survive descriptor pressure without host limit edits."""
import os
from pathlib import Path
import subprocess
import sys
import unittest

SCRIPT = Path(__file__).with_name('test-nofile.sh').resolve()
PROBE = '''import os
handles = []
try:
    for _ in range(1536): handles.append(os.open(os.devnull, os.O_RDONLY))
    print('DESCRIPTOR_PROBE_PASS')
finally:
    for handle in handles: os.close(handle)
'''


class TestProcessNofile(unittest.TestCase):
    def test_low_limit_positive_control_fails_then_helper_enables_same_probe(self):
        # Both runs are children: neither can change the parent or host limits.
        if subprocess.check_output(['bash', '-c', 'ulimit -Hn'], text=True).strip() == '1024':
            self.skipTest('host hard limit cannot support the descriptor probe')
        env = dict(os.environ, PROBE=PROBE, PYTHON=sys.executable)
        before = subprocess.run(['bash', '-eu', '-c',
            'ulimit -Sn 1024; "$PYTHON" -c "$PROBE"'], env=env, capture_output=True, text=True)
        self.assertNotEqual(before.returncode, 0)
        self.assertIn('Too many open files', before.stderr)
        after = subprocess.run(['bash', '-eu', '-c',
            'ulimit -Sn 1024; old_hard=$(ulimit -Hn); source "$1"; '
            'test "$(ulimit -Hn)" = "$old_hard"; "$PYTHON" -c "$PROBE"',
            'test', str(SCRIPT)], env=env, capture_output=True, text=True)
        self.assertEqual(after.returncode, 0, after.stderr)
        self.assertIn('DESCRIPTOR_PROBE_PASS', after.stdout)

    def test_raise_failure_stops_before_test_command(self):
        shell = '''
        ulimit() {
          case "$1" in -Sn) [[ $# == 1 ]] && echo 1024 ;;
            -Hn) echo 8192 ;; esac
        }
        source "$1"
        echo SHOULD_NOT_RUN
        '''
        result = subprocess.run(['bash', '-eu', '-c', shell, 'test', str(SCRIPT)],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('SHOULD_NOT_RUN', result.stdout)
        self.assertIn('Cannot raise test process soft nofile limit', result.stderr)
