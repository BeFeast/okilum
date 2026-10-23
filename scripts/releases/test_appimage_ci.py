"""Hosted-only CI recovery/local reuse regressions; no artifact execution here."""
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

import appimage_ci as ci
from test_appimage_publication import evidence, SOURCE


class Recovery(unittest.TestCase):
    def fixture(self, attempt=2):
        receipt, _, _, _ = evidence()
        run = {**receipt['run'], 'workflow_id': 71, 'run_attempt': attempt}
        prior = {**run, 'run_attempt': 1, 'conclusion': 'failure'}
        def step(name, conclusion='success'):
            return {'name': name, 'status': 'completed', 'conclusion': conclusion}
        jobs = [{'id': i + 100, 'name': name, 'run_id': run['id'], 'run_attempt': 1,
                 'head_sha': SOURCE, 'status': 'completed', 'conclusion': outcome,
                 'steps': []} for i, (name, outcome) in enumerate([
                     ('linux-checks', 'success'), ('appimage', 'failure'), ('linux', 'failure')])]
        jobs[0]['steps'] = [step('Run full Linux gate')]
        jobs[1]['steps'] = [step('Build exact-source AppImage once'),
                            step(ci.publisher.artifact.SMOKE), step(ci.SIGNATURE, 'failure')]
        api = Mock()
        api.request.side_effect = lambda path: ({'jobs': jobs, 'total_count': len(jobs)}
                                                if '/jobs?' in path else prior)
        log = ('Traceback (most recent call last):\n'
               '  File "appimage_ci.py", line 54, in handoff\n'
               "    data = artifact.download(os.environ['GH_TOKEN'], item['id'])\n"
               'urllib.error.HTTPError: HTTP Error 503: Egress is over the account limit.\n')
        fetch = Mock(return_value=log)
        return run, prior, jobs, api, fetch

    def test_verified_transport_is_positive_control_and_original_needs_no_history(self):
        run, _, _, api, fetch = self.fixture()
        ci.verify_attempt(api, run, fetch)
        fetch.assert_called_once_with(101)
        api.reset_mock()
        ci.verify_attempt(api, {**run, 'run_attempt': 1}, fetch)
        api.request.assert_not_called()

    def test_identity_compile_smoke_signature_and_incomplete_evidence_fail_closed(self):
        mutations = [lambda r, p, j: p.update(head_sha='b' * 40),
                     lambda r, p, j: p.update(id=99),
                     lambda r, p, j: p.update(workflow_id=99),
                     lambda r, p, j: p.update(run_attempt=2),
                     lambda r, p, j: p.update(conclusion='success'),
                     lambda r, p, j: j[0].update(conclusion='failure'),
                     lambda r, p, j: j[1].update(run_attempt=2),
                     lambda r, p, j: j[1]['steps'][0].update(conclusion='failure'),
                     lambda r, p, j: j[1]['steps'][1].update(conclusion='failure'),
                     lambda r, p, j: j[1]['steps'].append({'name': 'provenance', 'conclusion': 'failure'}),
                     lambda r, p, j: j.pop(),
                     lambda r, p, j: r.update(run_attempt=4)]
        for mutate in mutations:
            run, prior, jobs, api, fetch = self.fixture()
            mutate(run, prior, jobs)
            with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                ci.verify_attempt(api, run, fetch)

    def test_log_absence_signature_provenance_or_bare_503_never_authorize_recovery(self):
        run, _, _, api, fetch = self.fixture()
        positive = fetch.return_value
        for log in ['', 'HTTP Error 503: Egress is over the account limit.\n',
                    positive.replace('in handoff', 'in publish'),
                    positive.replace('HTTP Error 503', 'HTTP Error 403'),
                    positive + 'APPIMAGE_SIGNED_INSTALL_PASS',
                    positive + 'Traceback (most recent call last):\nValueError: provenance']:
            fetch.return_value = log
            with self.subTest(log=log), self.assertRaises(ValueError):
                ci.verify_attempt(api, run, fetch)

    def test_every_prior_attempt_must_be_verified(self):
        run, prior, jobs, api, fetch = self.fixture(attempt=3)
        # Replaying attempt-one evidence for attempt two must fail.
        with self.assertRaises(ValueError):
            ci.verify_attempt(api, run, fetch)
        self.assertEqual(fetch.call_count, 1)


class LocalReuse(unittest.TestCase):
    def test_no_archive_get_and_current_attempt_provenance_is_preserved(self):
        receipt, _, image, provenance = evidence()
        run = {**receipt['run'], 'run_attempt': 2}
        provenance['workflow_attempt'] = '2'
        api = Mock()
        api.request.side_effect = [receipt['artifact'], run]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            producer = root / 'producer'; producer.mkdir()
            (producer / ci.publisher.artifact.IMAGE).write_bytes(image)
            (producer / 'provenance.json').write_text(json.dumps(provenance))
            (producer / 'SHA256SUMS').write_text(ci.publisher.digest(image) + '  ' + ci.publisher.artifact.IMAGE + '\n')
            (producer / 'bundled-libraries.txt').write_text('')
            with patch.object(ci, 'current_run', return_value=(api, run, SOURCE)), \
                    patch.dict(os.environ, APPIMAGE_ARTIFACT_ID='29'), \
                    patch.object(ci.publisher.artifact, 'download', side_effect=AssertionError('no GET')):
                payload, source, result = ci.handoff(root, producer)
                self.assertEqual(source, SOURCE)
                self.assertEqual(result['run']['run_attempt'], 2)
                self.assertEqual((payload / ci.publisher.artifact.IMAGE).read_bytes(), image)
                self.assertEqual(json.loads((payload / 'provenance.json').read_text()), provenance)
                # Reused producer bytes cannot become original-attempt publication authority.
                with self.assertRaises(ValueError):
                    ci.publisher.artifact.accepted(run, receipt['jobs'], SOURCE)
                (producer / ci.publisher.artifact.IMAGE).write_bytes(image + b'tamper')
                api.request.side_effect = [receipt['artifact'], run]
                other = root / 'bad'; other.mkdir()
                with self.assertRaises(ValueError):
                    ci.handoff(other, producer)
