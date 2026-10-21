import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('shipping_bridge_under_test', Path(__file__).with_name('github-macos-shipping.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class HostedProducer(unittest.TestCase):
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
