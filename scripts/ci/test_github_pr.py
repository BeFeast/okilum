"""Hosted lanes cannot green a stale head, wrong job, or skipped acceptance step."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('pr_bridge', Path(__file__).with_name('github-pr.py'))
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)
SHA = 'a' * 40


class HostedPRTests(unittest.TestCase):
    def test_every_lane_uses_its_own_workflow_job_and_executed_step(self):
        for lane, (job, step, timeout) in bridge.LANES.items():
            with self.subTest(lane=lane), tempfile.TemporaryDirectory() as tmp:
                env = dict(CI_LANE=lane, MIRROR_TOKEN='test', PR_HEAD_SHA=SHA, PR_NUMBER='12',
                           GITHUB_RUN_ID='345', GITHUB_OUTPUT=str(Path(tmp) / 'output'))
                with patch.dict(os.environ, env), patch.object(bridge.transport, 'push_head') as push, \
                        patch.object(bridge.transport, 'GitHub'), \
                        patch.object(bridge.transport, 'wait_for_run', return_value=('success', 'ok')) as wait:
                    self.assertEqual(bridge.main(), 0)
                branch = push.call_args.args[0]
                self.assertTrue(branch.startswith(f'forgejo-{lane}-pr/'))
                self.assertEqual(push.call_args.args[1], SHA)
                self.assertEqual(wait.call_args.args[1:], (branch, SHA))
                self.assertEqual(wait.call_args.kwargs, dict(workflow=f'.github/workflows/forgejo-{lane}.yml',
                    job_name=job, build_step=step, queue_timeout=1800, run_timeout=timeout))

    def test_unknown_lane_never_pushes(self):
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, {
                'CI_LANE': '../../main', 'GITHUB_OUTPUT': str(Path(tmp) / 'output')}), \
                patch.object(bridge.transport, 'push_head') as push:
            self.assertEqual(bridge.main(), 1)
            push.assert_not_called()

    def test_each_acceptance_checks_exact_identity_and_execution(self):
        for lane, (job, step, _) in bridge.LANES.items():
            branch = f'forgejo-{lane}-pr/12/{SHA}'
            workflow = f'.github/workflows/forgejo-{lane}.yml'
            good = dict(id=1, head_sha=SHA, head_branch=branch, event='push', path=workflow,
                        status='completed', conclusion='success', html_url='https://github.com/run/1')
            def check(run, conclusion='success'):
                class API:
                    def request(self, path, method='GET'):
                        return ({'jobs': [dict(name=job, conclusion='success', steps=[
                            dict(name=step, conclusion=conclusion)])]} if '/jobs?' in path
                            else {'workflow_runs': [run]})
                return bridge.transport.wait_for_run(API(), branch, SHA, workflow=workflow,
                    job_name=job, build_step=step, queue_timeout=0, clock=lambda: 1, sleep=lambda _: None)[0]
            with self.subTest(lane=lane):
                self.assertEqual(check(good), 'success')
                self.assertEqual(check(good, 'skipped'), 'failure')
                self.assertEqual(check(good, 'failure'), 'failure')
                for field, value in [('head_sha', 'b' * 40), ('head_branch', 'main'), ('path', 'other.yml')]:
                    with self.assertRaises(bridge.transport.Unavailable):
                        check(dict(good, **{field: value}))
