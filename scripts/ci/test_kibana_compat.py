"""The Kibana compatibility lane stays pinned, hosted-only, read-only and opt-in (#1440)."""
import importlib.util
import json
from pathlib import Path
import re
import unittest

import yaml

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
spec = importlib.util.spec_from_file_location('kibana_compat', HERE / 'kibana-compat.py')
compat = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compat)
WORKFLOW = ROOT / '.github/workflows/kibana-compat.yml'


class KibanaCompatTests(unittest.TestCase):
    def setUp(self):
        self.workflow = yaml.safe_load(WORKFLOW.read_text())

    def test_every_matrix_version_is_digest_pinned(self):
        versions = self.workflow['jobs']['compat']['strategy']['matrix']['version']
        self.assertEqual(sorted(versions), sorted(compat.IMAGES))
        for version in versions:
            for image in compat.images(version):
                self.assertRegex(image, rf'^docker\.elastic\.co/[a-z]+/[a-z]+:{re.escape(version)}@sha256:[0-9a-f]{{64}}$')

    def test_lane_is_opt_in_hosted_and_secret_free(self):
        triggers = self.workflow['on']
        self.assertEqual(set(triggers), {'schedule', 'workflow_dispatch', 'push'})
        self.assertEqual(triggers['push'], {'branches': ['kibana-compat/**']})
        self.assertEqual(self.workflow['permissions'], {'contents': 'read'})
        text = WORKFLOW.read_text()
        self.assertNotIn('secrets.', text)
        self.assertNotIn('rust-cache', text)
        for name, job in self.workflow['jobs'].items():
            with self.subTest(job=name):
                self.assertEqual(job['runs-on'], 'ubuntu-24.04')
                self.assertLessEqual(job['timeout-minutes'], 30)

    def test_refuses_anything_but_a_github_hosted_runner(self):
        for env in ({}, {'GITHUB_ACTIONS': 'true'},
                    {'GITHUB_ACTIONS': 'true', 'RUNNER_ENVIRONMENT': 'self-hosted'}):
            with self.subTest(env=env), self.assertRaises(SystemExit):
                compat.require_hosted(env)
        compat.require_hosted({'GITHUB_ACTIONS': 'true', 'RUNNER_ENVIRONMENT': 'github-hosted'})

    def test_api_key_is_read_only_and_short_lived(self):
        request = compat.api_key_request()
        self.assertEqual(request['expiration'], '1h')
        (role,) = request['role_descriptors'].values()
        self.assertEqual(role['cluster'], [])
        for grant in role['indices']:
            self.assertEqual(set(grant['privileges']), {'read', 'view_index_metadata'})
            self.assertTrue(all(name.startswith('okilum-') for name in grant['names']))
        for grant in role['applications']:
            self.assertTrue(all(p.endswith('.read') for p in grant['privileges']))

    def test_containers_and_runner_are_bounded(self):
        self.assertIn('--memory', compat.ES_LIMITS)
        self.assertIn('--memory', compat.KIBANA_LIMITS)
        self.assertLessEqual(compat.ES_START_SECONDS + compat.KIBANA_START_SECONDS + compat.RUNNER_SECONDS, 15 * 60)
        self.assertTrue(compat.ES_URL.startswith('http://127.0.0.1:'))
        self.assertTrue(compat.KIBANA_URL.startswith('http://127.0.0.1:'))

    def test_status_and_summary_parsing(self):
        self.assertTrue(compat.kibana_available(json.dumps({'status': {'overall': {'level': 'available'}}})))
        self.assertFalse(compat.kibana_available(json.dumps({'status': {'overall': {'level': 'degraded'}}})))
        self.assertFalse(compat.kibana_available('<html>'))
        self.assertFalse(compat.kibana_available(''))
        text = compat.summary('9.5.4', {'checks': [
            {'name': 'kql_phrase', 'outcome': 'unsupported', 'seconds': 0.1, 'detail': 'a | b\nc'}]})
        self.assertIn('| kql_phrase | unsupported | 0.1 | a \\| b c |', text)
        self.assertIn('Kibana 9.5.4', compat.summary('9.5.4', {}))


if __name__ == '__main__':
    unittest.main()
