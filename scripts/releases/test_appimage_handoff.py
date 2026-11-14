"""Independent handoff regressions; run by the hosted Linux gate."""
import copy
import io
import tempfile
from pathlib import Path
from contextlib import redirect_stdout
import unittest
from unittest.mock import Mock, patch

import appimage_handoff as handoff
from test_appimage_publication import evidence, SOURCE


class IndependentAcceptance(unittest.TestCase):
    def test_hourly_and_manual_originals_are_accepted_but_nightly_copies_are_not(self):
        receipt, _, _, _ = evidence()
        for event in ['schedule', 'workflow_dispatch']:
            receipt['run']['event'] = event
            with self.assertRaises(handoff.ReuseUnavailable):
                handoff.accepted(receipt['run'], receipt['jobs'], SOURCE)
            build = {'name': 'Build exact-source AppImage once', 'status': 'completed',
                     'conclusion': 'success'}
            receipt['jobs'][0]['steps'].append(build)
            handoff.accepted(receipt['run'], receipt['jobs'], SOURCE)
            receipt['jobs'][0]['steps'].pop()
        for event in ['pull_request', 'pull_request_target', 'repository_dispatch']:
            receipt['run']['event'] = event
            with self.assertRaises(ValueError):
                handoff.accepted(receipt['run'], receipt['jobs'], SOURCE)

    def test_exact_hourly_source_ref_retains_immutable_run_job_and_artifact_sha(self):
        receipt, _, _, _ = evidence()
        receipt['run']['head_branch'] = 'forgejo-linux-pr/hourly-' + SOURCE
        handoff.accepted(receipt['run'], receipt['jobs'], SOURCE)
        handoff.artifact.artifact_identity(receipt['artifact'], receipt['run'], SOURCE)
        for branch in ['forgejo-linux-pr/hourly-' + 'b' * 40, 'forgejo-linux-pr/1']:
            with self.assertRaises(ValueError):
                handoff.accepted(dict(receipt['run'], head_branch=branch), receipt['jobs'], SOURCE)

    def test_appimage_smoke_survives_failed_linux_aggregate(self):
        receipt, _, _, _ = evidence()
        receipt['run']['conclusion'] = 'failure'
        receipt['jobs'][1]['conclusion'] = 'failure'
        handoff.accepted(receipt['run'], receipt['jobs'], SOURCE)
        for changes in [{'head_branch': 'forgejo-linux-pr/1'}, {'run_attempt': 2},
                        {'repository': {'full_name': 'other/repo'}}]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                handoff.accepted({**receipt['run'], **changes}, receipt['jobs'], SOURCE)
        jobs = copy.deepcopy(receipt['jobs'])
        jobs[0]['steps'] = []
        with self.assertRaises(ValueError):
            handoff.accepted(receipt['run'], jobs, SOURCE)

    def test_complete_appimage_does_not_wait_for_an_aggregate_runner(self):
        receipt, _, _, _ = evidence()
        receipt['run'].update(status='in_progress', conclusion=None)
        handoff.accepted(receipt['run'], receipt['jobs'], SOURCE)
        receipt['jobs'][0].update(status='in_progress', conclusion=None)
        with self.assertRaises(handoff.ReuseUnavailable):
            handoff.accepted(receipt['run'], receipt['jobs'], SOURCE)

    def test_legacy_full_native_smoke_survives_tooling_upgrade_without_hiding_new_failure(self):
        receipt, _, _, _ = evidence()
        old = {**receipt['jobs'][0]['steps'][0], 'name':'Verify unchanged AppImage on clean Ubuntu and Fedora'}
        receipt['jobs'][0]['steps'] = [old]
        handoff.accepted(receipt['run'], receipt['jobs'], SOURCE)
        receipt['jobs'][0]['steps'].append({**old, 'name':handoff.artifact.SMOKE, 'conclusion':'failure'})
        with self.assertRaises(handoff.ReuseUnavailable):
            handoff.accepted(receipt['run'], receipt['jobs'], SOURCE)

    def test_trusted_origin_and_unique_artifact_before_key_import(self):
        run = {'id': 7, 'workflow_id': 'linux-release.yml', 'prettyref': 'main',
               'is_fork_pull_request': False, 'trigger_event': 'workflow_dispatch',
               'commit_sha': SOURCE, 'index_in_repo': 123, 'status': 'failure'}
        handoff.trusted(run, 7)
        for changes in [{'workflow_id': 'other.yml'}, {'prettyref': 'feature'},
                        {'is_fork_pull_request': True}, {'trigger_event': 'pull_request'}]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                handoff.trusted({**run, **changes}, 7)
        item = {'id': 9, 'name': 'appimage-publication', 'expired': False, 'run_id': 7}
        with patch.object(handoff.publisher.arch, 'SigningKey') as key:
            for items in [[], [item, item], [{**item, 'expired': True}]]:
                client = Mock()
                client.call.side_effect = [run, items]
                with self.assertRaises(ValueError):
                    handoff.publish(client, 7)
            key.assert_not_called()


class AcceptanceGate(unittest.TestCase):
    """Ubuntu/Fedora smoke and disposable-key signing block beta publication."""

    def evidence(self, in_job=False, acceptance=('completed', 'success')):
        receipt, _, _, _ = evidence()
        run, jobs = receipt['run'], copy.deepcopy(receipt['jobs'])
        run = {**run, 'head_branch': 'forgejo-linux-pr/hourly-' + SOURCE}
        checks = [{'name': name, 'status': 'completed', 'conclusion': 'success'}
                  for name in handoff.ACCEPTANCE_STEPS]
        if in_job:
            jobs[0]['steps'] += checks
        elif acceptance:
            status, conclusion = acceptance
            jobs.append({'id': 5, 'name': handoff.ACCEPTANCE_JOB, 'run_id': run['id'], 'head_sha': SOURCE,
                         'status': status, 'conclusion': conclusion,
                         'steps': checks if conclusion == 'success' else checks[:1]})
        return run, jobs

    def test_separate_or_in_job_checks_pass_and_failures_never_publish(self):
        for in_job in (False, True):
            handoff.acceptance(*self.evidence(in_job=in_job), SOURCE)
        for outcome in [('completed', 'failure'), ('completed', 'cancelled'), ('completed', 'skipped')]:
            with self.subTest(outcome=outcome), self.assertRaises(ValueError) as caught:
                handoff.acceptance(*self.evidence(acceptance=outcome), SOURCE)
            self.assertNotIsInstance(caught.exception, handoff.ReuseUnavailable)
        # A successful job whose checks did not both run is not acceptance.
        run, jobs = self.evidence()
        jobs[-1]['steps'] = jobs[-1]['steps'][:1]
        with self.assertRaises(ValueError):
            handoff.acceptance(run, jobs, SOURCE)

    def test_running_or_absent_acceptance_waits_but_completed_absence_fails(self):
        for outcome in [('queued', None), ('in_progress', None)]:
            with self.subTest(outcome=outcome), self.assertRaises(handoff.ReuseUnavailable):
                handoff.acceptance(*self.evidence(acceptance=outcome), SOURCE)
        run, jobs = self.evidence(acceptance=None)
        with self.assertRaises(handoff.ReuseUnavailable):
            handoff.acceptance({**run, 'status': 'in_progress', 'conclusion': None}, jobs, SOURCE)
        with self.assertRaises(ValueError) as caught:
            handoff.acceptance(run, jobs, SOURCE)
        self.assertNotIsInstance(caught.exception, handoff.ReuseUnavailable)
        run, jobs = self.evidence()
        jobs[-1]['run_id'] = 99
        with self.assertRaises(ValueError):
            handoff.acceptance(run, jobs, SOURCE)

    def test_await_polls_until_success_and_refuses_failure_or_new_identity(self):
        import json
        run, pending = self.evidence(acceptance=('in_progress', None))
        _, done = self.evidence()
        _, failed = self.evidence(acceptance=('completed', 'failure'))
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            (directory / 'acceptance.json').write_text(json.dumps({'run': run, 'jobs': pending}))
            def api_for(*job_lists, current=run):
                api = Mock(); lists = iter(job_lists)
                api.request.side_effect = lambda path: (
                    {'jobs': next(lists)} if '/jobs?' in path else current)
                return api
            sleeps = []
            with redirect_stdout(io.StringIO()):
                handoff.await_acceptance(api_for(pending, done), directory, 600, sleep=sleeps.append)
            self.assertEqual(len(sleeps), 1)
            with self.assertRaises(ValueError):
                handoff.await_acceptance(api_for(failed), directory, 600, sleep=sleeps.append)
            with self.assertRaises(ValueError):
                handoff.await_acceptance(api_for(done, current={**run, 'run_attempt': 2}),
                                         directory, 600, sleep=sleeps.append)
            clock = iter([0, 0, 0, 1000])  # start, deadline check, sleep length, expired check
            with self.assertRaises(ValueError):
                handoff.await_acceptance(api_for(pending, pending), directory, 600,
                                         clock=lambda: next(clock), sleep=sleeps.append)

    def test_publisher_refuses_a_handoff_from_an_unsuccessful_reuse_job(self):
        run = {'id': 7, 'workflow_id': 'linux-release.yml', 'prettyref': 'main',
               'is_fork_pull_request': False, 'trigger_event': 'push',
               'commit_sha': SOURCE, 'index_in_repo': 123, 'status': 'running'}
        item = {'id': 9, 'name': 'appimage-publication', 'expired': False, 'run_id': 7}
        with patch.object(handoff.publisher.arch, 'SigningKey') as key:
            for jobs in [[], [{'name': 'appimage-reuse', 'status': 'failure'}],
                         [{'name': 'appimage-reuse', 'status': 'running'}]]:
                client = Mock()
                client.call.side_effect = [run, [item], jobs]
                with self.subTest(jobs=jobs), self.assertRaises(ValueError):
                    handoff.publish(client, 7)
            key.assert_not_called()

    def test_workflow_step_names_match_the_gate(self):
        import yaml
        root = Path(__file__).resolve().parents[2]
        producer = yaml.safe_load((root / '.github/workflows/forgejo-linux.yml').read_text())
        # The checks run in the producer job; a separate acceptance job (older
        # producers) is still honoured by acceptance().
        names = {step.get('name') for step in producer['jobs']['appimage']['steps']}
        self.assertLessEqual(set(handoff.ACCEPTANCE_STEPS), names)
        self.assertNotIn(handoff.ACCEPTANCE_JOB, producer['jobs'])
        release = yaml.safe_load((root / '.forgejo/workflows/linux-release.yml').read_text())
        steps = release['jobs']['appimage-reuse']['steps']
        runs = [step.get('run', '') for step in steps]
        upload = next(i for i, step in enumerate(steps) if 'upload-artifact' in str(step.get('uses', '')))
        wait = next(i for i, r in enumerate(runs) if 'appimage_handoff.py await-acceptance' in r)
        self.assertGreater(wait, upload)  # bytes move in parallel; publication still waits
        self.assertEqual(steps[wait]['if'], "steps.reuse.outputs.accepted == 'true'")
        self.assertIn('needs.appimage-reuse.result == \'success\'', release['jobs']['appimage-publication']['if'])


class ReuseOutcomes(unittest.TestCase):
    def test_hourly_idle_runs_do_not_make_the_original_producer_ambiguous(self):
        receipt, data, _, _ = evidence()
        original = receipt['run']
        idle = {**original, 'id': 99, 'event': 'schedule'}
        api = Mock()
        def request(path):
            if '/runs?' in path:
                return {'workflow_runs': [idle, original], 'total_count': 2}
            if '/runs/99/artifacts?' in path:
                return {'artifacts': [], 'total_count': 0}
            if '/artifacts?' in path:
                return {'artifacts': [receipt['artifact']]}
            if '/jobs?' in path:
                return {'jobs': receipt['jobs']}
            return original
        api.request.side_effect = request
        with tempfile.TemporaryDirectory() as directory:
            self.assertTrue(handoff.stage_result(api, SOURCE, Path(directory) / 'handoff', lambda _: data))
        # Existing real-producer ambiguity test below remains the negative control.

    def retry(self, first_change):
        """A push producer R1 and its recovery dispatch R2 on the same hourly ref."""
        receipt, data, _, _ = evidence()
        ref = 'forgejo-linux-pr/hourly-' + SOURCE
        second = {**receipt['run'], 'id': 18, 'run_number': 24, 'event': 'workflow_dispatch', 'head_branch': ref}
        first = {**receipt['run'], 'head_branch': ref}
        first_jobs = copy.deepcopy(receipt['jobs'])
        first_change(first, first_jobs)
        second_jobs = [{**job, 'run_id': 18} for job in copy.deepcopy(receipt['jobs'])]
        # A dispatched producer counts only with its own executed build step.
        second_jobs[0]['steps'].append({'name': 'Build exact-source AppImage once',
                                        'status': 'completed', 'conclusion': 'success'})
        item = {**receipt['artifact'], 'workflow_run': {'id': 18, 'head_sha': SOURCE}}
        def request(path):
            if '/runs?' in path:
                return {'workflow_runs': [first, second], 'total_count': 2}
            if '/artifacts?' in path:
                return {'artifacts': [item if '/runs/18/' in path else receipt['artifact']]}
            if '/jobs?' in path:
                return {'jobs': second_jobs if '/runs/18/' in path else first_jobs}
            return second if '/runs/18' in path else first
        api = Mock(); api.request.side_effect = request
        return api, data

    def test_failed_or_cancelled_push_producer_does_not_shadow_its_recovery_build(self):
        # In-job acceptance failed after the upload: the push run still has bytes.
        def failed(run, jobs):
            run.update(conclusion='failure'); jobs[0].update(conclusion='failure')
        def cancelled(run, jobs):
            run.update(conclusion='cancelled')
        for change in (failed, cancelled):
            api, data = self.retry(change)
            with self.subTest(change=change.__name__):
                self.assertEqual(handoff.original_main_producer(
                    api, SOURCE, api.request('/runs?'))['id'], 18)
        # Negative control: two accepted producers stay ambiguous.
        api, data = self.retry(lambda run, jobs: None)
        with self.assertRaises(ValueError) as caught:
            handoff.original_main_producer(api, SOURCE, api.request('/runs?'))
        self.assertNotIsInstance(caught.exception, handoff.ReuseUnavailable)

    def stage(self, change=None, error=None):
        receipt, data, _, _ = evidence()
        responses = {'runs': {'workflow_runs': [receipt['run']]},
                     'jobs': {'jobs': receipt['jobs']},
                     'artifacts': {'artifacts': [receipt['artifact']]}}
        if change:
            change(receipt, responses)
        def request(path):
            if error:
                raise error
            if '/runs?' in path:
                return responses['runs']
            if '/jobs?' in path:
                return responses['jobs']
            if '/artifacts?' in path:
                return responses['artifacts']
            return receipt['run']
        api = Mock(); api.request.side_effect = request
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp) / 'handoff'
            log = io.StringIO()
            with redirect_stdout(log):
                result = handoff.stage_result(api, SOURCE, directory, lambda aid: data)
            self.assertEqual(directory.exists(), result)
            if not result:
                self.assertIn('APPIMAGE_REUSE_SKIP source=' + SOURCE, log.getvalue())
            return result

    def test_accepted_payload_is_positive_control_despite_other_lane_failure(self):
        self.assertTrue(self.stage(lambda r, responses: r['run'].update(conclusion='failure')))

    def test_expected_absence_has_no_handoff(self):
        changes = [lambda r, x: x['runs'].update(workflow_runs=[]),
                   lambda r, x: r['jobs'][0].update(conclusion='failure'),
                   lambda r, x: x['artifacts'].update(artifacts=[]),
                   lambda r, x: r['artifact'].update(expired=True)]
        for change in changes:
            with self.subTest(change=change):
                self.assertFalse(self.stage(change))

    def test_cancelled_producer_is_explicit_error_despite_successful_smoke(self):
        with self.assertRaisesRegex(ValueError, 'Cancelled original') as caught:
            self.stage(lambda r, x: r['run'].update(conclusion='cancelled'))
        self.assertNotIsInstance(caught.exception, handoff.ReuseUnavailable)

    def test_legitimate_smoke_outcomes_skip_without_handoff(self):
        for conclusion in ['failure', 'skipped', 'cancelled', 'timed_out']:
            def unsuccessful(r, x):
                r['run']['conclusion'] = 'failure'
                r['jobs'][0]['conclusion'] = conclusion
                r['jobs'][0]['steps'][0]['conclusion'] = conclusion
            with self.subTest(conclusion=conclusion):
                self.assertFalse(self.stage(unsuccessful))
        def running(r, x):
            for record in [r['run'], r['jobs'][0], r['jobs'][0]['steps'][0]]:
                record.update(status='in_progress', conclusion=None)
        self.assertFalse(self.stage(running))
        self.assertFalse(self.stage(lambda r, x:
            r['jobs'][0]['steps'][0].update(conclusion='skipped')))
        self.assertFalse(self.stage(lambda r, x: r['jobs'][0].update(steps=[])))
        self.assertFalse(self.stage(lambda r, x: r['jobs'].pop(0)))

    def test_malformed_execution_evidence_is_error_never_successful_skip(self):
        # Exercise stage_result itself: ValueError alone would also match SKIP's
        # ReuseUnavailable subclass and miss the original regression.
        for level in ['producer', 'job', 'smoke']:
            for field in ['status', 'conclusion']:
                for value in ['missing', 'unknown', 42, [], None]:
                    def malformed(r, x):
                        record = {'producer': r['run'], 'job': r['jobs'][0],
                                  'smoke': r['jobs'][0]['steps'][0]}[level]
                        if value == 'missing':
                            record.pop(field)
                        else:
                            record[field] = value
                    with self.subTest(level=level, field=field, value=value):
                        with self.assertRaises(ValueError) as caught:
                            self.stage(malformed)
                        self.assertNotIsInstance(caught.exception, handoff.ReuseUnavailable)
        changes = [lambda r, x: r['run'].update(status='in_progress'),
                   lambda r, x: r['jobs'][0].update(status='queued'),
                   lambda r, x: r['jobs'][0]['steps'][0].update(status='in_progress'),
                   lambda r, x: r['jobs'][0].update(steps=None),
                   lambda r, x: r['jobs'][0].update(steps=[None]),
                   lambda r, x: r['jobs'][0]['steps'].append(r['jobs'][0]['steps'][0]),
                   lambda r, x: x['jobs'].update(jobs=[None])]
        for change in changes:
            with self.subTest(change=change), self.assertRaises(ValueError) as caught:
                self.stage(change)
            self.assertNotIsInstance(caught.exception, handoff.ReuseUnavailable)

    def test_identity_ambiguity_digest_and_transport_remain_errors(self):
        changes = [lambda r, x: x['runs']['workflow_runs'].append(r['run']),
                   lambda r, x: r['run'].update(run_attempt=2),
                   lambda r, x: r['jobs'][0].update(head_sha='b' * 40),
                   lambda r, x: r['artifact'].update(digest='sha256:' + '0' * 64),
                   lambda r, x: x['artifacts']['artifacts'].append(r['artifact'])]
        for change in changes:
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.stage(change)
        for error in [PermissionError('401'), ConnectionError('network')]:
            with self.subTest(error=error), self.assertRaises(type(error)):
                self.stage(error=error)


class ArchCompatibility(unittest.TestCase):
    def test_arch_retains_successful_aggregate_gate_and_own_artifact(self):
        import publication
        run = {'workflow_id': 'linux-release.yml', 'prettyref': 'main',
               'is_fork_pull_request': False, 'trigger_event': 'schedule',
               'status': 'success', 'commit_sha': SOURCE}
        self.assertTrue(publication.eligible(run, 'linux', SOURCE))
        for status in ['failure', 'running', 'cancelled', 'skipped']:
            with self.subTest(status=status):
                self.assertFalse(publication.eligible({**run, 'status': status}, 'linux', SOURCE))
        self.assertEqual(publication.ARTIFACTS['linux'], {'arch-publication': 'arch'})


class WorkflowIndependence(unittest.TestCase):
    def test_dag_decouples_appimage_without_weakening_arch(self):
        workflow = (Path(__file__).resolve().parents[2] /
                    '.forgejo/workflows/linux-release.yml').read_text()
        def job(name):
            section = workflow.split('\n  ' + name + ':\n', 1)[1]
            import re
            return re.split(r'\n  [a-z][a-z-]*:\n', section, maxsplit=1)[0]
        arch = job('publication')
        self.assertIn('needs: [select, release]', arch)
        self.assertIn("needs.release.result == 'success'", arch)
        self.assertNotIn('if: always()', arch)
        appimage = job('appimage-publication')
        self.assertIn('needs: [select, appimage-reuse]', appimage)
        self.assertIn("needs.appimage-reuse.outputs.accepted == 'true'", appimage)
        reuse = job('appimage-reuse')
        self.assertIn("needs.select.outputs.build == 'true'", reuse)
        self.assertIn("if: steps.reuse.outputs.accepted == 'true'", reuse)
