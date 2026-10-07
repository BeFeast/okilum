import copy
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
                    (lane == 'hosted' and hosted_job == 'success' and
                     hosted == 'success' and local == 'skipped') or
                    (lane == 'local' and hosted_job == 'skipped' and local == 'success'))))
            result = subprocess.run(['bash', '-c', script], capture_output=True,
                env=dict(os.environ, LINUX_RESULT=linux, MACOS_REQUIRED=required,
                         MACOS_LANE=lane, HOSTED_JOB=hosted_job,
                         HOSTED_RESULT=hosted, LOCAL_RESULT=local))
            self.assertEqual(result.returncode == 0, expected,
                             (lane, required, hosted_job, hosted, local, linux))



if __name__ == '__main__':
    unittest.main()
