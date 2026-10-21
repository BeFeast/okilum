"""Finite retained-evidence controls. No hosted execution or status mutation."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('reconcile_bridge', Path(__file__).with_name('github-macos.py'))
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)
FIXTURE = json.loads((Path(__file__).with_name('fixtures') / 'macos9765-current.json').read_text())


class API:
    def __init__(self, fixture):
        self.fixture = fixture
        self.calls = []

    def request(self, path, method='GET', data=None):
        self.calls.append((path, method))
        assert method == 'GET', 'No mutation allowed'
        if path == 'actions/runs/39604244715':
            return self.fixture['run']
        if path == 'actions/runs/39604244715/attempts/1/jobs?per_page=100':
            return self.fixture['jobs']
        if path == 'actions/workflows/forgejo-macos.yml':
            return self.fixture['workflow']
        raise AssertionError(path)


class ReconcileTests(unittest.TestCase):
    def verify(self, data, directory):
        api = API(data)
        result = bridge.reconcile(api, sha=FIXTURE['run']['head_sha'], number='1456',
                                  forgejo_run='15277', attempt='1', run_id='39604244715',
                                  job_id='119236245615', evidence=directory)
        return result, api

    def test_actual_retained_current_response_passes_original_and_strict_predicate(self):
        self.assertTrue(bridge.native_gate_executed(FIXTURE['jobs']['jobs']))
        with tempfile.TemporaryDirectory() as root:
            result, api = self.verify(copy.deepcopy(FIXTURE), Path(root)/'once')
            self.assertEqual(result['classification'], 'CURRENT_EXACT_NATIVE_SUCCESS')
            self.assertEqual(result['historical_mismatch_cause'], 'UNKNOWN')
            self.assertEqual(result['required_forgejo_aggregate'], 'UNCHANGED')
            self.assertEqual(result['compiler_dispatches'], 0)
            self.assertEqual(result['status_writes'], 0)
            self.assertEqual(len(api.calls), 3)
            for name in ['run', 'jobs', 'workflow']:
                self.assertEqual(json.loads((Path(root)/'once'/f'{name}.json').read_text()), FIXTURE[name])
            with self.assertRaises(FileExistsError):
                self.verify(FIXTURE, Path(root)/'once')

    def test_each_identity_or_executed_native_boundary_refuses(self):
        mutations = [
            ('run', 'id', 1), ('run', 'head_sha', 'f'*40),
            ('run', 'head_branch', FIXTURE['run']['head_branch'].replace('-15277-', '-15278-')),
            ('run', 'run_attempt', 2), ('run', 'run_attempt', True),
            ('run', 'path', '.github/workflows/ci.yml'), ('run', 'event', 'workflow_dispatch'),
            ('run', 'status', 'in_progress'), ('run', 'conclusion', 'failure'),
            ('run', 'repository', {'full_name': 'fork/okilum'}),
            ('run', 'head_repository', {'full_name': 'fork/okilum'}),
            ('run', 'html_url', 'https://example.org/not-evidence'), ('run', 'workflow_id', 1),
            ('workflow', 'id', 1), ('workflow', 'state', 'disabled_manually'),
            ('jobs', 'total_count', 2), ('jobs', 'total_count', True),
        ]
        for section, key, value in mutations:
            with self.subTest(section=section, key=key), tempfile.TemporaryDirectory() as root:
                data = copy.deepcopy(FIXTURE)
                data[section][key] = value
                with self.assertRaises(ValueError):
                    self.verify(data, Path(root)/'evidence')
                self.assertTrue((Path(root)/'evidence'/'run.json').exists())
        for key, value in [('id', 1), ('run_id', 1), ('run_attempt', 2), ('head_sha', 'f'*40),
                           ('name', 'unrelated'), ('status', 'in_progress'), ('conclusion', 'skipped'),
                           ('steps', [])]:
            with self.subTest(job=key), tempfile.TemporaryDirectory() as root:
                data = copy.deepcopy(FIXTURE)
                data['jobs']['jobs'][0][key] = value
                with self.assertRaises(ValueError):
                    self.verify(data, Path(root)/'evidence')
        for change in ['skipped', 'duplicate', 'timestamp', 'malformed', 'inverted']:
            with self.subTest(step=change), tempfile.TemporaryDirectory() as root:
                data = copy.deepcopy(FIXTURE)
                steps = data['jobs']['jobs'][0]['steps']
                step = next(s for s in steps if s['name'] == bridge.BUILD_STEP)
                if change == 'duplicate':
                    steps.append(dict(step))
                elif change == 'timestamp':
                    step.pop('started_at')
                elif change == 'malformed':
                    step['started_at'] = 'invalid'
                elif change == 'inverted':
                    step['completed_at'] = step['started_at']
                else:
                    step['conclusion'] = change
                with self.assertRaises(ValueError):
                    self.verify(data, Path(root)/'evidence')

    def test_missing_or_paginated_native_job_fails_closed(self):
        for jobs in [[], FIXTURE['jobs']['jobs']*2]:
            with tempfile.TemporaryDirectory() as root:
                data = copy.deepcopy(FIXTURE)
                data['jobs']['jobs'] = jobs
                with self.assertRaises(ValueError):
                    self.verify(data, Path(root)/'evidence')

    def test_transport_failure_keeps_partial_evidence_without_retry(self):
        api = API(FIXTURE)
        original = api.request
        def request(path):
            if '/jobs?' in path:
                raise bridge.Unavailable('read unavailable')
            return original(path)
        api.request = request
        with tempfile.TemporaryDirectory() as root:
            target = Path(root)/'evidence'
            with self.assertRaises(bridge.Unavailable):
                bridge.reconcile(api, sha=FIXTURE['run']['head_sha'], number='1456',
                                 forgejo_run='15277', attempt='1', run_id='39604244715',
                                 job_id='119236245615', evidence=target)
            self.assertTrue((target/'run.json').exists())
            self.assertFalse((target/'receipt.json').exists())
            self.assertEqual(len(api.calls), 1)


if __name__ == '__main__':
    unittest.main()
