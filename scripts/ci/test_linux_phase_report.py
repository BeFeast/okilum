"""Offline phase reports from the saved full-gate-success/production-failure run."""
import copy
from contextlib import redirect_stdout
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location('phase_bridge', Path(__file__).with_name('github-linux.py'))
linux = importlib.util.module_from_spec(spec)
spec.loader.exec_module(linux)
FIXTURE = json.loads((Path(__file__).with_name('fixtures') /
                      'linux-producer-production-failure.json').read_text())


class API:
    def __init__(self, run, jobs):
        self.run, self.jobs = run, jobs
        self.calls = []

    def request(self, path, method='GET', data=None):
        self.calls.append((method, path))
        if method == 'DELETE' and path == 'git/refs/heads/' + self.run['head_branch']:
            return None  # In-memory cleanup only.
        if method != 'GET':
            raise AssertionError('Unexpected mutation')
        if path.startswith('actions/runs?'):
            return {'workflow_runs': [self.run]}
        exact = f"actions/runs/{self.run['id']}/attempts/{self.run['run_attempt']}/jobs?per_page=100&page=1"
        legacy = f"actions/runs/{self.run['id']}/jobs?per_page=100"
        if path in (exact, legacy):
            return {'jobs': self.jobs}
        raise AssertionError('Unexpected endpoint')


class LinuxPhaseReportTests(unittest.TestCase):
    def setUp(self):
        self.run = copy.deepcopy(FIXTURE['run'])
        self.job = copy.deepcopy(FIXTURE['jobs'][0])
        self.api = API(self.run, [self.job])
        self.sha = self.run['head_sha']
        self.branch = self.run['head_branch']
        self.addCleanup(patch.stopall)
        patch.object(linux.urllib.request, 'urlopen', side_effect=AssertionError('Network forbidden')).start()
        patch.object(linux.subprocess, 'run', side_effect=AssertionError('Compiler forbidden')).start()

    def step(self, name):
        return next(s for s in self.job['steps'] if s['name'] == name)

    def report(self):
        return linux.workflow_failure_report(self.api, self.run, self.branch, self.sha)

    def main(self, *, resolve=None):
        output = io.StringIO()
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, dict(
                MIRROR_TOKEN='fixture-only', PR_HEAD_SHA=self.sha, PR_NUMBER='9249',
                GITHUB_RUN_ID='14476', GITHUB_RUN_ATTEMPT='1', GITHUB_RUN_NUMBER='9249',
                OKILUM_ARTIFACT_DIR=tmp, GITHUB_OUTPUT=str(Path(tmp) / 'output')), clear=True), \
                patch.object(linux.uuid, 'uuid4', return_value=Mock(hex=self.branch.rsplit('-', 1)[1])), \
                patch.object(linux.transport, 'GitHub', return_value=self.api), \
                patch.object(linux.transport, 'push_head') as push, \
                patch.object(linux, 'archived', return_value=None), \
                patch.object(linux, 'discover', return_value=(None, [])), \
                patch.object(linux, 'read_claim', return_value=None), \
                patch.object(linux, 'resolve', side_effect=resolve) as resolver, redirect_stdout(output):
            code = linux.main()
            result = (Path(tmp) / 'output').read_text()
            push.assert_called_once_with(self.branch, self.sha, 'fixture-only')
        return code, result, output.getvalue(), resolver

    def assert_failed(self, result):
        code, written, text, resolver = result
        self.assertEqual((code, written), (1, 'result=failure\n'))
        self.assertIn('Required Linux producer workflow did not pass', text)
        self.assertNotIn('Required full Linux gate did not pass', text)
        resolver.assert_not_called()
        return text

    def test_recorded_production_failure_after_successful_full_gate(self):
        text = self.assert_failed(self.main())
        self.assertIn('full_gate=success; production=failure', text)
        self.assertIn('production failed after successful full gate', text)
        self.assertIn('#10 Run actions/upload-artifact@v4=skipped', text)
        self.assertIn('#9 Produce canonical Linux payload=failure', text)
        self.assertNotIn('claim', text.lower())  # Metadata cannot identify the internal error.
        self.assertEqual([p for m, p in self.api.calls if '/jobs?' in p], [
            'actions/runs/39187802433/attempts/1/jobs?per_page=100&page=1'])

    def assert_unknown_phase(self):
        for text in (self.report(), self.assert_failed(self.main())):
            self.assertIn('phase unknown (exact-attempt job facts unavailable)', text)
            self.assertNotIn('full_gate=', text)
            self.assertNotIn('production failed after successful full gate', text)

    def test_r1_1_job_success_with_required_production_failure_is_unknown(self):
        self.job['conclusion'] = 'success'
        self.assert_unknown_phase()

    def test_r1_1_run_success_with_job_and_production_failure_is_unknown(self):
        self.run['conclusion'] = 'success'
        self.assert_unknown_phase()

    def test_r1_1_failed_gate_with_successful_production_is_unknown(self):
        self.step(linux.BUILD_STEP)['conclusion'] = 'failure'
        self.step(linux.PRODUCE_STEP)['conclusion'] = 'success'
        self.assert_unknown_phase()

    def test_production_failure_after_failed_gate_and_unknown_run_status_are_unknown(self):
        self.step(linux.BUILD_STEP)['conclusion'] = 'failure'
        self.assert_unknown_phase()
        self.step(linux.BUILD_STEP)['conclusion'] = 'success'
        self.run['conclusion'] = 'unexpected-secret'
        self.assert_unknown_phase()

    def test_continue_on_error_failures_do_not_invalidate_recorded_production_failure(self):
        # Both steps have continue-on-error in the exact workflow. Their failure
        # does not contradict successful gate execution or the required failure.
        for index in (5, 10):
            with self.subTest(step=index):
                self.job['steps'] = copy.deepcopy(FIXTURE['jobs'][0]['steps'])
                self.job['steps'][index]['conclusion'] = 'failure'
                text = self.assert_failed(self.main())
                self.assertIn('production failed after successful full gate', text)
                self.assertNotIn('phase unknown', text)

    def test_continue_on_error_failures_with_green_required_steps_remain_green(self):
        self.run['conclusion'] = self.job['conclusion'] = 'success'
        for index in (5, 10):
            with self.subTest(step=index):
                self.job['steps'] = copy.deepcopy(FIXTURE['jobs'][0]['steps'])
                self.step(linux.PRODUCE_STEP)['conclusion'] = 'success'
                self.job['steps'][index]['conclusion'] = 'failure'
                with patch.object(linux, 'workflow_failure_report') as report:
                    code, written, _, resolver = self.main()
                    report.assert_not_called()
                self.assertEqual((code, written), (0, 'result=success\n'))
                resolver.assert_called_once()

    def test_full_gate_failure_or_skipped_is_reported_as_recorded(self):
        for conclusion in ('failure', 'skipped', 'cancelled', 'timed_out'):
            with self.subTest(conclusion=conclusion):
                self.step(linux.BUILD_STEP)['conclusion'] = conclusion
                self.step(linux.PRODUCE_STEP)['conclusion'] = 'skipped'
                text = self.assert_failed(self.main())
                self.assertIn(f'full_gate={conclusion}; production=skipped', text)
                self.assertIn('full gate did not pass', text)
                self.assertNotIn('after successful full gate', text)

    def test_failed_upload_and_post_step_after_successful_production_stay_failed(self):
        for index in (9, 11):
            with self.subTest(step=index):
                self.job['steps'] = copy.deepcopy(FIXTURE['jobs'][0]['steps'])
                self.step(linux.PRODUCE_STEP)['conclusion'] = 'success'
                failed = self.job['steps'][index]
                failed['conclusion'] = 'failure'
                text = self.assert_failed(self.main())
                self.assertIn('full_gate=success; production=success', text)
                self.assertIn('post-production/upload failed', text)
                self.assertIn(f"#{failed['number']} {failed['name']}=failure", text)

    def test_failed_configuration_step_is_named_without_blame_on_gate(self):
        self.step('Identify production configuration')['conclusion'] = 'failure'
        self.step(linux.PRODUCE_STEP)['conclusion'] = 'skipped'
        text = self.assert_failed(self.main())
        self.assertIn('full_gate=success; production=skipped', text)
        self.assertIn('#8 Identify production configuration=failure', text)

    def test_missing_ambiguous_inconsistent_or_malformed_job_facts_are_unknown(self):
        def missing_step():
            self.job['steps'].remove(self.step(linux.PRODUCE_STEP))
        def duplicate_step():
            self.job['steps'].append(dict(self.step(linux.PRODUCE_STEP), number=99))
        def no_execution():
            self.step(linux.BUILD_STEP).pop('started_at')
        changes = [lambda: self.api.jobs.clear(),
                   lambda: self.api.jobs.append(copy.deepcopy(self.job)), missing_step, duplicate_step,
                   lambda: self.job.update(run_attempt=2), lambda: self.job.update(run_id=1),
                   lambda: self.job.update(head_sha='a' * 40), lambda: self.job.update(head_branch='main'),
                   lambda: self.job.pop('run_attempt'), lambda: self.job.update(steps=None),
                   lambda: self.step(linux.BUILD_STEP).update(conclusion='unexpected-secret'),
                   lambda: self.step(linux.PRODUCE_STEP).update(number=7), no_execution]
        for change in changes:
            with self.subTest(change=change):
                self.job = copy.deepcopy(FIXTURE['jobs'][0])
                self.api.jobs = [self.job]
                change()
                text = self.assert_failed(self.main())
                self.assertIn('phase unknown (exact-attempt job facts unavailable)', text)
                self.assertNotIn('unexpected-secret', text)
                self.assertNotIn('full_gate=success', text)

    def test_secondary_metadata_outage_preserves_original_failure_without_raw_error(self):
        request = self.api.request
        def unavailable(path, method='GET', data=None):
            if '/attempts/' in path:
                raise linux.transport.Unavailable('SECRET raw HTTP body', status=403)
            return request(path, method, data)
        hosted_request = linux.transport.hosted_request
        with patch.object(self.api, 'request', side_effect=unavailable), \
                patch.object(linux.transport, 'hosted_request',
                    side_effect=lambda api, path, **_: hosted_request(api, path, sleep=lambda _: None)):
            # Existing pagination/retry behavior is unchanged; no real sleeps.
            text = self.assert_failed(self.main())
        self.assertIn('GitHub native gate: failure', text)
        self.assertIn('phase unknown', text)
        self.assertNotIn('SECRET', text)
        self.assertNotIn('Traceback', text)

    def test_wrong_run_identity_fails_before_metadata_request(self):
        for key, value in (('head_sha', 'a' * 40), ('head_branch', 'main'),
                           ('path', linux.transport.WORKFLOW), ('event', 'pull_request'),
                           ('id', '../secret'), ('run_attempt', None), ('repository', {})):
            with self.subTest(field=key):
                self.run = dict(FIXTURE['run'], **{key: value})
                self.api.calls.clear()
                self.assertIn('phase unknown', self.report())
                self.assertEqual(self.api.calls, [])

    def test_report_uses_observed_exact_attempt_never_latest_jobs(self):
        self.run['run_attempt'] = self.job['run_attempt'] = 2
        text = self.assert_failed(self.main())
        self.assertIn('run=39187802433 attempt=2', text)
        self.assertEqual([p for m, p in self.api.calls if '/jobs?' in p], [
            'actions/runs/39187802433/attempts/2/jobs?per_page=100&page=1'])

    def test_external_names_are_withheld_and_same_named_uploads_keep_step_numbers(self):
        self.job['steps'][9].update(name='SECRET\n::error:: Bearer token', conclusion='failure')
        text = self.report()
        self.assertNotIn('SECRET', text)
        self.assertNotIn('::error::', text)
        self.assertIn('#10 unrecognized step (name omitted)=failure', text)
        self.assertIn('#11 Run actions/upload-artifact@v4=success', text)

    def test_green_metadata_cannot_override_original_workflow_failure(self):
        self.job['conclusion'] = 'success'
        self.step(linux.PRODUCE_STEP)['conclusion'] = 'success'
        text = self.assert_failed(self.main())
        self.assertIn('phase unknown', text)

    def test_failure_without_an_observed_exact_run_has_unknown_phase(self):
        with patch.object(linux.transport, 'wait_for_run', return_value=('failure', 'Ambiguous runs')):
            text = self.assert_failed(self.main())
        self.assertIn('Ambiguous runs', text)
        self.assertIn('phase unknown', text)
        self.assertFalse(any('/jobs?' in p for _, p in self.api.calls))

    def test_terminal_green_adds_no_report_reads_and_still_requires_artifact_resolution(self):
        self.run['conclusion'] = self.job['conclusion'] = 'success'
        self.step(linux.PRODUCE_STEP)['conclusion'] = 'success'
        for resolve, expected in ((None, 0), (ValueError('Strict artifact validation failed'), 1)):
            with self.subTest(resolve=resolve):
                self.api.calls.clear()
                with patch.object(linux, 'workflow_failure_report') as report:
                    code, written, text, resolver = self.main(resolve=resolve)
                    report.assert_not_called()
                self.assertEqual(code, expected)
                self.assertEqual(written, 'result=' + ('success' if expected == 0 else 'failure') + '\n')
                resolver.assert_called_once()
                self.assertEqual([p for m, p in self.api.calls if '/jobs?' in p], [
                    'actions/runs/39187802433/jobs?per_page=100'])


if __name__ == '__main__':
    unittest.main()
