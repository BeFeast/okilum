"""Execute the real watchdog against a hang; no compiler or real daemons."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/ci/test-group.sh'
FIXTURE = ROOT / 'scripts/ci/fixtures/hung-test-group.py'


@unittest.skipUnless(sys.platform == 'linux', 'Linux fast-runner process supervision')
class TestGroup(unittest.TestCase):
    def test_hung_tree_including_detached_daemons_is_killed_and_reaped(self):
        with tempfile.TemporaryDirectory() as temp:
            work = Path(temp)
            started = time.monotonic()
            limit = 2
            try:
                result = subprocess.run(
                    ['bash', str(SCRIPT), '--kill-after', '0.3', 'hanging-positive-control', str(limit),
                     sys.executable, str(FIXTURE), temp, 'test'],
                    capture_output=True, text=True, timeout=limit + 32)
                elapsed = time.monotonic() - started
                self.assertTrue((work / 'ready').exists(), result.stdout + result.stderr)
                self.assertEqual({p.name for p in work.glob('*.pid')},
                                 {'test.pid', 'daemon.pid', 'detached.pid', 'helper.pid'})
                self.assertEqual(result.returncode, 124, result.stdout + result.stderr)
                self.assertEqual(result.stdout.count('TEST_GROUP_TIMEOUT '), 1)
                self.assertIn('group=hanging-positive-control limit=2s kill_after=0.3s', result.stdout)
                # The detached helper ignores TERM: this really exercises KILL.
                self.assertGreaterEqual(elapsed, 2.2)
                self.assertLess(elapsed, limit + 30 + 1)  # scheduling/launch tolerance
                for path in work.glob('*.pid'):
                    self.assertFalse(Path('/proc', path.read_text()).exists(), path.name)
            finally:
                # Keep a broken watchdog mutation from leaking fixture processes.
                for path in work.glob('*.pid'):
                    try:
                        pid = int(path.read_text())
                        command = Path('/proc', str(pid), 'cmdline').read_bytes()
                        if str(work).encode() in command:
                            os.kill(pid, signal.SIGKILL)
                    except (FileNotFoundError, ProcessLookupError):
                        pass

    def test_normal_output_arguments_and_exit_codes_are_preserved(self):
        for status in [0, 23, 124, 137]:
            with self.subTest(status=status):
                result = subprocess.run(
                    ['bash', str(SCRIPT), 'normal-control', '5', sys.executable, '-c',
                     'import sys; print(repr(sys.argv[1])); sys.exit(int(sys.argv[2]))',
                     'a b;$(false)', str(status)], capture_output=True, text=True, timeout=6)
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertIn(repr('a b;$(false)'), result.stdout)
                self.assertNotIn('TEST_GROUP_TIMEOUT', result.stdout)

    def test_successful_parent_cannot_leave_an_orphan(self):
        with tempfile.TemporaryDirectory() as temp:
            pidfile = Path(temp) / 'pid'
            code = ('import subprocess, sys; '
                    'p=subprocess.Popen([sys.executable,"-c","import time; time.sleep(120)"]); '
                    'open(sys.argv[1],"w").write(str(p.pid))')
            result = subprocess.run(['bash', str(SCRIPT), 'orphan-control', '5',
                                     sys.executable, '-c', code, str(pidfile)],
                                    capture_output=True, text=True, timeout=6)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse(Path('/proc', pidfile.read_text()).exists())

    def test_cancellation_propagates_failure_and_cleans_children(self):
        with tempfile.TemporaryDirectory() as temp:
            ready = Path(temp) / 'ready'
            code = ('import os,sys,time; open(sys.argv[1],"w").write(str(os.getpid())); '
                    'time.sleep(120)')
            process = subprocess.Popen(['bash', str(SCRIPT), 'cancel-control', '5',
                                        sys.executable, '-c', code, str(ready)],
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                deadline = time.monotonic() + 3
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(.02)
                self.assertTrue(ready.exists())
                process.terminate()
                stdout, stderr = process.communicate(timeout=3)
                self.assertEqual(process.returncode, 143, stdout + stderr)
                self.assertNotIn('TEST_GROUP_TIMEOUT', stdout)
                self.assertFalse(Path('/proc', ready.read_text()).exists())
            finally:
                if process.poll() is None:
                    process.kill()
                process.communicate(timeout=3)


    def test_sync_entrypoint_keeps_both_trials_and_policy_and_preserves_failure(self):
        for status in [0, 23]:
            with self.subTest(status=status), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                ci = root / 'scripts/ci'
                ci.mkdir(parents=True)
                for name in ['test-group.sh', 'test-group.py', 'sync-source-sharing-fast.sh']:
                    (ci / name).write_text((ROOT / 'scripts/ci' / name).read_text())
                vendor = root / 'scripts/vendor-setup.sh'
                vendor.write_text('#!/bin/sh\nexit 0\n')
                vendor.chmod(0o755)
                for name in ['sync-source-sharing.sh', 'sync-network-policy.sh']:
                    (ci / name).write_text(
                        'set -eu\ntest ! -e /proc/$$/fd/9\n'
                        'echo "$0 $* target=$CARGO_TARGET_DIR" >> "$COMMANDS"\n'
                        + ('exit "$SHARING_STATUS"\n' if name == 'sync-source-sharing.sh' else 'exit 0\n'))
                commands = root / 'commands'
                env = dict(os.environ, HOME=temp, COMMANDS=str(commands), SHARING_STATUS=str(status))
                result = subprocess.run(['bash', 'scripts/ci/sync-source-sharing-fast.sh'],
                                        cwd=root, env=env, capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, status, result.stdout + result.stderr)
                self.assertIn('TEST_GROUP_START group=sync-source-sharing limit=480s', result.stdout)
                rows = commands.read_text().splitlines()
                self.assertIn('--runs 2 --parallel 2', rows[0])
                self.assertIn(f'target={temp}/.cache/okilum-sync-source-sharing/target', rows[0])
                self.assertEqual(len(rows), 1 if status else 2)
                if not status:
                    self.assertIn('TEST_GROUP_START group=sync-network-policy limit=180s', result.stdout)
                    self.assertIn('sync-network-policy.sh', rows[1])
                # All commands, including the daemons' subprocesses, released the lock.
                lock = root / '.cache/okilum-sync-source-sharing/gate.lock'
                self.assertEqual(subprocess.run(['flock', '-n', str(lock), 'true']).returncode, 0)

    def test_workflow_bounds_setup_with_additive_job_budgets(self):
        workflow = (ROOT / '.forgejo/workflows/fast.yml').read_text()
        for command in [
            'fast-local-check 2100 bash scripts/ci/fast-local.sh',
            'fast-local-shell-full 1980 bash scripts/ci/fast-local.sh shell-full',
            'sync-local 1500 bash scripts/ci/sync-source-sharing-fast.sh',
        ]:
            wrapper = ('bash scripts/ci/test-group.sh ' if command.startswith('sync-local') else
                       'bash "${OKILUM_TEST_GROUP:-scripts/ci/test-group.sh}" ')
            self.assertIn(wrapper + command, workflow)
        self.assertIn('timeout-minutes: 55', workflow.split('  check:', 1)[1].split('  shell-full:', 1)[0])
        self.assertIn('timeout-minutes: 37', workflow.split('  shell-full:', 1)[1].split('  sync-source-sharing:', 1)[0])
        self.assertIn('timeout-minutes: 30', workflow.split('  sync-source-sharing:', 1)[1])


if __name__ == '__main__':
    unittest.main()
