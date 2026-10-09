import copy
import re
import http.client
import socket
from unittest.mock import MagicMock, patch
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('bridge', Path(__file__).with_name('github-macos.py'))
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)
SHA = 'a' * 40
BRANCH = bridge.ref_name('564', SHA, '123', '1')
RUN = dict(id=1, head_sha=SHA, head_branch=BRANCH, event='push', path=bridge.WORKFLOW,
           status='completed', conclusion='success', html_url='https://github.com/run/1')
JOB = dict(name='macos', conclusion='success', steps=[
    dict(name=bridge.BUILD_STEP, conclusion='success')])


class API:
    def __init__(self, runs=None, jobs=None):
        self.runs = [copy.deepcopy(RUN)] if runs is None else runs
        self.jobs = [copy.deepcopy(JOB)] if jobs is None else jobs
        self.calls = []

    def request(self, path, method='GET', data=None):
        self.calls.append((path, method))
        if method == 'POST':
            return None
        if '/jobs?' in path:
            return {'jobs': self.jobs}
        return {'workflow_runs': self.runs}


class Clock:
    now = 0

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class BridgeTests(unittest.TestCase):
    def wait(self, api):
        clock = Clock()
        return bridge.wait_for_run(api, BRANCH, SHA, clock=clock, sleep=clock.sleep,
                                   queue_timeout=30, run_timeout=60)[0]

    def test_hosted_reads_retry_with_bounded_backoff(self):
        class Flaky(API):
            remaining = 2
            def request(self, path, method='GET', data=None):
                if self.remaining:
                    self.remaining -= 1
                    raise bridge.Unavailable('temporary outage')
                return super().request(path, method, data)
        delays = []
        self.assertEqual(bridge.hosted_request(Flaky(), 'actions/runs',
                         sleep=delays.append)['workflow_runs'][0]['id'], 1)
        self.assertEqual(delays, [5, 10])
        class Offline(API):
            def request(self, *args):
                raise bridge.Unavailable('offline')
        delays = []
        with self.assertRaises(bridge.Unavailable):
            bridge.hosted_request(Offline(), 'actions/runs', sleep=delays.append)
        self.assertEqual(delays, [5, 10])

    def test_exact_run_and_executed_job_pass(self):
        self.assertEqual(self.wait(API()), 'success')

    def test_wrong_sha_branch_workflow_or_event_cannot_pass(self):
        for field in ['head_sha', 'head_branch', 'path', 'event']:
            run = dict(RUN, **{field: 'other'})
            with self.subTest(field=field), self.assertRaises(bridge.Unavailable):
                self.wait(API(runs=[run]))

    def test_failed_cancelled_skipped_neutral_or_timed_out_run_fails(self):
        for result in ['failure', 'cancelled', 'skipped', 'neutral', 'timed_out', 'action_required']:
            with self.subTest(result=result):
                self.assertEqual(self.wait(API(runs=[dict(RUN, conclusion=result)])), 'failure')

    def test_green_run_without_real_native_step_fails(self):
        for jobs in [[], [dict(JOB, conclusion='skipped')],
                     [dict(JOB, steps=[])], [dict(JOB, steps=[dict(name=bridge.BUILD_STEP,
                                                               conclusion='skipped')])]]:
            with self.subTest(jobs=jobs):
                self.assertEqual(self.wait(API(jobs=jobs)), 'failure')

    def test_no_runner_falls_back_but_hung_execution_fails(self):
        queued = API(runs=[dict(RUN, status='queued', conclusion=None)])
        with self.assertRaises(bridge.Unavailable):
            self.wait(queued)
        self.assertIn(('actions/runs/1/cancel', 'POST'), queued.calls)
        running = API(runs=[dict(RUN, status='in_progress', conclusion=None)])
        self.assertEqual(self.wait(running), 'failure')
        self.assertIn(('actions/runs/1/cancel', 'POST'), running.calls)

    def test_failed_cancel_cannot_mask_execution_timeout(self):
        class CannotCancel(API):
            def request(self, path, method='GET', data=None):
                if method == 'POST':
                    raise bridge.Unavailable('cancel unavailable')
                return super().request(path, method, data)
        self.assertEqual(self.wait(CannotCancel(
            runs=[dict(RUN, status='in_progress', conclusion=None)])), 'failure')

    def test_default_queue_budget_allows_start_after_old_eight_minute_limit(self):
        clock = Clock()
        class SlowQueue(API):
            def request(self, path, method='GET', data=None):
                self.runs = [dict(RUN, status='queued', conclusion=None)] if clock() < 600 else [RUN]
                return super().request(path, method, data)
        api = SlowQueue()
        result, _ = bridge.wait_for_run(api, BRANCH, SHA, clock=clock, sleep=clock.sleep)
        self.assertEqual(result, 'success')
        self.assertEqual(clock(), 600)
        self.assertFalse(any(method == 'POST' for _, method in api.calls))

    def test_long_github_queue_is_waiting_not_failure(self):
        # 2026-10-09: GitHub kept macOS jobs queued for over an hour; the old
        # 30 min budget turned that into a red ci / check without any step.
        clock = Clock()
        lines = []
        class LongQueue(API):
            def request(self, path, method='GET', data=None):
                self.runs = [dict(RUN, status='queued', conclusion=None)] if clock() < 2 * 3600 else [RUN]
                return super().request(path, method, data)
        api = LongQueue()
        result, _ = bridge.wait_for_run(api, BRANCH, SHA, clock=clock, sleep=clock.sleep,
                                       log=lambda line, **_: lines.append(line))
        self.assertEqual(result, 'success')
        self.assertFalse(any(method == 'POST' for _, method in api.calls))
        self.assertTrue(any('Waiting for a GitHub macOS runner, queued 5 min' in line for line in lines))
        self.assertGreaterEqual(len(lines), 20)
        # The Forgejo job budget covers the full queue plus execution budget.
        workflow = (Path(__file__).resolve().parents[2] / '.forgejo/workflows/ci.yml').read_text()
        minutes = int(re.search(r'macos-github:.*?timeout-minutes: (\d+)', workflow, re.S)[1])
        self.assertGreater(minutes * 60, bridge.QUEUE_TIMEOUT + bridge.RUN_TIMEOUT)

    def test_never_started_run_says_so(self):
        clock = Clock()
        class Stuck(API):
            def request(self, path, method='GET', data=None):
                if method == 'POST':
                    raise bridge.Unavailable('cancel refused')
                return super().request(path, method, data)
        result, message = bridge.wait_for_run(Stuck(runs=[dict(RUN, status='queued', conclusion=None)]),
                                              BRANCH, SHA, clock=clock, sleep=clock.sleep,
                                              queue_timeout=30, run_timeout=60, log=lambda *a, **k: None)
        self.assertEqual(result, 'failure')
        self.assertIn('did not start', message)

    def test_queue_cancel_refusal_waits_for_exact_run_result(self):
        for conclusion, expected in [('success', 'success'), ('failure', 'failure')]:
            clock = Clock()
            class CancelRace(API):
                def request(self, path, method='GET', data=None):
                    if method == 'POST':
                        self.calls.append((path, method))
                        raise bridge.Unavailable('cancel refused')
                    status = 'queued' if clock() < 60 else 'in_progress' if clock() < 80 else 'completed'
                    self.runs = [dict(RUN, status=status, conclusion=conclusion if status == 'completed' else None)]
                    return super().request(path, method, data)
            api = CancelRace()
            result, _ = bridge.wait_for_run(api, BRANCH, SHA, clock=clock, sleep=clock.sleep,
                                           queue_timeout=30, run_timeout=60)
            self.assertEqual(result, expected)
            self.assertEqual(sum(method == 'POST' for _, method in api.calls), 1)

    def test_refused_queue_cancel_has_bounded_fail_closed_deadline(self):
        clock = Clock()
        class Stuck(API):
            def request(self, path, method='GET', data=None):
                if method == 'POST':
                    raise bridge.Unavailable('cancel refused')
                return super().request(path, method, data)
        result, _ = bridge.wait_for_run(Stuck(runs=[dict(RUN, status='queued', conclusion=None)]),
                                       BRANCH, SHA, clock=clock, sleep=clock.sleep,
                                       queue_timeout=30, run_timeout=60)
        self.assertEqual(result, 'failure')
        self.assertLessEqual(clock(), 110)

    def test_missing_run_during_cancel_grace_still_has_deadline(self):
        clock = Clock()
        class Disappearing(API):
            def request(self, path, method='GET', data=None):
                if method == 'POST':
                    raise bridge.Unavailable('cancel refused')
                self.runs = [dict(RUN, status='queued', conclusion=None)] if clock() <= 40 else []
                # Guard the regression itself from hanging on a broken implementation.
                if clock() > 120:
                    raise AssertionError('Grace period lost its deadline')
                return super().request(path, method, data)
        result, _ = bridge.wait_for_run(Disappearing(), BRANCH, SHA, clock=clock,
                                       sleep=clock.sleep, queue_timeout=30, run_timeout=60)
        self.assertEqual(result, 'failure')
        self.assertLessEqual(clock(), 110)

    def test_startup_failure_and_api_outage_allow_fallback(self):
        with self.assertRaises(bridge.Unavailable):
            self.wait(API(runs=[dict(RUN, conclusion='startup_failure')]))
        class Offline(API):
            def request(self, *args):
                raise bridge.Unavailable('offline')
        with self.assertRaises(bridge.Unavailable):
            self.wait(Offline())

    def test_process_publishes_fallback_only_for_unavailability(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'outputs'
            env = dict(os.environ, GITHUB_OUTPUT=str(output), MIRROR_TOKEN='')
            command = [sys.executable, str(Path(__file__).with_name('github-macos.py')), 'run']
            result = subprocess.run(command, env=env, capture_output=True)
            self.assertEqual(result.returncode, 0)
            self.assertEqual(output.read_text(), 'result=unavailable\n')
            output.unlink()
            env.update(MIRROR_TOKEN='test-not-a-real-token', PR_HEAD_SHA='invalid',
                       PR_NUMBER='564', GITHUB_RUN_ID='123')
            result = subprocess.run(command, env=env, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(output.read_text(), 'result=failure\n')
            self.assertNotIn(b'test-not-a-real-token', result.stdout + result.stderr)

    def test_ref_is_unique_and_rejects_injection(self):
        self.assertNotEqual(BRANCH, bridge.ref_name('564', SHA, '123', '2'))
        for args in [('564; echo bad', SHA, '123', '1'), ('564', 'HEAD', '123', '1'),
                     ('564', SHA, '../x', '1')]:
            with self.assertRaises(ValueError):
                bridge.ref_name(*args)

    def test_repeated_forgejo_attempt_has_isolated_github_ref(self):
        first = bridge.ref_name('564', SHA, '123', '1', '1' * 32)
        second = bridge.ref_name('564', SHA, '123', '1', '2' * 32)
        self.assertNotEqual(first, second)
        api = API(runs=[dict(RUN, head_branch=first, id=10),
                        dict(RUN, head_branch=first, id=11),
                        dict(RUN, head_branch=second, id=12)])
        clock = Clock()
        result, _ = bridge.wait_for_run(api, second, SHA,
                                        clock=clock, sleep=clock.sleep)
        self.assertEqual(result, 'success')
        self.assertIn(('actions/runs/12/jobs?per_page=100', 'GET'), api.calls)
        with self.assertRaises(ValueError):
            bridge.ref_name('564', SHA, '123', '1', '../bad')

    def test_duplicate_runs_for_same_invocation_still_fail_closed(self):
        self.assertEqual(self.wait(API(runs=[RUN, dict(RUN, id=2)])), 'failure')

    def test_cleanup_cannot_delete_main_tags_or_another_pr(self):
        class Refs(API):
            def request(self, path, method='GET', data=None):
                self.calls.append((path, method))
                if method == 'GET':
                    return [{'ref': ref} for ref in ['refs/heads/main', 'refs/tags/beta',
                            'refs/heads/forgejo-pr/5640/head', 'refs/heads/' + BRANCH]]
        api = Refs()
        bridge.cleanup(api, '564')
        self.assertEqual([call for call in api.calls if call[1] == 'DELETE'],
                         [('git/refs/heads/' + BRANCH, 'DELETE')])

    def test_forgejo_gate_result_matrix(self):
        workflow = Path(__file__).resolve().parents[2] / '.forgejo/workflows/ci.yml'
        block = workflow.read_text().split('      - name: Require a native result\n', 1)[1]
        block = block.split('  # Keep the existing protected-branch context.', 1)[0]
        script = '\n'.join(line[10:] for line in block.split('        run: |\n', 1)[1].splitlines())
        import itertools
        for lane, required, hosted_job, hosted, local, linux in itertools.product(
                ['hosted', 'local', 'invalid'], ['true', 'false', ''],
                ['success', 'failure', 'cancelled', 'skipped'],
                ['success', 'failure', 'unavailable', ''],
                ['success', 'failure', 'cancelled', 'skipped'], ['success', 'failure']):
            expected = linux == 'success' and (required == 'false' or
                (required == 'true' and (
                    (hosted_job == 'success' and
                     hosted == 'success' and local == 'skipped') or
                    (hosted_job == 'skipped' and local == 'success'))))
            result = subprocess.run(['bash', '-c', script], capture_output=True,
                env=dict(os.environ, SCOPE_RESULT="success", LINUX_RESULT=linux, MACOS_REQUIRED=required,
                         MACOS_LANE=lane, HOSTED_JOB=hosted_job,
                         HOSTED_RESULT=hosted, LOCAL_RESULT=local))
            self.assertEqual(result.returncode == 0, expected,
                             (lane, required, hosted_job, hosted, local, linux))



if __name__ == '__main__':
    unittest.main()


class TransportRetryTests(unittest.TestCase):
    def test_socket_errors_retry_real_request_and_recover(self):
        errors = [http.client.RemoteDisconnected('sensitive reason'),
                  ConnectionResetError('sensitive reason'),
                  socket.timeout('sensitive reason'),
                  bridge.urllib.error.URLError(socket.timeout('sensitive reason'))]
        for error in errors:
            with self.subTest(error=type(error).__name__):
                response = MagicMock()
                response.__enter__.return_value.read.return_value = b'{"workflow_runs": []}'
                delays = []
                with patch.object(bridge.urllib.request, 'urlopen',
                                  side_effect=[error, response]) as request:
                    self.assertEqual(bridge.hosted_request(bridge.GitHub('secret'),
                                     'actions/runs', sleep=delays.append), {'workflow_runs': []})
                self.assertEqual(request.call_count, 2)
                self.assertEqual(delays, [5])

    def test_disconnect_while_reading_exhausts_existing_retry_budget(self):
        response = MagicMock()
        response.__enter__.return_value.read.side_effect = http.client.RemoteDisconnected('secret')
        delays = []
        with patch.object(bridge.urllib.request, 'urlopen', return_value=response) as request:
            with self.assertRaises(bridge.Unavailable) as raised:
                bridge.hosted_request(bridge.GitHub('secret'), 'actions/runs', sleep=delays.append)
        self.assertEqual(request.call_count, 3)
        self.assertEqual(delays, [5, 10])
        self.assertNotIn('secret', str(raised.exception))
