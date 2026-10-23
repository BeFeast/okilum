"""Exercise the real timeout wrapper with offline fake apt subprocesses."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPT = Path(__file__).with_name('apt-bounded.sh').resolve()
FAKE_APT = r'''
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

root = Path(os.environ['FAKE_APT_ROOT'])
(root / 'args').write_text(json.dumps(sys.argv[1:]))
mode = os.environ['FAKE_APT_MODE']
if mode == 'success':
    sys.exit(0)
if mode == 'failure':
    sys.exit(42)
if mode == 'resistant':
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    child = subprocess.Popen([sys.executable, '-c',
        "import os, signal, time; from pathlib import Path; "
        "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        "Path(os.environ['FAKE_APT_ROOT'], 'child').write_text(str(os.getpid())); "
        "time.sleep(60)"])
(root / 'parent').write_text(str(os.getpid()))
time.sleep(60)
'''


def running(pid):
    """A killed orphan can briefly remain as a zombie until init reaps it."""
    try:
        return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[0] != 'Z'
    except FileNotFoundError:
        return False


@unittest.skipUnless(sys.platform == 'linux', 'GNU timeout and /proc contract')
class AptBoundedTests(unittest.TestCase):
    def invoke(self, mode):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            apt = root / 'apt-get'
            apt.write_text(f'#!{sys.executable}\n' + FAKE_APT)
            apt.chmod(0o755)
            env = dict(os.environ, PATH=str(root) + ':' + os.environ['PATH'],
                       FAKE_APT_ROOT=tmp, FAKE_APT_MODE=mode,
                       OKILUM_APT_TIMEOUT='2s', OKILUM_APT_KILL_AFTER='0.3s')
            started = time.monotonic()
            try:
                result = subprocess.run(['bash', str(SCRIPT), 'update'], env=env,
                                        capture_output=True, text=True, timeout=8)
                elapsed = time.monotonic() - started
                self.assertTrue((root / 'args').exists(), 'fake apt never started')
                self.assertEqual(json.loads((root / 'args').read_text())[-1], 'update')
                if mode in ('timeout', 'resistant'):
                    self.assertTrue((root / 'parent').exists(), 'apt never reached stall')
                    self.assertGreaterEqual(elapsed, 1.8, 'deadline did not fire')
                    self.assertLess(elapsed, 6, 'deadline failed to bound invocation')
                if mode == 'resistant':
                    self.assertTrue((root / 'child').exists(), 'descendant never started')
                    pids = [int((root / name).read_text()) for name in ('parent', 'child')]
                    deadline = time.monotonic() + 1
                    while any(running(pid) for pid in pids) and time.monotonic() < deadline:
                        time.sleep(0.02)
                    self.assertFalse(any(running(pid) for pid in pids),
                                     'timeout left an apt process or descendant running')
                return result.returncode
            finally:
                # A regression must fail without leaking our fake processes into CI.
                for name in ('parent', 'child'):
                    if (root / name).exists():
                        pid = int((root / name).read_text())
                        if running(pid):
                            try:
                                os.kill(pid, signal.SIGKILL)
                            except ProcessLookupError:
                                pass

    def test_success_positive_control(self):
        self.assertEqual(self.invoke('success'), 0)

    def test_original_failure_status(self):
        self.assertEqual(self.invoke('failure'), 42)

    def test_timeout_status(self):
        self.assertEqual(self.invoke('timeout'), 124)

    def test_kill_deadline_cleans_term_resistant_descendant(self):
        # GNU timeout itself receives SIGKILL when its process group resists TERM.
        self.assertEqual(self.invoke('resistant'), -signal.SIGKILL)


if __name__ == '__main__':
    unittest.main()
