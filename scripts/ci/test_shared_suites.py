"""Failures must neither hide later suites nor make the aggregate green."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('check-shared.sh').resolve()
FAKE = r'''
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
if Path(sys.argv[0]).name == 'python3':
    if args[0].endswith('/test-group.py'):
        assert args[1:5] == ['--isolated-writer-account', 'shared-log', '3600', 'cargo'], args
        os.execvp(args[4], args[4:])
    name = args[args.index('-s') + 1] if '-s' in args else args[0]
    if name == 'scripts/ci/workspace-rust-tests.py':
        assert args == ['scripts/ci/workspace-rust-tests.py', '--shared'], args
        name = 'workspace'
else:
    name = args[0]
    if name == 'test':
        name = ('logs' if 'reader_log' in args[:args.index('--')] else 'workspace') if '--workspace' in args else 'portable'
        assert '--no-fail-fast' in args, args
with open(os.environ['SHARED_TRACE'], 'a') as trace:
    trace.write(json.dumps([name, args]) + '\n')
pooled = (name == 'workspace' and '--skip' in args and 'reader_log' not in args)
sys.exit(42 if name in os.environ.get('SHARED_FAILURES', '').split(',') else
         43 if pooled and os.environ.get('SHARED_POOL_CONFLICT') == '1' else 0)
'''
EXPECTED = ['fmt', 'clippy', 'workspace', 'logs', 'portable', 'scripts/ci',
            'scripts/releases', 'scripts/manifests/check.py', 'scripts/updater']


class SharedSuitesTests(unittest.TestCase):
    def invoke(self, failures, pool_conflict=False):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name in ('cargo', 'python3'):
                command = root / name
                command.write_text(f'#!{sys.executable}\n' + FAKE)
                command.chmod(0o755)
            trace = root / 'trace'
            summary = root / 'summary'
            result = subprocess.run(
                ['bash', str(SCRIPT)], capture_output=True, text=True, timeout=10,
                env=dict(os.environ, PATH=str(root) + os.pathsep + os.environ['PATH'],
                         SHARED_TRACE=str(trace), SHARED_FAILURES=','.join(failures),
                         SHARED_POOL_CONFLICT=str(int(pool_conflict)),
                         GITHUB_STEP_SUMMARY=str(summary)))
            events = [json.loads(line) for line in trace.read_text().splitlines()]
            self.assertEqual([event[0] for event in events], EXPECTED)
            receipt = summary.read_text()
            self.assertIn(receipt.strip(), result.stdout)
            self.assertEqual(receipt.count('| PASS |'), len(EXPECTED) - len(failures))
            self.assertEqual(receipt.count('| FAIL | 42 |'), len(failures))
            return result, receipt, events

    def test_workspace_and_python_failures_are_both_reported_after_all_suites(self):
        result, receipt, events = self.invoke(['workspace', 'scripts/ci'])
        self.assertEqual(result.returncode, 1)
        self.assertIn('| Workspace Rust tests | FAIL | 42 |', receipt)
        self.assertIn('| CI Python tests | FAIL | 42 |', receipt)
        self.assertIn('| Updater Python tests | PASS | 0 |', receipt)
        self.assertEqual(events[2][1], [
            'scripts/ci/workspace-rust-tests.py', '--shared'])

    def test_log_pool_is_isolated_and_log_failure_still_fails_the_gate(self):
        result, _, events = self.invoke([], pool_conflict=True)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(events[3][1], [
            'test', '--workspace', '--locked', '--no-fail-fast', 'reader_log', '--',
            '--test-threads=1', '--skip', 'platform::'])
        result, receipt, _ = self.invoke(['logs'])
        self.assertEqual(result.returncode, 1)
        self.assertIn('| Log Rust tests | FAIL | 42 |', receipt)
        self.assertIn('| Updater Python tests | PASS | 0 |', receipt)

    def test_lint_failure_does_not_hide_rust_or_python_results(self):
        result, receipt, _ = self.invoke(['fmt', 'clippy'])
        self.assertEqual(result.returncode, 1)
        self.assertIn('| Workspace Rust tests | PASS | 0 |', receipt)
        self.assertIn('| Updater Python tests | PASS | 0 |', receipt)

    def test_all_green_is_successful(self):
        result, _, _ = self.invoke([])
        self.assertEqual(result.returncode, 0)


if __name__ == '__main__':
    unittest.main()
