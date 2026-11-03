"""Exercise the real timeout wrapper with offline fake apt subprocesses."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

SCRIPT = Path(__file__).with_name('apt-bounded.sh').resolve()
# Start the fixture without launching a second Python interpreter under a
# two-second deadline. Ignored TERM is inherited by exec'ed sleep descendants.
FAKE_APT = r'''#!/bin/bash
set -eu
printf '%s\n' "$@" > "$FAKE_APT_ROOT/args"
case "$FAKE_APT_MODE" in
    success) exit 0 ;;
    failure) exit 42 ;;
    resistant)
        trap '' TERM
        bash -c 'printf "%s\n" "$$" > "$FAKE_APT_ROOT/child"; exec sleep 60' &
        ;;
esac
printf '%s\n' "$$" > "$FAKE_APT_ROOT/parent"
sleep 60
'''


def run_contained(command, env, watchdog):
    """An independent watchdog must also clean a broken timeout wrapper."""
    with tempfile.TemporaryFile(mode='w+') as output:
        process = subprocess.Popen(command, env=env, stdout=output,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        try:
            return process.wait(timeout=watchdog)
        except subprocess.TimeoutExpired as error:
            output.seek(0)
            error.output = output.read()
            raise
        finally:
            # The group is private to this fixture. Even if its leader exited,
            # kill descendants holding output open or left by a broken wrapper.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()


def running(pid):
    """A killed orphan can remain as a zombie (Z), then dead (X), during reaping."""
    try:
        state = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[0]
        return state not in ('Z', 'X')
    except (FileNotFoundError, ProcessLookupError):
        return False


class RunningTests(unittest.TestCase):
    def test_live_and_exited_states(self):
        for state in ('R', 'S', 'D', 'T', 'Z', 'X'):
            with self.subTest(state=state), mock.patch.object(
                    Path, 'read_text', autospec=True,
                    return_value=f'12345 (fake apt (child)) {state} 1 2 3') as read:
                self.assertEqual(running(12345), state not in ('Z', 'X'))
                read.assert_called_once_with(Path('/proc/12345/stat'))

    def test_process_vanishes_during_stat_read(self):
        # ENOENT can happen at open; ESRCH can happen after open, during read.
        for error in (FileNotFoundError, ProcessLookupError):
            with self.subTest(error=error), mock.patch.object(
                    Path, 'read_text', autospec=True, side_effect=error) as read:
                self.assertFalse(running(12345))
                read.assert_called_once_with(Path('/proc/12345/stat'))

    def test_other_read_errors_propagate(self):
        with mock.patch.object(Path, 'read_text', side_effect=PermissionError):
            with self.assertRaises(PermissionError):
                running(12345)


@unittest.skipUnless(sys.platform == 'linux', 'GNU timeout and /proc contract')
class AptBoundedTests(unittest.TestCase):
    def invoke(self, mode):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            apt = root / 'apt-get'
            apt.write_text(FAKE_APT)
            apt.chmod(0o755)
            env = dict(os.environ, PATH=str(root) + ':' + os.environ['PATH'],
                       FAKE_APT_ROOT=tmp, FAKE_APT_MODE=mode,
                       OKILUM_APT_TIMEOUT='2s', OKILUM_APT_KILL_AFTER='0.3s')
            started = time.monotonic()
            try:
                status = run_contained(['bash', str(SCRIPT), 'update'], env,
                                       float(os.environ.get('OKILUM_APT_TEST_WATCHDOG', '30')))
                elapsed = time.monotonic() - started
                self.assertTrue((root / 'args').exists(), 'fake apt never started')
                self.assertEqual((root / 'args').read_text().splitlines()[-1], 'update')
                if mode in ('timeout', 'resistant'):
                    self.assertTrue((root / 'parent').exists(), 'apt never reached stall')
                    self.assertGreaterEqual(elapsed, 1.8, 'deadline did not fire')
                if mode == 'resistant':
                    self.assertTrue((root / 'child').exists(), 'descendant never started')
                    pids = [int((root / name).read_text()) for name in ('parent', 'child')]
                    deadline = time.monotonic() + 1
                    while any(running(pid) for pid in pids) and time.monotonic() < deadline:
                        time.sleep(0.02)
                    self.assertFalse(any(running(pid) for pid in pids),
                                     'timeout left an apt process or descendant running')
                return status
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

    def test_outer_watchdog_cleans_a_broken_wrapper_group(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            command = ['bash', '-c',
                       'trap "" TERM; echo $$ > "$FAKE_APT_ROOT/parent"; '
                       'bash -c \'echo $$ > "$FAKE_APT_ROOT/child"; exec sleep 60\' & wait']
            env = dict(os.environ, FAKE_APT_ROOT=tmp)
            with self.assertRaises(subprocess.TimeoutExpired):
                run_contained(command, env, 1)
            for name in ('parent', 'child'):
                self.assertTrue((root / name).exists(), 'stall positive control did not run')
                pid = int((root / name).read_text())
                deadline = time.monotonic() + 1
                while running(pid) and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertFalse(running(pid), f'watchdog leaked {name}')

    def test_success_positive_control(self):
        self.assertEqual(self.invoke('success'), 0)

    def test_original_failure_status(self):
        self.assertEqual(self.invoke('failure'), 42)

    def test_timeout_status(self):
        self.assertEqual(self.invoke('timeout'), 124)

    def test_kill_deadline_cleans_term_resistant_descendant(self):
        # Direct signal termination and a waiting shell's 128+signal status
        # represent the same KILL. Surviving descendants are checked separately.
        self.assertIn(self.invoke('resistant'), (-signal.SIGKILL, 128 + signal.SIGKILL))


if __name__ == '__main__':
    unittest.main()
