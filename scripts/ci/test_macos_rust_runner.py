"""Native suite verdict controls; shell mocks only, no Cargo process is started."""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
import yaml


SCRIPT = Path(__file__).with_name("check-macos.sh").read_text()
RUN_TESTS = re.search(r"^run_tests\(\) \{.*?^\}", SCRIPT, re.M | re.S)[0]


class NativeRustVerdicts(unittest.TestCase):
    def test_split_jobs_execute_the_complete_gate_without_serial_production_build(self):
        # Execute the actual shell orchestration with synthetic platform tools.
        # Cargo is never started on this host. The old unsplit script fails
        # both mode checks: production reaches tests and tests builds the app.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'scripts/ci').mkdir(parents=True)
            (root / 'scripts/ci/release-cache.sh').write_text('release_cache_stats() { :; }\n')
            (root / 'scripts/ci/check-supervisor-bundle.sh').write_text('echo supervisor-bundle >> "$RECEIPT"\n')
            (root / 'scripts/vendor-setup.sh').write_text(':\n')
            binary = root / 'bin'
            binary.mkdir()
            programs = {
                'uname': 'case "$1" in -s) echo Darwin;; -m) echo arm64;; esac',
                'rustc': ':', 'rustup': ':',
                'python3': 'echo "python $*" >> "$RECEIPT"',
                'cargo': 'echo "cargo $*" >> "$RECEIPT"; echo "fixture::positive: test"',
                'xcrun': '''
if [ "$1" = --sdk ]; then echo /synthetic-sdk; exit; fi
while [ "$1" != -o ]; do shift; done
printf '#!/bin/sh\nexit 0\n' > "$2"
chmod +x "$2"
''',
            }
            for name, text in programs.items():
                path = binary / name
                path.write_text('#!/bin/sh\n' + text + '\n')
                path.chmod(0o755)
            script = root / 'check-macos.sh'
            script.write_text(SCRIPT)
            receipts = {}
            for mode in ('all', 'production', 'tests'):
                receipt = root / (mode + '.receipt')
                result = subprocess.run(['bash', str(script), mode], cwd=root,
                    env=dict(os.environ, PATH=str(binary) + os.pathsep + os.environ['PATH'],
                             HOME=str(root), RUNNER_TEMP=str(root), RECEIPT=str(receipt),
                             GITHUB_ACTIONS='true', GITHUB_SERVER_URL='https://github.com',
                             OKILUM_NETWORK_WRITE_QA='0', OKILUM_ENCODING_REPAIR_QA='0'),
                    capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                receipts[mode] = receipt.read_text().splitlines()
            production_build = 'cargo build --release --locked --target aarch64-apple-darwin -p okilum-shell'
            self.assertIn(production_build, receipts['production'])
            self.assertNotIn(production_build, receipts['tests'])
            self.assertFalse(any(line.startswith('cargo test') for line in receipts['production']))
            for line in receipts['all']:
                # Shared setup intentionally runs in both isolated jobs.
                self.assertIn(line, receipts['production'] + receipts['tests'])
            for line in receipts['tests']:
                self.assertIn(line, receipts['all'])
            for control in ('negative_control.py', 'sparkle-freshness/run.py'):
                self.assertTrue(any(control in line for line in receipts['production']))
                self.assertFalse(any(control in line for line in receipts['tests']))
            self.assertTrue(any('--workspace --no-run' in line for line in receipts['tests']))
            workflow = yaml.safe_load((Path(__file__).parents[2] / '.github/workflows/forgejo-macos.yml').read_text())
            for job, mode in (('macos', 'tests'), ('production', 'production')):
                value = workflow['jobs'][job]
                self.assertEqual(value['needs'], 'select')
                self.assertEqual(value['timeout-minutes'], 75)
                commands = [step.get('run', '') for step in value['steps']]
                self.assertTrue(any('check-macos.sh ' + mode in command for command in commands))

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
