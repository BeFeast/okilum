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


class ReuseOutcomes(unittest.TestCase):
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
                   lambda r, x: r['run'].update(status='in_progress', conclusion=None),
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


class ArchIndependence(unittest.TestCase):
    def test_unique_successful_release_authorizes_failed_or_running_aggregate(self):
        import publication
        run = {'workflow_id': 'linux-release.yml', 'prettyref': 'main',
               'is_fork_pull_request': False, 'trigger_event': 'schedule',
               'status': 'failure', 'commit_sha': SOURCE}
        job = {'name': 'release', 'status': 'success'}
        for status in ['failure', 'running', 'success']:
            self.assertTrue(publication.eligible({**run, 'status': status}, 'linux', SOURCE, [job]))
        for jobs in [[], [job, job], [{**job, 'status': 'failure'}],
                     [{**job, 'status': 'running'}]]:
            self.assertFalse(publication.eligible(run, 'linux', SOURCE, jobs))
        self.assertFalse(publication.eligible({**run, 'status': 'cancelled'}, 'linux', SOURCE, [job]))
        self.assertEqual(publication.ARTIFACTS['linux'], {'arch-publication': 'arch'})


class WorkflowIndependence(unittest.TestCase):
    def test_dag_preserves_each_lanes_own_acceptance(self):
        workflow = (Path(__file__).resolve().parents[2] /
                    '.forgejo/workflows/linux-release.yml').read_text()
        def job(name):
            section = workflow.split('\n  ' + name + ':\n', 1)[1]
            import re
            return re.split(r'\n  [a-z][a-z-]*:\n', section, maxsplit=1)[0]
        arch = job('publication')
        self.assertIn('needs: [select, release]', arch)
        self.assertIn("needs.release.result == 'success'", arch)
        self.assertIn('if: always()', arch)
        appimage = job('appimage-publication')
        self.assertIn('needs: [select, appimage-reuse]', appimage)
        self.assertIn("needs.appimage-reuse.outputs.accepted == 'true'", appimage)
        reuse = job('appimage-reuse')
        self.assertNotIn('outputs.build', reuse)
        self.assertIn("if: steps.reuse.outputs.accepted == 'true'", reuse)
