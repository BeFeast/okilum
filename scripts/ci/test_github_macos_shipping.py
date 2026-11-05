import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch
from unittest.mock import Mock
import sys
import json

spec = importlib.util.spec_from_file_location('shipping_bridge_under_test', Path(__file__).with_name('github-macos-shipping.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class HostedProducer(unittest.TestCase):
    def test_hourly_and_urgent_main_stage_beta_but_archive_and_verify_stay_artifacts_only(self):
        source = 'a' * 40
        env = {'GITHUB_REF': 'refs/heads/main', 'GITHUB_SHA': source}
        for event in ['schedule', 'workflow_dispatch', 'push']:
            selected = env | {'GITHUB_EVENT_NAME': event}
            self.assertTrue(m.beta_publication(selected, source))
            self.assertFalse(m.beta_publication(selected, 'b' * 40))
            self.assertFalse(m.beta_publication(selected | {'VERIFY_ONLY': 'true'}, source))
            self.assertFalse(m.beta_publication(selected | {'GITHUB_REF': 'refs/heads/branch'}, source))
        for event in ['pull_request', 'pull_request_target', 'repository_dispatch']:
            self.assertFalse(m.beta_publication(env | {'GITHUB_EVENT_NAME': event}, source))

    def test_verify_mirror_is_bare_exact_and_attested_only_after_live_recheck(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'releases'))
        import macos_verify_authorization as authorization
        api = Mock(token='fixture-token')
        api.request.return_value = {'sha': 'c' * 40}
        env = dict(GITHUB_RUN_ID='700', VERIFY_PR='2218')
        with patch.object(authorization, 'authorize', return_value='a' * 40) as admit, \
             patch.object(m.subprocess, 'run') as run:
            evidence = json.loads(m.prepare_verify_mirror(api, 'a' * 40, 'b' * 40, env))
            self.assertEqual(admit.call_count, 2)
            self.assertEqual(evidence['source'], 'a' * 40)
            commands = [c.args[0] for c in run.call_args_list]
            self.assertIn('--bare', commands[0])
            self.assertTrue(any('push' in c and 'a' * 40 + ':refs/heads/' + evidence['ref'] in c for c in commands))
            self.assertFalse(any('checkout' in c for c in commands))
            self.assertEqual(api.request.call_args.args[:2], ('git/refs', 'POST'))
            self.assertTrue(api.request.call_args.args[2]['ref'].startswith('refs/tags/okilum-verify-admission/'))
        api.reset_mock()
        with patch.object(authorization, 'authorize', side_effect=['a' * 40, ValueError('stale')]), \
             patch.object(m.subprocess, 'run'), self.assertRaises(ValueError):
            m.prepare_verify_mirror(api, 'a' * 40, 'b' * 40, env)
        api.request.assert_not_called()

    def test_only_exact_workflow_source_ref_event_owner_is_accepted(self):
        run = {'path': m.WORKFLOW, 'head_sha': 'b' * 40, 'head_branch': 'main',
               'event': 'workflow_dispatch', 'display_title': 'Mac shipping Forgejo 7 attempt 1'}
        self.assertIs(m.accepted_run([run], 'b' * 40, run['display_title']), run)
        for key in run:
            wrong = run | {key: 'wrong'}
            self.assertIsNone(m.accepted_run([wrong], 'b' * 40, run['display_title']))
        with self.assertRaises(ValueError):
            m.accepted_run([run, run], 'b' * 40, run['display_title'])

    @patch.object(m.subprocess, 'run')
    @patch.object(m.subprocess, 'check_output', return_value='b' * 40)
    def test_original_forgejo_number_not_github_number_and_no_reruns(self, output, run):
        env = {'GITHUB_REF': 'refs/heads/main', 'GITHUB_EVENT_NAME': 'workflow_dispatch',
               'PRODUCT_SOURCE': 'a' * 40, 'GITHUB_SHA': 'b' * 40,
               'GITHUB_RUN_ID': '700', 'GITHUB_RUN_NUMBER': '600', 'GITHUB_RUN_ATTEMPT': '1'}
        self.assertEqual(m.identity(env), ('a' * 40, 'b' * 40, '5600'))
        for delta in [{'GITHUB_RUN_ATTEMPT': '2'}, {'GITHUB_EVENT_NAME': 'pull_request'},
                      {'GITHUB_REF': 'refs/heads/branch'}, {'GITHUB_RUN_NUMBER': '0'}]:
            with self.assertRaises(ValueError):
                m.identity(env | delta)


class TransientGitHub(unittest.TestCase):
    """#2589: one GitHub 502 while polling failed every hourly Mac beta."""
    SOURCE, TOOLING, BUILD = 'a' * 40, 'b' * 40, '5600'
    LABEL = 'Mac shipping Forgejo 700 attempt 1'

    def run_bridge(self, failures):
        calls, sleeps = self.calls, self.sleeps = [], []
        outage = m.bridge.Unavailable(
            'GitHub GET actions/workflows/forgejo-macos-shipping.yml/runs: HTTP 502 (request failed)')
        remaining = {'runs': failures}

        def request(path, method='GET', data=None):
            calls.append((path.split('?')[0], method))
            if path == 'git/ref/heads/main':
                return {'object': {'sha': self.TOOLING}}
            if method == 'POST':
                return None
            if path.startswith('actions/workflows/forgejo-macos-shipping.yml/runs'):
                if remaining['runs']:
                    remaining['runs'] -= 1
                    raise outage
                return {'workflow_runs': [dict(
                    id=9, path=m.WORKFLOW, head_sha=self.TOOLING, head_branch='main',
                    event='workflow_dispatch', display_title=self.LABEL, status='completed',
                    conclusion='success', run_attempt=1, html_url='https://github.com/run/9')]}
            if path.startswith('actions/runs/9/jobs'):
                return {'jobs': [dict(name='shipping', conclusion='success',
                                      steps=[dict(name=m.STEP, conclusion='success')])]}
            if path.startswith('actions/runs/9/artifacts'):
                return {'artifacts': [dict(id=1, expired=False, digest='sha256:' + '0' * 64,
                                           name=f'macos-shipping-{self.SOURCE}-{self.BUILD}')]}
            raise AssertionError(path)
        api = Mock()
        api.request.side_effect = request
        env = {'GITHUB_RUN_ID': '700', 'GITHUB_RUN_NUMBER': '600', 'GITHUB_RUN_ATTEMPT': '1',
               'OKILUM_SIGNING_IDENTITY': 'fixture', 'OKILUM_NOTARY_PROFILE': 'fixture'}
        with patch.dict(m.os.environ, env, clear=True), \
                patch.object(m, 'identity', return_value=(self.SOURCE, self.TOOLING, self.BUILD)), \
                patch.object(m.bridge, 'GitHub', return_value=api), \
                patch.object(m.bridge, 'poll_delay', return_value=30), \
                patch.object(m.time, 'sleep', side_effect=sleeps.append):
            m.main()
        return calls, sleeps

    def test_transient_poll_failure_is_retried_and_the_build_is_still_collected(self):
        calls, sleeps = self.run_bridge(failures=2)
        dispatches = [c for c in calls if c[1] == 'POST']
        self.assertEqual(dispatches, [('actions/workflows/forgejo-macos-shipping.yml/dispatches', 'POST')])
        self.assertEqual(sleeps, [30, 30])
        self.assertIn(('actions/runs/9/artifacts', 'GET'), calls)

    def test_lasting_outage_still_fails_after_bounded_retries_without_redispatch(self):
        with self.assertRaises(m.bridge.Unavailable):
            self.run_bridge(failures=3)
        self.assertEqual([c for c in self.calls if c[1] == 'POST'],
                         [('actions/workflows/forgejo-macos-shipping.yml/dispatches', 'POST')])
        self.assertEqual(self.sleeps, [30, 30])


if __name__ == '__main__':
    unittest.main()
