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


FORGEJO = json.loads((Path(__file__).with_name('fixtures')/'macos9765-forgejo.json').read_text())
TOOLING = 'b'*40
EXECUTION = {'tooling': TOOLING, 'id': 999, 'number': 888}
INPUTS = {'source': FIXTURE['run']['head_sha'], 'pr': '1456', 'forgejo_run': '15277',
          'github_run': '39604244715', 'github_job': '119236245615'}


class Forgejo:
    """Original gate responses retained; own new workflow/API reporting synthetic."""
    def __init__(self):
        self.data = copy.deepcopy(FORGEJO)
        self.own = {'id': 999, 'index_in_repo': 888, 'workflow_id': 'ci-reconcile.yml',
                    'event': 'workflow_dispatch', 'commit_sha': TOOLING,
                    'repository': {'full_name': bridge.REPOSITORY}, 'status': 'running',
                    'event_payload': json.dumps({'inputs': INPUTS})}
        self.own_jobs = [{'name': 'reconcile', 'run_id': 999, 'attempt': 1, 'status': 'running'}]
        self.artifacts = {'artifacts': [{'id': 123, 'name': 'ci-reconcile-999-'+INPUTS['source'],
                                        'expired': False, 'size_in_bytes': 100}]}
        self.calls = []
        self.main = TOOLING
        self.main_ci = {'statuses':[{'context':c,'status':'success'} for c in ['ci / linux (push)','ci / check (push)']]}
        self.fail_second_post = False

    def call(self, method, path, body=None):
        self.calls.append((method, path, copy.deepcopy(body)))
        if method == 'POST':
            assert path == '/statuses/'+INPUTS['source']
            if self.fail_second_post and body['context'].startswith('ci / check'):
                raise bridge.Unavailable('report unavailable')
            self.data['statuses']['statuses'] = [s for s in self.data['statuses']['statuses']
                                                if s['context'] != body['context']]
            self.data['statuses']['statuses'].append({'context': body['context'],
                                                     'status': body['state'], 'target_url': body['target_url']})
            return {}
        return {'/pulls/1456': self.data['pr'], '/branches/main': {'commit': {'id': self.main}},
                '/commits/'+TOOLING+'/status': self.main_ci,
                '/actions/runs/999': self.own, '/actions/runs/999/jobs': self.own_jobs,
                '/branch_protections/main': self.data['protection'],
                '/actions/runs/15277': self.data['original'],
                '/actions/runs/15277/jobs': self.data['jobs'],
                '/commits/'+INPUTS['source']+'/status': self.data['statuses'],
                '/actions/runs/999/artifacts': self.artifacts}[path]


class GateConsumerTests(unittest.TestCase):
    def prepare(self, forgejo, directory):
        return bridge.prepare_gates(forgejo, API(copy.deepcopy(FIXTURE)), EXECUTION,
                                    sha=INPUTS['source'], number='1456', forgejo_run='15277',
                                    attempt='1', run_id='39604244715', job_id='119236245615',
                                    evidence=directory)

    def test_retained_all_gates_then_automatic_status_report_has_distinct_execution_target(self):
        f = Forgejo()
        with tempfile.TemporaryDirectory() as root:
            proof = self.prepare(f, Path(root)/'proof')
            self.assertTrue(all(c[0] == 'GET' for c in f.calls))
            self.assertEqual(f.data['original']['status'], 'failure')
            result = bridge.report_gates(f, proof, evidence_uploaded='success')
            self.assertEqual(result['source'], INPUTS['source'])
            self.assertNotEqual(result['source'], result['tooling'])
            posts = [c for c in f.calls if c[0] == 'POST']
            self.assertEqual(len(posts), 2)
            self.assertEqual([c[2]['context'] for c in posts], proof['report_contexts'])
            self.assertTrue(all(c[2]['target_url'].endswith('/actions/runs/888') for c in posts))
            self.assertEqual(f.data['original']['status'], 'failure')
            self.assertEqual(next(j['status'] for j in f.data['jobs'] if j['name']=='macos'), 'failure')
            self.assertTrue((Path(root)/'proof'/'original-jobs.json').exists())

    def test_any_other_failed_missing_rerun_or_ambiguous_original_gate_refuses_before_status(self):
        for index, job in enumerate(FORGEJO['jobs']):
            for field, value in [('status','failure' if job['status'] != 'failure' else 'success'),
                                 ('attempt',2),('run_id',1)]:
                with self.subTest(job=job['name'],field=field), tempfile.TemporaryDirectory() as root:
                    f=Forgejo(); f.data['jobs'][index][field]=value
                    with self.assertRaises(ValueError): self.prepare(f,Path(root)/'proof')
                    self.assertTrue(all(c[0]=='GET' for c in f.calls))
        for change in ['extra_context','stale_status','duplicate_status','missing_job','foreign_original','wrong_original_sha',
                       'different_main','red_main','wrong_execution','wrong_inputs','rerun_execution','new_pr_source','draft','closed']:
            with self.subTest(change=change), tempfile.TemporaryDirectory() as root:
                f=Forgejo()
                if change=='extra_context': f.data['protection']['status_check_contexts'].append('new-required-gate')
                elif change=='stale_status': f.data['statuses']['statuses'][0]['target_url']='/unrelated'
                elif change=='duplicate_status': f.data['statuses']['statuses'].append(dict(f.data['statuses']['statuses'][0]))
                elif change=='missing_job': f.data['jobs'].pop()
                elif change=='foreign_original': f.data['original']['repository']['full_name']='fork/okilum'
                elif change=='wrong_original_sha': f.data['original']['commit_sha']='f'*40
                elif change=='different_main': f.main='f'*40
                elif change=='red_main': f.main_ci['statuses'][0]['status']='failure'
                elif change=='wrong_execution': f.own['workflow_id']='ci.yml'
                elif change=='wrong_inputs': f.own['event_payload']='{}'
                elif change=='rerun_execution': f.own_jobs[0]['attempt']=2
                elif change=='new_pr_source': f.data['pr']['head']['sha']='f'*40
                elif change=='draft': f.data['pr']['draft']=True
                elif change=='closed': f.data['pr']['state']='closed'
                with self.assertRaises(ValueError): self.prepare(f,Path(root)/'proof')
                self.assertTrue(all(c[0]=='GET' for c in f.calls))

    def test_upload_failure_missing_artifact_or_changed_head_never_reports(self):
        for change in ['upload','artifact','source']:
            with self.subTest(change=change), tempfile.TemporaryDirectory() as root:
                f=Forgejo(); proof=self.prepare(f,Path(root)/'proof')
                if change=='artifact': f.artifacts={'artifacts':[]}
                if change=='source': f.data['pr']['head']['sha']='f'*40
                with self.assertRaises(ValueError):
                    bridge.report_gates(f,proof,evidence_uploaded='failure' if change=='upload' else 'success')
                self.assertFalse(any(c[0]=='POST' for c in f.calls))

    def test_partial_two_status_failure_leaves_required_check_failed_without_retry(self):
        f=Forgejo();f.fail_second_post=True
        with tempfile.TemporaryDirectory() as root:
            proof=self.prepare(f,Path(root)/'proof')
            with self.assertRaises(bridge.Unavailable): bridge.report_gates(f,proof,evidence_uploaded='success')
        posts=[c for c in f.calls if c[0]=='POST']
        self.assertEqual(len(posts),3)
        self.assertEqual(next(s['status'] for s in f.data['statuses']['statuses']
                              if s['context']=='ci / check (pull_request)'), 'failure')

    def test_context_rejects_interactive_fork_nonmain_and_rerun(self):
        env={'GITHUB_REPOSITORY':bridge.REPOSITORY,'GITHUB_REF':'refs/heads/main',
             'GITHUB_EVENT_NAME':'workflow_dispatch','GITHUB_RUN_ATTEMPT':'1',
             'GITHUB_JOB':'reconcile','GITHUB_SHA':TOOLING,'GITHUB_RUN_ID':'999','GITHUB_RUN_NUMBER':'888'}
        self.assertEqual(bridge.gate_execution(env,TOOLING),EXECUTION)
        for key,value in [('GITHUB_EVENT_NAME','pull_request'),('GITHUB_REF','refs/heads/feature'),
                          ('GITHUB_REPOSITORY','fork/okilum'),('GITHUB_RUN_ATTEMPT','2'),
                          ('GITHUB_JOB','unrelated'),('GITHUB_SHA','f'*40)]:
            with self.subTest(key=key),self.assertRaises(ValueError):
                bridge.gate_execution(dict(env,**{key:value}),TOOLING)

    def test_workflow_uses_main_tooling_explicit_source_and_evidence_before_report(self):
        text=(Path(__file__).resolve().parents[2]/'.forgejo/workflows/ci-reconcile.yml').read_text()
        self.assertIn('ref: ${{ github.sha }}',text)
        self.assertIn('persist-credentials: false',text)
        self.assertLess(text.index('id: evidence'),text.index('--phase report'))
        self.assertNotIn('cargo ',text)
        self.assertNotIn('github-macos.py run',text)
        self.assertNotIn('pull_request:',text)


if __name__ == '__main__':
    unittest.main()
