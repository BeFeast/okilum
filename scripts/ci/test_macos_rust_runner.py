"""Native suite verdict controls; shell mocks only, no Cargo process is started."""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("check-macos.sh").read_text()
RUN_TESTS = re.search(r"^run_tests\(\) \{.*?^\}", SCRIPT, re.M | re.S)[0]


class NativeRustVerdicts(unittest.TestCase):
    def run_suites(self, list_case="match", run_status=0, second=False):
        with tempfile.TemporaryDirectory() as directory:
            program = r'''
set -euo pipefail
macos_rust_failed=0
cargo_args=(test)
SHARED=(--workspace)
cargo() {
    case " $* " in
        *" --list "*)
            if [[ "$LIST_CASE" == empty ]]; then return 0; fi
            echo 'fixture::positive: test'
            if [[ "$LIST_CASE" == failure ]]; then return 7; fi
            return 0
            ;;
        *)
            echo invoked >> "$RUNNER_TEMP/executed"
            return "$RUN_STATUS"
            ;;
    esac
}
'''
            program += RUN_TESTS + "\nrun_tests okilum-shell --bins fixture::\n"
            if second:
                program += "RUN_STATUS=0\nrun_tests okilum-core --lib fixture::\n"
            program += 'echo "FINAL_FAILED=$macos_rust_failed"\nexit "$macos_rust_failed"\n'
            result = subprocess.run(
                ["bash", "-c", program],
                env=dict(os.environ, RUNNER_TEMP=directory, LIST_CASE=list_case,
                         RUN_STATUS=str(run_status)),
                text=True, capture_output=True,
            )
            executed = Path(directory, "executed")
            calls = len(executed.read_text().splitlines()) if executed.exists() else 0
            return result, calls

    def test_matching_success_runs_and_passes(self):
        result, calls = self.run_suites()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("FINAL_FAILED=0", result.stdout)
        self.assertEqual(calls, 1)

    def test_failed_suite_continues_but_later_success_cannot_clear_it(self):
        result, calls = self.run_suites(run_status=3, second=True)
        self.assertEqual(calls, 2, result.stderr)
        self.assertIn("FINAL_FAILED=1", result.stdout)
        self.assertEqual(result.returncode, 1)

    def test_an_unfired_filter_fails_without_running_any_tests(self):
        result, calls = self.run_suites(list_case="empty")
        self.assertEqual(calls, 0)
        self.assertIn("No native Rust test matched", result.stderr)
        self.assertIn("FINAL_FAILED=1", result.stdout)
        self.assertEqual(result.returncode, 1)

    def test_failed_listing_cannot_pass_even_when_it_prints_a_match(self):
        result, calls = self.run_suites(list_case="failure")
        self.assertEqual(calls, 0)
        self.assertIn("FINAL_FAILED=1", result.stdout)
        self.assertEqual(result.returncode, 1)


if __name__ == "__main__":
    unittest.main()
