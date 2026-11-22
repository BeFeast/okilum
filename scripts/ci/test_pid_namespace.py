"""PID-namespace isolation controls for managed-admission host discovery (#3186).

They need `sudo unshare --pid --mount-proc`, which only hosts advertising the
build-slot capability `pidns` provide (okilum-dev has sudo but refuses the
/proc mount). They run only in the pool `admission` shard (pool-linux-gate.sh
sets OKILUM_PIDNS_CONTROLS=1, so there they are mandatory and a missing
capability fails). Pre-merge and `base` exclude this module from discovery by
name (OKILUM_UNITTEST_EXCLUDE=test_pid_namespace); any other discovery skips
them.
"""
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
MODULE = "test_pid_namespace"
PIDNS = runpy.run_path(str(ROOT / "scripts/ci/pid-namespace.py"), run_name="pidns")


def load_tests(loader, tests, pattern):
    """Discovery named in OKILUM_UNITTEST_EXCLUDE (pre-merge, base) gets nothing."""
    if MODULE in os.environ.get("OKILUM_UNITTEST_EXCLUDE", "").split(","):
        return unittest.TestSuite()
    return tests


@unittest.skipUnless(os.environ.get("OKILUM_PIDNS_CONTROLS") == "1",
                     "runs only in the pool admission shard (OKILUM_PIDNS_CONTROLS=1)")
class PidNamespaceControls(unittest.TestCase):
    """A sibling slot's same-uid Syncthing must not reach host discovery; leaks still fail."""

    def test_a_concurrent_sibling_syncthing_is_invisible_inside_and_status_and_env_pass_through(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            PIDNS["probe"](work)  # raises unless the decoy is visible outside only
            sibling_dir = work / "sibling"
            sibling_dir.mkdir()
            shutil.copy2(shutil.which("bash"), sibling_dir / "syncthing")
            sibling = subprocess.Popen(["bash", "-c", "read -r -t 60 _ || true"],
                                       executable=str(sibling_dir / "syncthing"), stdin=subprocess.PIPE)
            try:
                scan = ("import runpy, sys, os; ns = runpy.run_path(sys.argv[1], run_name='x'); "
                        "print(ns['same_uid_syncthing']()); print(os.environ['POOL_TEST_VALUE']); sys.exit(7)")
                argv, env_file, _ = PIDNS["command"](
                    [sys.executable, "-I", "-c", scan, str(ROOT / "scripts/ci/pid-namespace.py")],
                    {**os.environ, "POOL_TEST_VALUE": "kept"}, work)
                deadline = time.monotonic() + 5
                while sibling.pid not in PIDNS["same_uid_syncthing"]() and time.monotonic() < deadline:
                    time.sleep(.02)
                self.assertIn(sibling.pid, PIDNS["same_uid_syncthing"]())
                result = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                                        timeout=60)
                self.assertEqual(result.returncode, 7, result.stderr)
                self.assertEqual(result.stdout.split(), ["[]", "kept"])
                self.assertFalse(Path(env_file).exists())
                self.assertIsNone(sibling.poll())
            finally:
                sibling.kill()
                sibling.wait()
                sibling.stdin.close()

    def test_killing_the_run_ends_the_whole_namespace(self):
        with tempfile.TemporaryDirectory() as directory:
            argv, _, token = PIDNS["command"](["bash", "-c", "sleep 600 & wait"], dict(os.environ), Path(directory))
            process = subprocess.Popen(argv, stdin=subprocess.DEVNULL)
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and not self.marked(token, "sleep"):
                time.sleep(.05)
            self.assertTrue(self.marked(token, "sleep"))
            PIDNS["kill_marked"](token)
            self.assertNotEqual(process.wait(timeout=30), 0)
            self.assertFalse(self.marked(token))

    def test_a_successful_run_that_leaks_a_fixture_fails_before_the_namespace_ends(self):
        """Leaving PID 1 kills survivors, so the outside guard alone would see none."""
        guard = runpy.run_path(str(ROOT / "scripts/ci/sync-fixture-processes.py"), run_name="fixtures")
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            fixture_dir = work / "cache/okilum-sync-fixture/bin"
            fixture_dir.mkdir(parents=True)
            shutil.copy2(shutil.which("bash"), fixture_dir / "syncthing")
            fifo = work / "hold"
            os.mkfifo(fifo)
            home = work / "evidence/home"
            home.mkdir(parents=True)
            # A matching fixture: exe .../okilum-sync-fixture/.../syncthing, --home under the root.
            # Inside the fixture $0 is --home, $1 its home and $2 the fifo it waits on.
            start = ('"$0" -c \'read -r -t 60 _ <> "$2"\' --home "$2" "$1" & '
                     'for _ in $(seq 100); do [ -e /proc/$!/exe ] && break; sleep 0.05; done; ')
            for leave, expected in ((True, 70), (False, 0)):
                with self.subTest(leave_fixture=leave):
                    script = start + ('exit 0' if leave else 'kill -9 $!; wait $! 2>/dev/null; exit 0')
                    argv, _, token = PIDNS["command"](
                        ["bash", "-c", script, str(fixture_dir / "syncthing"), str(fifo), str(home)],
                        dict(os.environ), work, guard=work / "evidence")
                    result = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                                            timeout=60)
                    self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                    survivors = "survivors=1" if leave else "survivors=0"
                    self.assertIn(f"Fixture guard: root={work.resolve() / 'evidence'} {survivors}", result.stdout)
                    if leave:
                        self.assertIn("ORPHAN fixture", result.stdout)
                        self.assertIn("left fixture processes", result.stderr)
                    # The namespace is gone either way: nothing survives outside.
                    self.assertEqual(guard["fixtures"](work / "evidence"), [])
                    self.assertFalse(self.marked(token))
            # A failing command keeps its own code even when it also leaked.
            argv, _, _ = PIDNS["command"](
                ["bash", "-c", start + "exit 3", str(fixture_dir / "syncthing"), str(fifo), str(home)],
                dict(os.environ), work, guard=work / "evidence")
            result = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
            self.assertEqual(result.returncode, 3, result.stderr)
            self.assertIn("ORPHAN fixture", result.stdout)

    @staticmethod
    def marked(token, comm=None):
        found = []
        for entry in Path("/proc").iterdir():
            try:
                if entry.name.isdecimal() and f"OKILUM_PIDNS_TOKEN={token}".encode() in \
                        (entry / "environ").read_bytes().split(b"\0"):
                    if comm is None or (entry / "comm").read_text().strip() == comm:
                        found.append(int(entry.name))
            except (OSError, ProcessLookupError):
                continue
        return found


if __name__ == "__main__":
    unittest.main()
