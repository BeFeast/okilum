import contextlib
import importlib.util
import io
from pathlib import Path
import unittest
import urllib.error

spec = importlib.util.spec_from_file_location('health', Path(__file__).with_name('main-health.py'))
health = importlib.util.module_from_spec(spec)
spec.loader.exec_module(health)


class FakeApi:
    """Statuses per commit, Actions reads (an answer or an exception per path), and a record of writes."""

    def __init__(self, green, authors, actions=None):
        self.green_commits = set(green)
        self.authors = authors
        self.actions = actions or {}
        self.unexpected = []
        self.comments = {}
        self.writes = []

    def green(self, sha):
        return sha in self.green_commits

    def read(self, kind, path):
        # As with the real Api: job logs come through text(), other Actions reads are JSON.
        # red() swallows read errors, so a wrong read is also recorded for the test to see.
        if path not in self.actions or (kind == 'text') != path.endswith('/logs'):
            self.unexpected.append((kind, path))
            raise LookupError(f'unexpected {kind} read of {path}')
        answer = self.actions[path]
        if isinstance(answer, Exception):
            raise answer
        return answer

    def text(self, path):
        return self.read('text', path)

    def call(self, method, path, body=None):
        if method == 'GET' and path.startswith('/actions/'):
            return self.read('json', path)
        if method == 'GET' and path.startswith('/pulls/'):
            return {'user': {'login': self.authors[int(path.split('/')[2])]}}
        if method == 'GET' and path.endswith('/comments?limit=50'):
            return self.comments.get(int(path.split('/')[2]), [])
        self.writes.append((method, path, body))
        if method == 'POST' and path.endswith('/comments'):
            self.comments.setdefault(int(path.split('/')[2]), []).append(body)
        return {}

    def open_issue(self):
        return None


class MainHealthTests(unittest.TestCase):
    def test_lane_success_is_green_and_cancelled_check_is_not(self):
        newest_first = [
            {'context': 'ci / check (push)', 'status': 'failure'},
            {'context': 'ci / linux-local (push)', 'status': 'success'},
            {'context': 'ci / linux-local (push)', 'status': 'pending'},
        ]
        self.assertTrue(health.lane_passed(newest_first))
        cancelled = [{'context': 'ci / linux-local (push)', 'status': 'failure'},
                     {'context': 'ci / check (push)', 'status': 'pending'}]
        self.assertFalse(health.lane_passed(cancelled))
        self.assertFalse(health.lane_passed([]))

    def test_pull_number_from_merge_subject(self):
        subject = "Merge pull request 'fix(x): y (#12)' (#1086) from ci/996-qa-fixes into main"
        self.assertEqual(health.pull_number(subject), 1086)
        self.assertIsNone(health.pull_number('fix: direct push'))

    def test_every_merge_since_green_is_a_suspect_once_per_episode(self):
        history = [('c3', "Merge pull request 'a' (#3) from x into main"),
                   ('c2', 'chore: direct push'),
                   ('c1', "Merge pull request 'b' (#1) from y into main"),
                   ('c0', "Merge pull request 'c' (#9) from z into main")]
        original = health.first_parents, health.tip
        health.first_parents = lambda sha, limit: history
        health.tip = lambda: 'c3'
        try:
            api = FakeApi(green={'c0'}, authors={3: 'ana', 1: 'bo'})
            health.red(api, 'c3', 'run/1')
            health.red(api, 'c3', 'run/2')
        finally:
            health.first_parents, health.tip = original
        pr_comments = [w for w in api.writes if w[1] in ('/issues/3/comments', '/issues/1/comments')]
        self.assertEqual(len(pr_comments), 2, 'one comment per suspect PR per red episode')
        self.assertIn('@ana', api.comments[3][0]['body'])
        self.assertNotIn(9, api.comments, 'the last green merge is not a suspect')
        issue = next(w for w in api.writes if w[1] == '/issues')
        self.assertIn('c2 chore: direct push (direct push)', issue[2]['body'])
        self.assertIn('20 minutes', issue[2]['body'])
        self.assertNotIn('Lane log unreadable', issue[2]['body'], 'no RUN_ID: no log was tried')

    def test_older_green_does_not_close_a_newer_red(self):
        api = FakeApi(green=set(), authors={})
        issue = {'number': 7, 'body': 'The Linux gate failed on main at c5c5c5c5: run'}
        api.open_issue = lambda: issue
        original = health.is_ancestor
        health.is_ancestor = lambda old, new: (old, new) == ('c5c5c5c5', 'c6')
        try:
            health.green(api, 'c4', 'run/older')
            self.assertEqual(api.writes, [], 'c4 does not contain the red c5')
            health.green(api, 'c6', 'run/newer')
        finally:
            health.is_ancestor = original
        self.assertIn(('PATCH', '/issues/7', {'state': 'closed'}), api.writes)

    def test_red_already_fixed_by_a_newer_green_commit_is_not_reported(self):
        history = [('c6', 'm6'), ('c5', 'm5'), ('c4', 'm4')]
        saved = health.first_parents, health.tip, health.is_ancestor
        health.first_parents = lambda sha, limit: history[[c for c, _ in history].index(sha):]
        health.tip = lambda: 'c6'
        health.is_ancestor = lambda old, new: True
        try:
            api = FakeApi(green={'c6'}, authors={})
            health.red(api, 'c5', 'run/late')
            self.assertEqual(api.writes, [], 'c6 contains c5 and is green')
            api = FakeApi(green={'c4'}, authors={})
            health.red(api, 'c5', 'run/real')
            self.assertTrue(any(w[1] == '/issues' for w in api.writes), 'a live red is reported')
        finally:
            health.first_parents, health.tip, health.is_ancestor = saved

    def test_failing_tests_are_named_and_infrastructure_blames_no_merge(self):
        cargo = ('2026-10-10T23:00:00Z    Compiling okilum v0.1.0\n'
                 '2026-10-10T23:02:51Z ---- platform::clip::tests::pipe stdout ----\n'
                 '2026-10-10T23:02:51Z test result: FAILED. 817 passed; 1 failed\n')
        self.assertEqual(health.classify(cargo), ('code', ['platform::clip::tests::pipe']))
        self.assertEqual(health.classify('2026Z error[E0425]: cannot find value\n')[0], 'code')
        self.assertEqual(health.classify('2026Z ⚙️ [runner]: Error response from daemon: no such image\n'),
                         ('infra', 'Error response from daemon'))
        self.assertEqual(health.classify('2026Z Run Main checkout\n2026Z Job failed\n')[0], 'infra')
        self.assertEqual(health.classify('2026Z    Compiling x\n2026Z Job failed\n')[0], 'unknown')
        history = [('c3c3c3c3', "Merge pull request 'a' (#3) from x into main"), ('c0c0c0c0', 'm0')]
        saved = health.first_parents, health.tip, health.lane_log
        health.first_parents = lambda sha, limit: history
        health.tip = lambda: 'c3c3c3c3'
        try:
            health.lane_log = lambda api, run_id: '2026Z Job failed\n'
            api = FakeApi(green={'c0c0c0c0'}, authors={3: 'ana'})
            health.red(api, 'c3c3c3c3', 'run/1', run_id=7)
            self.assertEqual([w[1] for w in api.writes], ['/issues'])
            self.assertIn('looks like infrastructure', api.writes[0][2]['body'])
            # One failing test: the first red waits for confirmation and wakes nobody.
            health.lane_log = lambda api, run_id: cargo
            api = FakeApi(green={'c0c0c0c0'}, authors={3: 'ana'})
            health.red(api, 'c3c3c3c3', 'run/2', run_id=8)
            self.assertEqual([w[1] for w in api.writes], ['/issues'])
            body = api.writes[0][2]['body']
            self.assertIn('`platform::clip::tests::pipe`', body)
            self.assertIn(health.AWAITING, body)
            # The next main commit is red too: now the merges are blamed.
            api.open_issue = lambda: {'number': 5, 'body': body}
            health.red(api, 'c3c3c3c3', 'run/3', run_id=9)
            self.assertIn('/issues/3/comments', [w[1] for w in api.writes])
            # A compile error is deterministic and blames at once.
            health.lane_log = lambda api, run_id: '2026Z    Compiling x\n2026Z error[E0308]: mismatched types\n'
            api = FakeApi(green={'c0c0c0c0'}, authors={3: 'ana'})
            health.red(api, 'c3c3c3c3', 'run/4', run_id=10)
            self.assertIn('/issues/3/comments', [w[1] for w in api.writes])
        finally:
            health.first_parents, health.tip, health.lane_log = saved

    def red_reading(self, actions):
        """red() for c3 (merge #3 by ana; c0 is the last green) with the real lane_log."""
        history = [('c3c3c3c3', "Merge pull request 'a' (#3) from x into main"), ('c0c0c0c0', 'm0')]
        saved = health.first_parents, health.tip
        health.first_parents = lambda sha, limit: history
        health.tip = lambda: 'c3c3c3c3'
        api = FakeApi(green={'c0c0c0c0'}, authors={3: 'ana'}, actions=actions)
        try:
            with contextlib.redirect_stdout(io.StringIO()) as out:
                health.red(api, 'c3c3c3c3', 'run/1', run_id=7)
        finally:
            health.first_parents, health.tip = saved
        self.assertEqual(api.unexpected, [], 'only the configured Actions reads, each of the right kind')
        return api, out.getvalue()

    def assert_merges_blamed(self, api, out, reason):
        self.assertIn('/issues/3/comments', [w[1] for w in api.writes], 'the suspect merge is told')
        body = next(w[2]['body'] for w in api.writes if w[1] == '/issues')
        self.assertNotIn('looks like infrastructure', body)
        self.assertIn('- #3 by @ana (c3c3c3c3)', body)
        self.assertIn(health.RULE, body)
        self.assertIn(f'Lane log unreadable: `{reason}`; classified as unknown.', body)
        self.assertIn(f'Cannot read the lane log: {reason}', out)

    def test_only_a_read_log_can_be_infrastructure(self):
        self.assertEqual(health.classify(None), ('unknown', []), 'not read')
        # Read but empty is still a read log (#1937 keeps the two apart): no build or test output.
        self.assertEqual(health.classify(''), ('infra', 'the job ended before any build or test output'))

    def test_a_forbidden_lane_log_blames_the_merges(self):
        forbidden = urllib.error.HTTPError('https://forgejo/actions/runs/7/jobs', 403, 'Forbidden', {}, None)
        api, out = self.red_reading({'/actions/runs/7/jobs': forbidden})
        self.assert_merges_blamed(api, out, 'HTTPError: HTTP Error 403: Forbidden')

    def test_a_run_without_a_failed_linux_local_job_blames_the_merges(self):
        jobs = {'jobs': [{'name': 'linux', 'status': 'failure', 'id': 1},
                         {'name': 'linux-local', 'status': 'success', 'id': 2}]}
        api, out = self.red_reading({'/actions/runs/7/jobs': jobs})
        self.assert_merges_blamed(api, out, 'LookupError: no failed linux-local job in run 7')

    def test_an_expired_job_log_blames_the_merges(self):
        jobs = {'jobs': [{'name': 'linux-local', 'status': 'failure', 'id': 41}]}
        gone = urllib.error.HTTPError('https://forgejo/actions/jobs/41/logs', 404, 'Not Found', {}, None)
        api, out = self.red_reading({'/actions/runs/7/jobs': jobs, '/actions/jobs/41/logs': gone})
        self.assert_merges_blamed(api, out, 'HTTPError: HTTP Error 404: Not Found')

    def test_a_timed_out_read_keeps_its_reason_visible(self):
        # '<urlopen error timed out>' outside a code span is an HTML tag that Forgejo strips.
        timeout = urllib.error.URLError(TimeoutError('timed out'))
        api, out = self.red_reading({'/actions/runs/7/jobs': timeout})
        self.assert_merges_blamed(api, out, 'URLError: <urlopen error timed out>')

    def test_a_read_log_without_build_output_is_still_infrastructure(self):
        jobs = {'jobs': [{'name': 'linux', 'status': 'failure', 'id': 1},
                         {'name': 'linux-local', 'status': 'failure', 'id': 41}]}
        api, out = self.red_reading({'/actions/runs/7/jobs': jobs, '/actions/jobs/41/logs': '2026Z Job failed\n'})
        self.assertEqual(out, '', 'the log was read')
        self.assertEqual([w[1] for w in api.writes], ['/issues'], 'no merge is blamed')
        self.assertIn('looks like infrastructure', api.writes[0][2]['body'])
        self.assertNotIn('Lane log unreadable', api.writes[0][2]['body'])

    # Lines of saved red-main logs (#1962): each fixture takes a few lines of one run, in their
    # order, long lines shortened. test_github_pr.py passes and prints 'Hosted CI unavailable'
    # in nearly every linux-local run (check-linux.sh runs scripts/ci before cargo test).
    HOSTED_TEST_OUTPUT = ('2026-10-29T10:18:48.4324503Z Hosted CI unavailable; '
                          'no fallback success: Unknown hosted lane\n')  # run 15255

    def red_on_log(self, log):
        """red() for c3 when the failed linux-local job's log is `log`."""
        jobs = {'jobs': [{'name': 'linux-local', 'status': 'failure', 'id': 41}]}
        api, out = self.red_reading({'/actions/runs/7/jobs': jobs, '/actions/jobs/41/logs': log})
        self.assertEqual(out, '', 'the log was read')
        return api, next(w[2]['body'] for w in api.writes if w[1] == '/issues')

    def assert_blamed(self, api, body):
        self.assertIn('/issues/3/comments', [w[1] for w in api.writes], 'the suspect merge is told')
        self.assertNotIn('looks like infrastructure', body)
        self.assertIn('- #3 by @ana (c3c3c3c3)', body)

    # Python unittest headers (#1968), as the shared lane prints them with and without the runner's
    # timestamps. Python 3.11+ repeats the method inside the parentheses.
    PY_FAIL = 'test_patch_numbers.PatchNumberTests.test_every_patch_has_a_unique_four_digit_number'
    PY_ERROR = 'test_sign_code_guard.GuardTests.test_inventory_timeout_at_each_boundary'
    PY_RUNNER_END = ("2026-10-24T18:34:59.3278955Z ⚙️ [runner]: exitcode '1': failure\n"
                     '2026-10-24T18:35:02.0829698Z 🏁  Job failed\n')

    def python_log(self, headers, summary, timestamped=True):
        """A failed scripts/ci run: the hosted test output, the unittest headers, then the summary."""
        lines = ['Hosted CI unavailable; no fallback success: Unknown hosted lane']
        for header in headers:
            lines += ['=' * 70, header, '-' * 70, 'Traceback (most recent call last):',
                      '  File "x.py", line 1, in test', 'AssertionError: synthetic']
        lines += ['-' * 70, 'Ran 126 tests in 18.918s', '', summary]
        stamp = '2026-10-24T18:34:57.6438674Z ' if timestamped else ''
        return ''.join(f'{stamp}{line}\n' for line in lines) + self.PY_RUNNER_END

    def assert_awaiting(self, api, body, name):
        self.assertEqual([w[1] for w in api.writes], ['/issues'], 'no merge is blamed yet')
        self.assertNotIn('looks like infrastructure', body)
        self.assertIn(f'Failing tests:\n- `{name}`\n\n', body)
        self.assertIn(health.AWAITING, body)

    def test_a_python_unittest_failure_names_its_test_and_awaits_confirmation(self):
        # Run 11737 (shared lane, the same scripts/ci suite): the hosted test output, then a failure.
        log = ('2026-10-24T18:34:57.6383365Z Hosted CI unavailable; no fallback success: Unknown hosted lane\n'
               '2026-10-24T18:34:57.6438674Z FAIL: test_every_patch_has_a_unique_four_digit_number '
               '(test_patch_numbers.PatchNumberTests.test_every_patch_has_a_unique_four_digit_number)\n'
               '2026-10-24T18:34:57.6439652Z Ran 126 tests in 18.918s\n'
               '2026-10-24T18:34:57.6439863Z FAILED (failures=1)\n'
               "2026-10-24T18:34:59.3278955Z ⚙️ [runner]: exitcode '1': failure\n"
               '2026-10-24T18:35:02.0829698Z 🏁  Job failed\n')
        self.assertEqual(health.classify(log), ('code', [self.PY_FAIL]))
        self.assert_awaiting(*self.red_on_log(log), self.PY_FAIL)
        plain = self.python_log([f'FAIL: test_every_patch_has_a_unique_four_digit_number ({self.PY_FAIL})'],
                                'FAILED (failures=1)', timestamped=False)
        self.assertEqual(health.classify(plain), ('code', [self.PY_FAIL]))
        self.assert_awaiting(*self.red_on_log(plain), self.PY_FAIL)

    def test_a_python_test_error_names_its_test_and_awaits_confirmation(self):
        # Run 10711: a test of scripts/windows timed out (an error, not a failure).
        log = ('2026-10-23T08:53:33.2590259Z ERROR: test_inventory_timeout_at_each_boundary '
               '(test_sign_code_guard.GuardTests.test_inventory_timeout_at_each_boundary)\n'
               '2026-10-23T08:53:33.2592724Z Ran 45 tests in 17.267s\n'
               '2026-10-23T08:53:33.2592982Z FAILED (errors=1)\n'
               "2026-10-23T08:54:26.8027996Z ⚙️ [runner]: exitcode '1': failure\n"
               '2026-10-23T08:54:29.4699068Z 🏁  Job failed\n')
        self.assertEqual(health.classify(log), ('code', [self.PY_ERROR]))
        self.assert_awaiting(*self.red_on_log(log), self.PY_ERROR)
        plain = self.python_log([f'ERROR: test_inventory_timeout_at_each_boundary ({self.PY_ERROR})'],
                                'FAILED (errors=1)', timestamped=False)
        self.assertEqual(health.classify(plain), ('code', [self.PY_ERROR]))
        self.assert_awaiting(*self.red_on_log(plain), self.PY_ERROR)

    def test_two_python_failures_blame_the_merges_at_once(self):
        log = self.python_log([f'ERROR: test_inventory_timeout_at_each_boundary ({self.PY_ERROR})',
                               f'FAIL: test_every_patch_has_a_unique_four_digit_number ({self.PY_FAIL})'],
                              'FAILED (failures=1, errors=1)')
        self.assertEqual(health.classify(log), ('code', [self.PY_ERROR, self.PY_FAIL]), 'in log order')
        api, body = self.red_on_log(log)
        self.assert_blamed(api, body)
        self.assertNotIn(health.AWAITING, body)
        self.assertIn(f'Failing tests:\n- `{self.PY_ERROR}`\n- `{self.PY_FAIL}`\n\n', body)

    def test_a_repeated_python_header_is_one_failure(self):
        # Each failing subTest prints its own header for the same test; Python before 3.11 names
        # only the class inside the parentheses.
        headers = [f'FAIL: test_every_patch_has_a_unique_four_digit_number ({self.PY_FAIL}) (number=1)',
                   f'FAIL: test_every_patch_has_a_unique_four_digit_number ({self.PY_FAIL}) (number=2)',
                   'FAIL: test_every_patch_has_a_unique_four_digit_number (test_patch_numbers.PatchNumberTests)']
        log = self.python_log(headers, 'FAILED (failures=3)')
        self.assertEqual(health.classify(log), ('code', [self.PY_FAIL]))
        self.assert_awaiting(*self.red_on_log(log), self.PY_FAIL)

    def test_an_unnamed_python_failure_summary_blames_the_merges(self):
        log = self.python_log([], 'FAILED (errors=1)')
        self.assertEqual(health.classify(log), ('code', []))
        api, body = self.red_on_log(log)
        self.assert_blamed(api, body)
        self.assertNotIn(health.AWAITING, body)
        self.assertNotIn('Failing tests:', body)

    def test_a_passing_python_run_names_no_failure(self):
        # A passing suite may print header-shaped output; without a FAILED summary it names nothing.
        log = (self.python_log([f'FAIL: test_every_patch_has_a_unique_four_digit_number ({self.PY_FAIL})'], 'OK')
               .replace(self.PY_RUNNER_END, '') +
               '2026-10-24T18:35:00.0000000Z    Compiling okilum v0.1.0\n' + self.PY_RUNNER_END)
        self.assertEqual(health.classify(log), ('unknown', []))
        api, body = self.red_on_log(log)
        self.assert_blamed(api, body)
        self.assertNotIn('Failing tests:', body)
        # The same suite failing is the positive control: the header is found.
        self.assertEqual(health.classify(log.replace('Z OK\n', 'Z FAILED (failures=1)\n')),
                         ('code', [self.PY_FAIL]))

    def test_a_log_cut_off_mid_compile_is_not_infrastructure(self):
        # Run 7379 stops while compiling, with no runner line: nothing says why it ended.
        log = ('2026-10-12T12:12:22.1383254Z Hosted CI unavailable; no fallback success: Unknown hosted lane\n'
               '2026-10-12T12:21:37.1393568Z    Compiling enum-iterator v2.3.0\n'
               '2026-10-12T12:21:37.3137073Z    Compiling itertools v0.13.0\n')
        self.assertEqual(health.classify(log), ('unknown', []))
        self.assert_blamed(*self.red_on_log(log))

    def test_the_runner_line_names_the_infrastructure(self):
        # Run 10907 (shared lane): out of time while still making progress; every test that
        # cargo called slow finished, so the host was slow.
        log = ('2026-10-23T15:15:17.4195730Z test file_editor::tests::destination_reservation_protects_'
               'orphaned_drafts_and_active_writers has been running for over 60 seconds\n'
               '2026-10-23T15:16:58.3124316Z test file_editor::tests::destination_reservation_protects_'
               'orphaned_drafts_and_active_writers ... ok\n'
               '2026-10-23T15:16:58.7151823Z      Running tests/desktop_editor.rs '
               '(target/debug/deps/desktop_editor-f13e251d373dbd2a)\n'
               '2026-10-23T15:17:01.7730118Z test windows_vault_paths_nested_creation_templates_and_identity ... ok\n'
               '2026-10-23T15:17:05.6098885Z ⚙️ [runner]: context deadline exceeded\\nunable to copy '
               '"/var/run/act/workflow/SUMMARY.md" from container to host: context deadline exceeded\n'
               '2026-10-23T15:17:08.3923882Z 🏁  Job failed\n')
        self.assertEqual(health.classify(log), ('infra', 'context deadline exceeded'))
        api, body = self.red_on_log(log)
        self.assertEqual([w[1] for w in api.writes], ['/issues'], 'no merge is blamed')
        self.assertIn('**This looks like infrastructure, not a merge**: context deadline exceeded.', body)
        # The same words printed by a step, or a step echoing the runner's line, are output.
        self.assertEqual(health.classify(log.replace('⚙️ [runner]: ', '')), ('unknown', []))
        self.assertEqual(health.classify(log.replace('⚙️ [runner]: ', '  > ⚙️ [runner]: ')), ('unknown', []))
        self.assertEqual(health.classify(log.replace('⚙️ [runner]: ', '[runner]: ')), ('unknown', []))
        # act's cleanup error is the runner's too: the only line run 14845 has (no container).
        # The cause is in "(original error: ...)".
        cleanup = ('2026-10-29T00:14:24.6053384Z Error occurred running finally: All attempts fail:\\n#1: Error '
                   'response from daemon: remove c9009af5ff4d-env: volume is in use (original error: failed to '
                   'create container: Post "http://%2Fvar%2Frun%2Fdocker.sock/v1.55/containers/create")\n')
        self.assertEqual(health.classify(cleanup), ('infra', 'failed to create container'))

    def test_a_test_still_running_at_the_time_limit_hung(self):
        # Run 10704: one test never finished; 38 minutes of silence until the job's 80 minutes ran out.
        hung = 'platform::reveal::tests::show_items_selects_the_item_and_fails_without_a_file_manager'
        log = ('2026-10-23T08:44:04.8550919Z Hosted CI unavailable; no fallback success: Unknown hosted lane\n'
               '2026-10-23T08:56:34.9206331Z test file_editor::tests::completed_exchange_before_journal_'
               'acknowledgement_recovers_without_conflict has been running for over 60 seconds\n'
               '2026-10-23T09:01:17.3720824Z test file_editor::tests::completed_exchange_before_journal_'
               'acknowledgement_recovers_without_conflict ... ok\n'
               '2026-10-23T09:13:22.4042256Z      Running unittests src/main.rs '
               '(target/debug/deps/okilum-2092075bda669346)\n'
               f'2026-10-23T09:15:32.0294418Z test {hung} has been running for over 60 seconds\n'
               '2026-10-23T09:19:24.5656896Z test workspace::tests::local_entry_native_pickers_open_exact_'
               'results_and_cancel_neutrally ... ok\n'
               '2026-10-23T09:57:07.5539393Z ⚙️ [runner]: context deadline exceeded\\nunable to copy '
               '"/var/run/act/workflow/SUMMARY.md" from container to host: context deadline exceeded\n'
               '2026-10-23T09:57:07.7051936Z 🏁  Job failed\n')
        self.assertEqual(health.classify(log), ('code', [hung]))
        # One hung test is how a flake looks: the issue names it and waits for a second red.
        api, body = self.red_on_log(log)
        self.assertEqual([w[1] for w in api.writes], ['/issues'])
        self.assertNotIn('looks like infrastructure', body)
        self.assertIn(f'Failing tests:\n- `{hung}`', body)
        self.assertIn(health.AWAITING, body)

    def test_a_cargo_failure_after_the_hosted_test_output_names_its_tests(self):
        # Reverse control from run 15255: the 32 saved cargo reds must keep their tests.
        names = ['reader_mermaid::render::tests::tall_td_and_tb_render_with_bounded_rasters_and_full_logical_height',
                 'reader_mermaid::render::tests::synthetic_families_paint_both_themes_and_invalid_is_an_error']
        log = (self.HOSTED_TEST_OUTPUT +
               '2026-10-29T10:54:04.8015865Z failures:\n'
               f'2026-10-29T10:54:04.8016068Z ---- {names[0]} stdout ----\n'
               '2026-10-29T10:54:04.8016471Z called `Result::unwrap()` on an `Err` value: "operation cancelled"\n'
               f'2026-10-29T10:54:04.8016673Z ---- {names[1]} stdout ----\n'
               '2026-10-29T10:54:04.8016980Z synthetic renderer panic\n'
               '2026-10-29T10:54:04.8018075Z test result: FAILED. 1393 passed; 2 failed; 9 ignored; 0 measured\n'
               "2026-10-29T10:54:05.5062550Z ⚙️ [runner]: exitcode '101': failure\n"
               '2026-10-29T10:54:09.7440131Z 🏁  Job failed\n')
        self.assertEqual(health.classify(log), ('code', names))
        api, body = self.red_on_log(log)
        self.assert_blamed(api, body)
        self.assertIn(''.join(f'\n- `{name}`' for name in names), body)


if __name__ == '__main__':
    unittest.main()
