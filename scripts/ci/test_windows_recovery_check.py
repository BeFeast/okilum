"""A missing Windows regression or a failing native suite cannot pass the gate."""
import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "windows_recovery", Path(__file__).with_name("windows-recovery-check.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


class WindowsRecoveryCheckTests(unittest.TestCase):
    def test_missing_regression_stops_before_test_execution(self):
        with patch.object(check.subprocess, "run") as run:
            run.return_value.stdout = "0 tests, 0 benchmarks\n"
            with self.assertRaisesRegex(RuntimeError, "did not compile/discover"):
                check.main()
            self.assertEqual(run.call_count, 1)

    def test_discovered_regression_runs_and_propagates_failure(self):
        for result in (0, 101):
            listing = subprocess.CompletedProcess([], 0,
                check.REGRESSION + ": test\n" + check.PARENT_REGRESSION + ": test\n")
            with patch.object(check.subprocess, "run", side_effect=[
                    listing, subprocess.CompletedProcess([], result)]) as run:
                self.assertEqual(check.main(), result)
                self.assertIn("windows_", run.call_args.args[0])
                self.assertIn("--tests", run.call_args.args[0])
                self.assertIn("--test-threads=1", run.call_args.args[0])

    def test_missing_changed_parent_regression_stops_before_execution(self):
        with patch.object(check.subprocess, "run") as run:
            run.return_value.stdout = check.REGRESSION + ": test\n"
            with self.assertRaisesRegex(RuntimeError, check.PARENT_REGRESSION):
                check.main()
            self.assertEqual(run.call_count, 1)
