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


if __name__ == '__main__':
    unittest.main()
