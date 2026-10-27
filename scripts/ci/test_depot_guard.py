"""Offline Depot boundary tests; never create an account, queue a job or query history."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('depot_guard_test', Path(__file__).with_name('depot_guard.py'))
g = importlib.util.module_from_spec(spec)
spec.loader.exec_module(g)


def usage(minutes):
    return {'githubActionsJobs': [{'repo': 'BeFeast/okilum', 'jobs': [
        {'runner': 'depot-macos-15', 'minutesElapsed': minutes, 'minutesBilled': minutes}]}]}


class API:
    def __init__(self, age=0, label='depot-macos-15', total=1):
        self.calls = []
        self.age, self.label, self.total = age, label, total

    def request(self, path, method='GET'):
        self.calls.append((path, method))
        if path.startswith('actions/variables?'):
            return {'variables': [{'name': x, 'value': 'depot-macos-15'} for x in g.VARIABLES]}
        if method == 'DELETE':
            return None
        if '/jobs?' in path:
            return {'jobs': [{'name': 'macos', 'status': 'queued', 'labels': [self.label]}]}
        if '/runs?' in path:
            workflow = path.split('/')[2]
            created = g.datetime.datetime.fromtimestamp(2000-self.age, g.datetime.timezone.utc).isoformat()
            return {'total_count': self.total, 'workflow_runs': [{'id': 1, 'path': '.github/workflows/'+workflow,
                'head_branch': 'forgejo-pr/12/sha', 'event': 'push', 'created_at': created}]}
        raise AssertionError(path)


class DepotGuardTests(unittest.TestCase):
    def test_default_off_never_contacts_any_service(self):
        with patch.dict(os.environ, {}, clear=True), patch.object(g.transport, 'GitHub') as api:
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(g.main(), 0)
            api.assert_not_called()

    def test_budget_alert_at_sixty_percent_and_stop_at_ninety(self):
        self.assertEqual(g.budget(usage(3499), 500)[1:], (False, False))
        self.assertEqual(g.budget(usage(3500), 500)[1:], (True, False))
        self.assertEqual(g.budget(usage(5375), 500)[1:], (True, True))
        with self.assertRaises(ValueError):
            g.budget(usage(0), 499)

    def test_unpriced_wrong_repo_or_invalid_usage_rejected(self):
        for value in (-1, float('nan'), '1', None):
            with self.subTest(value=value), self.assertRaises(ValueError):
                g.budget(usage(value), 500)
        for key, value in [('repo', 'other/repo'), ('jobs', 'bad')]:
            bad = usage(1); bad['githubActionsJobs'][0][key] = value
            with self.assertRaises(ValueError):
                g.budget(bad, 500)
        bad = usage(1); bad['githubActionsJobs'][0]['jobs'][0]['runner'] = 'unknown'
        with self.assertRaises(ValueError):
            g.budget(bad, 500)

    def test_sixty_percent_emits_alert_without_disabling(self):
        api, lines = API(), []
        result = g.guard(api, usage(3500), 500, 2000, 1000, 3000, lines.append)
        self.assertEqual(result, 'healthy')
        self.assertTrue(any('60%' in line for line in lines))
        self.assertFalse(any(method == 'DELETE' for _, method in api.calls))

    def test_near_cap_disables_all_pilot_variables_without_cancelling_or_dispatching(self):
        api = API()
        self.assertEqual(g.guard(api, usage(5375), 500, 2000, 1000, 3000, lambda _: None), 'disabled')
        self.assertEqual([p for p, m in api.calls if m == 'DELETE'], ['actions/variables/'+v for v in g.VARIABLES])
        self.assertTrue(all(m in ('GET', 'DELETE') for _, m in api.calls))

    def test_queued_over_ten_minutes_disables_but_standard_runner_does_not(self):
        for age, label, expected in [(600, 'depot-macos-15', 'healthy'),
                                     (601, 'depot-macos-15', 'disabled'), (601, 'macos-15', 'healthy')]:
            api = API(age, label)
            with self.subTest(age=age, label=label):
                self.assertEqual(g.guard(api, usage(1), 500, 2000, 1000, 3000, lambda _: None), expected)

    def test_missing_queue_evidence_and_expiry_fail_closed(self):
        for api, now, end in [(API(total=2), 2000, 3000), (API(), 3000, 3000)]:
            self.assertEqual(g.guard(api, usage(1), 500, now, 1000, end, lambda _: None), 'disabled')

    def test_variable_permission_failure_never_claims_fallback(self):
        api = API()
        original = api.request
        def request(path, method='GET'):
            if method == 'DELETE':
                raise g.transport.Unavailable('HTTP403 permission denied')
            return original(path, method)
        api.request = request
        with self.assertRaises(g.transport.Unavailable):
            g.guard(api, usage(5375), 500, 2000, 1000, 3000, lambda _: None)

    def test_optional_guard_permission_does_not_change_normal_bridge_permissions(self):
        auth = g.transport.auth
        env = {'MIRROR_TOKEN': 'sentinel'}
        normal = auth.Credentials(env=env)
        guard = auth.Credentials(env=env, variable_guard=True)
        self.assertNotIn('variables', normal.permissions)
        self.assertEqual(guard.permissions['variables'], 'write')
        self.assertNotIn('variables', auth.PERMISSIONS)

    def test_arbitrary_token_permissions_and_nonboolean_guard_mode_are_rejected(self):
        auth = g.transport.auth
        with self.assertRaises(TypeError):
            auth.Credentials(env={'MIRROR_TOKEN': 'sentinel'}, permissions={'administration': 'write'})
        for value in ('true', 1, {}, None):
            with self.subTest(value=value), self.assertRaises(auth.CredentialError):
                auth.Credentials(env={'MIRROR_TOKEN': 'sentinel'}, variable_guard=value)

    def test_depot_http_json_usage_period_and_secret_redaction(self):
        start, now = 1000, 2000
        data = {'periodStart': '1970-01-01T00:16:40Z', 'periodEnd': '1970-01-01T00:33:20Z'}
        opener = Mock(return_value=io.BytesIO(json.dumps(data).encode()))
        self.assertEqual(g.depot_usage('secret', start, now, opener), data)
        sent = opener.call_args.args[0]
        self.assertEqual(sent.full_url, 'https://api.depot.dev/depot.core.v1.UsageService/GetUsage')
        self.assertEqual(sent.get_header('Authorization'), 'Bearer secret')
        with self.assertRaisesRegex(ValueError, 'withheld'):
            g.depot_usage('secret', start, now, Mock(side_effect=OSError('secret')))
        with self.assertRaises(ValueError):
            g.depot_usage('secret', start, now, Mock(return_value=io.BytesIO(b'{}')))

    def test_workflows_are_off_without_variables_and_do_not_save_shadow_cache(self):
        shadow = (ROOT/'.github/workflows/forgejo-macos-depot.yml').read_text()
        main = (ROOT/'.github/workflows/forgejo-macos.yml').read_text()
        self.assertIn("if: vars.OKILUM_DEPOT_MACOS_SHADOW == 'true' && vars.OKILUM_DEPOT_MACOS_RUNNER == 'depot-macos-15'", shadow)
        self.assertIn("vars.OKILUM_GITHUB_MACOS_RUNNER || 'macos-15'", main)
        self.assertNotIn('actions/cache/save', shadow)
        self.assertNotIn('secrets.', shadow)
        self.assertIn("if: vars.OKILUM_DEPOT_PILOT_ENABLED == 'true'", (ROOT/'.forgejo/workflows/depot-pilot-guard.yml').read_text())
        self.assertIn('ref: main', (ROOT/'.forgejo/workflows/depot-pilot-guard.yml').read_text())


if __name__ == '__main__':
    unittest.main()
