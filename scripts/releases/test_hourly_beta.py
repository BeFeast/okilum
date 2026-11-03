"""Offline hourly admission controls; no dispatch, signing or feed publication."""
import copy
import os
from pathlib import Path
import runpy
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

import yaml
import coalesce
import hourly_beta as hourly

ROOT = Path(__file__).resolve().parents[2]
SOURCE, OLD = 'a' * 40, 'b' * 40


class Admission(unittest.TestCase):
    def setUp(self):
        self.status = {'sha': SOURCE, 'total_count': 2, 'statuses': [
            {'context': context, 'status': 'success'} for context in hourly.REQUIRED]}
        self.calls = []
        def api(path, method='GET', body=None):
            self.calls.append((path, method, body))
            if path == '/branches/main':
                return {'commit': {'id': SOURCE}}
            if path == f'/commits/{SOURCE}/status?limit=100':
                return self.status
            if method == 'POST' and path.endswith('/dispatches'):
                return None
            raise AssertionError((path, method, body))
        guard = patch.object(hourly, 'api', side_effect=api)
        guard.start(); self.addCleanup(guard.stop)
        network = patch('urllib.request.urlopen', side_effect=AssertionError('Offline fixture'))
        network.start(); self.addCleanup(network.stop)
        self.descriptor = Mock(return_value=None)
        self.env = {'GITHUB_REF': 'refs/heads/main', 'GITHUB_RUN_ID': '100'}

    def select(self, source=SOURCE, platform='macos', event='schedule', **env):
        return hourly.select(event, source, platform, {**self.env, **env}, self.descriptor)

    def test_positive_control_builds_all_three_only_with_both_merge_checks(self):
        for platform in hourly.WORKFLOWS:
            self.assertEqual(self.select(platform=platform)[:2], (True, SOURCE))
        self.assertEqual(self.descriptor.call_args_list[0].args, (SOURCE, 'linux-appimage'))
        self.assertFalse(any(method == 'POST' for _, method, _ in self.calls))

    def test_red_pending_missing_skipped_pr_only_or_duplicate_checks_cannot_build(self):
        original = copy.deepcopy(self.status)
        for index in range(2):
            for state in ['failure', 'pending', 'error', 'cancelled', 'skipped']:
                self.status = copy.deepcopy(original)
                self.status['statuses'][index]['status'] = state
                build, source, reason = self.select(source=OLD)
                self.assertFalse(build)
                self.assertEqual(source, OLD)  # no fallback to older green source
                self.assertIn('retry next hour', reason)
        for statuses in [[], original['statuses'][:1],
                         [{'context': c.replace('(push)', '(pull_request)'), 'status': 'success'}
                          for c in hourly.REQUIRED], original['statuses'] * 2]:
            self.status = {'sha': SOURCE, 'total_count': len(statuses), 'statuses': statuses}
            self.assertFalse(self.select()[0])
        self.descriptor.assert_not_called()
        self.assertFalse(any(method == 'POST' for _, method, _ in self.calls))

    def test_incomplete_wrong_sha_or_network_error_fails_closed(self):
        for field, value in [('sha', OLD), ('total_count', 3)]:
            with self.subTest(field=field), patch.dict(self.status, {field: value}):
                with self.assertRaises(ValueError):
                    self.select()
        with patch.object(hourly, 'api', side_effect=TimeoutError('offline')):
            with self.assertRaises(TimeoutError):
                self.select()
        self.descriptor.assert_not_called()

    def test_unchanged_hour_skips_every_platform_and_uses_appimage_not_frozen_pacman(self):
        for platform in hourly.WORKFLOWS:
            published_platform = 'linux-appimage' if platform == 'linux' else platform
            self.descriptor.return_value = {'platform': published_platform, 'source': SOURCE}
            self.assertFalse(self.select(platform=platform)[0])
            self.descriptor.assert_called_with(SOURCE, published_platform)
        self.assertFalse(any(method == 'POST' for _, method, _ in self.calls))
        for bad in [{'source': OLD, 'platform': 'macos'}, {'source': SOURCE, 'platform': 'linux'}]:
            self.descriptor.return_value = bad
            with self.assertRaises(ValueError):
                self.select()

    def test_stale_tick_dispatches_current_source_and_dispatch_race_cannot_build(self):
        for platform, workflow in hourly.WORKFLOWS.items():
            self.assertEqual(self.select(source=OLD, platform=platform)[:2], (False, OLD))
            self.assertEqual(self.calls[-1], ('/actions/workflows/' + workflow + '/dispatches',
                'POST', {'ref': 'main', 'inputs': {'hourly_source': SOURCE}}))
            self.assertTrue(self.select(platform=platform, event='workflow_dispatch', HOURLY_SOURCE=SOURCE)[0])
            self.assertFalse(self.select(source=OLD, platform=platform,
                                         event='workflow_dispatch', HOURLY_SOURCE=OLD)[0])
        count = len(self.calls)
        self.assertFalse(self.select(source=OLD, event='workflow_dispatch')[0])
        self.assertFalse(any(method == 'POST' for _, method, _ in self.calls[count:]))
        with self.assertRaises(ValueError):
            self.select(GITHUB_REF='refs/heads/untrusted')

    def test_macos_coalesce_reaches_shared_admission_and_never_routes_to_m4(self):
        for event in ['schedule', 'workflow_dispatch']:
            with patch.object(hourly, 'published_beta', self.descriptor):
                result = coalesce.select_macos(event, SOURCE, self.env, coalesce.datetime.now(coalesce.timezone.utc))
            self.assertEqual(result[:3], (True, SOURCE, False))

    def test_real_coalesce_entrypoints_publish_exact_source_outputs_for_all_platforms(self):
        for platform in hourly.WORKFLOWS:
            for event in ['schedule', 'workflow_dispatch']:
                with self.subTest(platform=platform, event=event), tempfile.TemporaryDirectory() as directory:
                    output = Path(directory) / 'outputs'
                    env = {**self.env, 'GITHUB_SHA': SOURCE, 'GITHUB_EVENT_NAME': event,
                        'GITHUB_OUTPUT': str(output), 'OKILUM_SOURCE': '', 'OKILUM_RESIGN_BUILD': '',
                        'MACOS_PREVIEW_PR': '', 'HOURLY_SOURCE': ''}
                    with patch.dict(os.environ, env), patch.object(sys, 'argv', ['coalesce.py', platform]), \
                            patch.object(hourly, 'published_beta', self.descriptor):
                        runpy.run_path(str(ROOT / 'scripts/releases/coalesce.py'), run_name='__main__')
                    values = dict(line.split('=', 1) for line in output.read_text().splitlines())
                    self.assertEqual(values['source'], SOURCE)
                    self.assertEqual(values['build'], 'true')
                    self.assertEqual(values['resign'], '')
                    if platform == 'macos':
                        self.assertEqual(values['local'], 'false')
                        self.assertEqual(values['preview'], 'false')

    def test_github_mirror_uses_same_green_gate_and_preserves_trusted_pr_proxy(self):
        env = {**self.env, 'GITHUB_SHA': SOURCE}
        with patch.object(hourly, 'github_produced', return_value=False) as produced:
            self.assertTrue(hourly.github_main('linux', env, self.descriptor)[0])
            produced.assert_called_once_with('linux', SOURCE, env)
            self.assertFalse(hourly.github_main('linux', {**env, 'GITHUB_SHA': OLD}, self.descriptor)[0])
            self.assertFalse(hourly.github_main('linux', {**env, 'HOURLY_SOURCE': OLD}, self.descriptor)[0])
        self.assertTrue(hourly.github_main('linux', {**env, 'GITHUB_REF': 'refs/heads/forgejo-linux-pr/1'},
                                          self.descriptor)[0])
        self.status['statuses'][0]['status'] = 'pending'
        with patch.object(hourly, 'github_produced') as produced:
            self.assertFalse(hourly.github_main('macos', env, self.descriptor)[0])
            produced.assert_not_called()


class ProducerInventory(unittest.TestCase):
    def test_urgent_manual_linux_dispatch_is_source_pinned_and_does_not_duplicate_bytes(self):
        import appimage_artifact
        client = Mock(token='fixture')
        client.request.return_value = {'object': {'sha': SOURCE}}
        with patch.object(appimage_artifact.transport, 'GitHub', return_value=client), \
                patch.object(hourly, 'github_produced', return_value=False) as produced:
            hourly.dispatch_linux(SOURCE)
            self.assertEqual(client.request.call_args.args, ('actions/workflows/forgejo-linux.yml/dispatches',
                'POST', {'ref': 'main', 'inputs': {'source': SOURCE}}))
            client.reset_mock()
            produced.return_value = True
            hourly.dispatch_linux(SOURCE)
            client.request.assert_called_once_with('git/ref/heads/main')
            client.request.return_value = {'object': {'sha': OLD}}
            with self.assertRaises(ValueError):
                hourly.dispatch_linux(SOURCE)

    def test_only_executed_original_exact_source_jobs_suppress_duplicate_builds(self):
        for platform, workflow, job, step in [
                ('linux', 'forgejo-linux.yml', 'appimage', 'Verify AppImage package'),
                ('macos', 'forgejo-macos.yml', 'macos', 'Compile Reader and run quick native regressions')]:
            run = dict(id=9, head_sha=SOURCE, head_branch='main', path='.github/workflows/' + workflow,
                       event='schedule', run_attempt=1, conclusion='success')
            jobs = {'total_count': 1, 'jobs': [dict(name=job, conclusion='success', head_sha=SOURCE,
                run_id=9, steps=[dict(name=step, conclusion='success')])]}
            env = {'GITHUB_RUN_ID': '10'}
            def check(r=run, j=jobs):
                with patch.object(hourly, 'github_api', side_effect=[{'total_count': 1, 'workflow_runs': [r]}, j]):
                    return hourly.github_produced(platform, SOURCE, env)
            self.assertTrue(check())
            for field, value in [('head_sha', OLD), ('head_branch', 'feature'), ('event', 'pull_request'),
                                 ('run_attempt', 2), ('conclusion', 'cancelled'), ('id', 10)]:
                self.assertFalse(check({**run, field: value}))
            for level in ['job', 'step']:
                broken = copy.deepcopy(jobs)
                target = broken['jobs'][0] if level == 'job' else broken['jobs'][0]['steps'][0]
                target['conclusion'] = 'skipped'
                self.assertFalse(check(j=broken))
            self.assertFalse(check(j={'total_count': 0, 'jobs': []}))
            with patch.object(hourly, 'github_api', return_value={'total_count': 2, 'workflow_runs': [run]}):
                with self.assertRaises(ValueError):
                    hourly.github_produced(platform, SOURCE, env)


class WorkflowContract(unittest.TestCase):
    def load(self, path):
        return yaml.load((ROOT / path).read_text(), Loader=yaml.BaseLoader)

    def test_post_merge_green_is_the_same_required_fast_jobs(self):
        data = self.load('.forgejo/workflows/fast.yml')
        self.assertEqual(data['on']['push']['branches'], ['main'])
        self.assertIn('pull_request', data['on'])
        for name in ['check', 'shell-full']:
            job = data['jobs'][name]
            self.assertEqual(job['runs-on'], 'fast-linux')
            self.assertNotIn("!= 'push'", job['if'])
        self.assertEqual(data['jobs']['shell-full']['needs'], ['check'])

    def test_public_proxies_drop_main_push_preserve_prs_and_hourly_admission(self):
        for name, branch in [('forgejo-linux', 'forgejo-linux-pr/**'), ('forgejo-macos', 'forgejo-pr/**')]:
            path = '.github/workflows/' + name + '.yml'
            data = self.load(path)
            self.assertEqual(data['on']['push']['branches'], [branch])
            self.assertIn({'cron': '17 * * * *'}, data['on']['schedule'])
            self.assertIn('workflow_dispatch', data['on'])
            self.assertEqual(data['concurrency']['cancel-in-progress'], 'true')
            self.assertIn('hourly_beta.py', (ROOT / path).read_text())
        shipping = self.load('.github/workflows/forgejo-macos-shipping.yml')
        self.assertEqual(set(shipping['on']), {'workflow_dispatch'})
        # Its caller now controls cadence. Preserve notarization/keychain cleanup.
        self.assertEqual(shipping['concurrency']['cancel-in-progress'], 'false')
        mac = self.load('.forgejo/workflows/macos-release.yml')
        self.assertTrue(mac['jobs']['release']['if'].startswith('false'))
        hosted = mac['jobs']['hosted-artifact']
        self.assertIn("github.event_name == 'schedule'", hosted['if'])
        self.assertTrue(any(step.get('run') == 'python3 scripts/releases/publication.py dispatch macos'
                            for step in hosted['steps']))


class CurrentFeeds(unittest.TestCase):
    def test_only_actual_beta_heads_suppress_work_and_stable_or_archive_does_not(self):
        import json
        from test_release_notes import Store, feed
        store = Store({'okilum/appcast.xml': feed((800, SOURCE, 'stable'), (700, OLD, 'beta')),
            'okilum/linux/beta/latest.json': json.dumps(dict(build=700, source=OLD,
                platform='linux-appimage', channel='beta')).encode(),
            'okilum/windows/beta/releases.beta.json': b'{"Assets":[{"Version":"0.1.700"}]}',
            'okilum/windows/builds/700/release.json': json.dumps(dict(build=700, source=OLD)).encode(),
            # A catalog alone is insufficient: it may be archive-only.
            f'okilum/releases/{SOURCE}/macos.json': json.dumps(dict(source=SOURCE, build=800)).encode()})
        with patch.object(hourly, 'PublicFeeds', return_value=store):
            for platform in ['linux-appimage', 'macos', 'windows']:
                self.assertIsNone(hourly.published_beta(SOURCE, platform))
                self.assertEqual(hourly.published_beta(OLD, platform),
                                 dict(source=OLD, build=700, platform=platform))
        with patch.object(hourly.subprocess, 'run') as run:
            run.return_value.stdout = b'feed\n200'
            self.assertEqual(hourly.PublicFeeds().call('GET', 'okilum/appcast.xml'), b'feed')
            run.return_value.stdout = b'\n404'
            self.assertIsNone(hourly.PublicFeeds().call('GET', 'okilum/appcast.xml'))
            for status in [b'403', b'500']:
                run.return_value.stdout = b'error\n' + status
                with self.assertRaises(ValueError):
                    hourly.PublicFeeds().call('GET', 'okilum/appcast.xml')
