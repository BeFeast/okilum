import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('ci_logs', Path(__file__).with_name('ci-logs.py'))
logs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(logs)


class SummaryTests(unittest.TestCase):
    def test_summary_points_at_the_real_failure(self):
        cargo = ('2026Z    Compiling okilum\n2026Z ---- brain::a::tests::b stdout ----\n'
                 '2026Z test result: FAILED. 1 failed\n')
        self.assertEqual(logs.summary(cargo, 'ci.yml', 'linux-local'), 'tests: brain::a::tests::b')
        self.assertTrue(logs.summary('2026Z error[E0308]: mismatched types\n').startswith('error[E0308]'))
        self.assertTrue(logs.summary('', 'ci.yml', 'check').startswith('aggregate'))
        self.assertTrue(logs.summary('2026Z see https://github.com/BeFeast/okilum/actions/runs/1\n').startswith('hosted lane'))
        self.assertIn('infrastructure', logs.summary('2026Z Job failed\n'))

    # test_github_pr.py passes and prints this in nearly every linux-local run (#1962).
    HOSTED_TEST_OUTPUT = '2026-10-29T10:18:48.4324503Z Hosted CI unavailable; no fallback success: Unknown hosted lane\n'
    RUNNER_EXIT = ("2026-10-24T18:34:59.3278955Z ⚙️ [runner]: exitcode '1': failure\n"
                   '2026-10-24T18:35:02.0829698Z 🏁  Job failed\n')

    def test_a_passing_test_echo_does_not_hide_a_python_failure(self):
        log = (self.HOSTED_TEST_OUTPUT +
               '2026-10-24T18:34:57.6438674Z FAIL: test_every_patch_has_a_unique_four_digit_number '
               '(test_patch_numbers.PatchNumberTests.test_every_patch_has_a_unique_four_digit_number)\n'
               '2026-10-24T18:34:57.6439000Z ERROR: test_inventory_timeout (test_sign_code_guard.GuardTests)\n'
               '2026-10-24T18:34:57.6439652Z Ran 126 tests in 18.918s\n'
               '2026-10-24T18:34:57.6439863Z FAILED (failures=1, errors=1)\n' + self.RUNNER_EXIT)
        self.assertEqual(logs.summary(log, 'ci.yml', 'linux-local'),
                         'tests: test_patch_numbers.PatchNumberTests.test_every_patch_has_a_unique_four_digit_number '
                         'test_sign_code_guard.GuardTests.test_inventory_timeout')
        # Without the per-test lines, the unittest summary line still says it was the tests.
        bare = self.HOSTED_TEST_OUTPUT + '2026Z Ran 45 tests in 17.267s\n2026Z FAILED (errors=1)\n' + self.RUNNER_EXIT
        self.assertEqual(logs.summary(bare, 'ci.yml', 'linux-local'), 'tests: FAILED (errors=1)')
        # Positive control: the echo alone, in a passing suite, is not a test failure either.
        passing = self.HOSTED_TEST_OUTPUT + '2026Z Ran 126 tests in 18.918s\n2026Z OK\n' + self.RUNNER_EXIT
        self.assertNotIn('tests:', logs.summary(passing, 'ci.yml', 'linux-local'))
        self.assertNotIn('Hosted CI unavailable', logs.summary(passing, 'ci.yml', 'linux-local'))

    def test_a_log_cut_off_mid_compile_with_the_echo_is_not_infrastructure(self):
        log = (self.HOSTED_TEST_OUTPUT +
               '2026-10-12T12:21:37.1393568Z    Compiling enum-iterator v2.3.0\n'
               '2026-10-12T12:21:37.3137073Z    Compiling itertools v0.13.0\n')
        self.assertEqual(logs.summary(log, 'ci.yml', 'linux-local'), '-')

    def test_a_runner_line_names_the_infrastructure(self):
        log = (self.HOSTED_TEST_OUTPUT +
               '2026-10-23T15:16:58.7151823Z      Running tests/desktop_editor.rs\n'
               '2026-10-23T15:17:05.6098885Z ⚙️ [runner]: failed to create container: '
               'Cannot connect to the Docker daemon at unix:///var/run/docker.sock\n'
               '2026-10-23T15:17:08.3923882Z 🏁  Job failed\n')
        self.assertEqual(logs.summary(log, 'ci.yml', 'linux-local'),
                         'infrastructure: Cannot connect to the Docker daemon')
        # The same words printed by a step, or a step echoing the runner's line, are output.
        for echo in ('', '  > ⚙️ [runner]: ', '[runner]: '):
            self.assertEqual(logs.summary(log.replace('⚙️ [runner]: ', echo), 'ci.yml', 'linux-local'), '-')
        # act's cleanup error is the runner's too (no container was ever created).
        cleanup = ('2026-10-29T00:14:24.6053384Z Error occurred running finally: All attempts fail:\\n#1: Error '
                   'response from daemon: remove c9009af5ff4d-env: volume is in use (original error: failed to '
                   'create container: Post "http://%2Fvar%2Frun%2Fdocker.sock/v1.55/containers/create")\n')
        self.assertEqual(logs.summary(cleanup), 'infrastructure: failed to create container')
        # Out of time: slow host when every slow test finished, a hung test when one did not.
        slow = ('2026Z test a::b has been running for over 60 seconds\n2026Z test a::b ... ok\n'
                '2026Z      Running tests/x.rs\n'
                '2026Z ⚙️ [runner]: context deadline exceeded\\nunable to copy SUMMARY.md\n')
        self.assertEqual(logs.summary(slow), 'infrastructure: context deadline exceeded')
        hung = slow.replace('2026Z test a::b ... ok\n', '')
        self.assertEqual(logs.summary(hung), 'tests (hung): a::b')

    def test_the_echo_keeps_the_earlier_precedence(self):
        echo, runner = self.HOSTED_TEST_OUTPUT, '2026Z ⚙️ [runner]: No space left on device\n'
        cargo = '2026Z    Compiling okilum\n2026Z ---- brain::a::tests::b stdout ----\n'
        self.assertEqual(logs.summary(echo + cargo + runner, 'ci.yml', 'linux-local'), 'tests: brain::a::tests::b')
        compile_error = '2026Z error[E0308]: mismatched types\n2026Z FAILED (failures=1)\n'
        self.assertTrue(logs.summary(echo + compile_error + runner).startswith('error[E0308]'))
        self.assertTrue(logs.summary(echo + runner, 'ci.yml', 'check').startswith('aggregate'))
        github = '2026Z see https://github.com/BeFeast/okilum/actions/runs/1\n'
        self.assertTrue(logs.summary(echo + github + runner).startswith('hosted lane'))
        self.assertEqual(logs.summary(echo + runner), 'infrastructure: No space left on device')

    def test_web_run_number_is_resolved_to_the_api_id(self):
        pages = {1: [{'index_in_repo': 6123, 'id': 10902}, {'index_in_repo': 6122, 'id': 10901}]}
        logs.RUN_IDS.clear()
        original = logs.fetch
        logs.fetch = lambda path, raw=False: pages.get(int(path.split('page=')[1]), [])
        try:
            self.assertEqual(logs.run_id(6123)[0], 10902)
            with self.assertRaises(LookupError):
                logs.run_id(5)
        finally:
            logs.fetch = original
            logs.RUN_IDS.clear()


if __name__ == '__main__':
    unittest.main()
