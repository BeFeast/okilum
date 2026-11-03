"""Offline hourly admission controls; no dispatch, signing or feed publication."""
import copy
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import runpy
import sys
import tempfile
import unittest
from unittest.mock import MagicMock, Mock, patch

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
                         [{'context': c.replace('(workflow_dispatch)', '(pull_request)'), 'status': 'success'}
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


class Certification(unittest.TestCase):
    def setUp(self):
        self.now = datetime(2026, 11, 3, 17, 17, tzinfo=timezone.utc)
        self.tip = SOURCE
        self.statuses = {}
        self.runs, self.dispatches, self.writes = [], [], []
        self.descriptor = Mock(return_value=None)
        self.env = {'GITHUB_REF': 'refs/heads/main', 'GITHUB_RUN_ID': '100'}
        def api(path, method='GET', body=None):
            if path == '/branches/main':
                return {'commit': {'id': self.tip}}
            if path.startswith('/commits/'):
                source = path.split('/')[2]
                statuses = self.statuses.get(source, [])
                return {'sha': source, 'statuses': statuses, 'total_count': len(statuses)}
            if path.startswith('/actions/runs?'):
                return {'total_count': len(self.runs), 'workflow_runs': self.runs}
            if path.startswith('/statuses/') and method == 'POST':
                self.writes.append((path, copy.deepcopy(body)))
                return None
            if path == '/actions/workflows/fast.yml/dispatches' and method == 'POST':
                self.dispatches.append(copy.deepcopy(body))
                self.runs.insert(0, self.make_run(body['inputs']['certification_source']))
                return None
            raise AssertionError((path, method, body))
        guard = patch.object(hourly, 'api', side_effect=api)
        guard.start(); self.addCleanup(guard.stop)
        clock = patch.object(hourly, 'datetime', wraps=datetime)
        self.clock = clock.start(); self.addCleanup(clock.stop)
        self.clock.now.side_effect = lambda *args: self.now
        network = patch('urllib.request.urlopen', side_effect=AssertionError('Offline fixture'))
        network.start(); self.addCleanup(network.stop)

    def make_run(self, source, state='running', created=None):
        return dict(workflow_id='fast.yml', prettyref='main', trigger_event='workflow_dispatch',
                    is_fork_pull_request=False, commit_sha=source, status=state,
                    created=(created or self.now).isoformat(),
                    event_payload=json.dumps({'inputs': {'certification_source': source}}))

    def select(self, platform='linux', event='schedule', source=None, **extra):
        return hourly.select(event, source or self.tip, platform, {**self.env, **extra}, self.descriptor)

    def green(self, source):
        self.statuses[source] = [dict(context=c, status='success') for c in hourly.REQUIRED]

    def test_sixty_merges_and_all_platform_ticks_request_at_most_one_certification_per_hour(self):
        # Merges themselves cannot run fast (see WorkflowContract). Only the
        # current tip at the hourly tick is certified, never intermediate heads.
        for n in range(60):
            self.tip = f'{n:040x}'
        first = self.tip
        for _ in range(2):  # A replayed Linux tick cannot dispatch a duplicate.
            for platform in hourly.WORKFLOWS:
                build, _, reason = self.select(platform)
                self.assertFalse(build)
                self.assertIn('no fallback', reason)
        self.assertEqual(self.dispatches, [{'ref': 'main', 'inputs': {'certification_source': first}}])
        self.assertEqual([path for path, _ in self.writes], ['/statuses/' + first] * 2)
        self.assertTrue(all(body['state'] == 'pending' for _, body in self.writes))
        self.descriptor.assert_not_called()
        # More merges in this hour still cannot allocate another certification.
        self.tip = 'c' * 40
        self.assertFalse(self.select()[0])
        self.assertEqual(len(self.dispatches), 1)
        self.now += timedelta(hours=1)
        self.assertFalse(self.select()[0])
        self.assertEqual(len(self.dispatches), 2)
        self.assertEqual(self.dispatches[-1]['inputs']['certification_source'], self.tip)

    def test_red_certification_never_builds_and_retry_waits_for_another_hour(self):
        self.assertFalse(self.select()[0])
        self.runs[0]['status'] = 'failure'
        self.green(SOURCE)
        self.statuses[SOURCE][1]['status'] = 'failure'
        for platform in hourly.WORKFLOWS:
            self.assertFalse(self.select(platform)[0])
        self.assertEqual(len(self.dispatches), 1)
        self.descriptor.assert_not_called()
        self.now += timedelta(hours=1)
        self.assertFalse(self.select()[0])
        self.assertEqual(len(self.dispatches), 2)

    def test_success_builds_later_tick_but_old_success_never_certifies_newest_main(self):
        self.assertFalse(self.select()[0])
        self.green(SOURCE)
        self.runs[0]['status'] = 'success'
        self.now += timedelta(hours=1)
        for platform in hourly.WORKFLOWS:
            self.assertEqual(self.select(platform)[:2], (True, SOURCE))
        self.assertEqual(len(self.dispatches), 1)
        self.tip = OLD
        for platform in hourly.WORKFLOWS:
            self.assertFalse(self.select(platform, source=SOURCE)[0])
        self.assertEqual(len(self.dispatches), 2)
        self.assertEqual(self.dispatches[-1]['inputs']['certification_source'], OLD)

    def test_pending_same_source_is_not_restarted_and_manual_or_pinned_retries_never_dispatch(self):
        self.runs = [self.make_run(SOURCE, created=self.now - timedelta(hours=1))]
        self.assertFalse(self.select()[0])
        self.assertFalse(self.dispatches)
        self.runs.clear()
        for platform in hourly.WORKFLOWS:
            self.assertFalse(self.select(platform, event='workflow_dispatch')[0])
            self.assertFalse(self.select(platform, event='workflow_dispatch', HOURLY_SOURCE=SOURCE)[0])
        env = {**self.env, 'GITHUB_SHA': SOURCE}
        for platform in ['linux', 'macos']:
            self.assertFalse(hourly.github_main(platform, env, self.descriptor)[0])
        self.assertFalse(self.dispatches)

    def test_capture_race_never_compiles_or_certifies_a_different_commit(self):
        env = {**self.env, 'CERTIFICATION_SOURCE': SOURCE,
               'GITHUB_SHA': SOURCE, 'GITHUB_EVENT_NAME': 'workflow_dispatch'}
        # Positive control reaches the executable validation entrypoint.
        response = MagicMock()
        response.__enter__.return_value.read.return_value = json.dumps({'commit': {'id': SOURCE}}).encode()
        with patch.dict(os.environ, env), patch.object(sys, 'argv', ['hourly_beta.py', 'certify-source']), \
                patch('urllib.request.urlopen', return_value=response):
            with self.assertRaises(SystemExit) as result:
                runpy.run_path(str(ROOT / 'scripts/releases/hourly_beta.py'), run_name='__main__')
            self.assertEqual(result.exception.code, 0)
        for changes in [dict(CERTIFICATION_SOURCE=OLD), dict(GITHUB_SHA=OLD),
                        dict(CERTIFICATION_SOURCE='main'), dict(GITHUB_REF='refs/heads/feature'),
                        dict(GITHUB_EVENT_NAME='pull_request')]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                hourly.certification_source({**env, **changes})
        self.tip = OLD
        with self.assertRaises(ValueError):
            hourly.certification_source(env)
        with patch.object(hourly, 'api', side_effect=[{'total_count': 0, 'workflow_runs': []},
                                                    {'commit': {'id': OLD}}]) as api:
            self.assertIn('advanced', hourly.certify(SOURCE, self.now))
            self.assertFalse(any(call.args[1:] for call in api.call_args_list))

    def test_inventory_pages_and_failures_cannot_duplicate_a_request(self):
        # A page full of unrelated manual dispatches must not hide this hour's
        # certification on the next page, nor prevent the first real request.
        manual = self.make_run(SOURCE)
        manual['event_payload'] = {'inputs': {}}
        pages = [{'total_count': 101, 'workflow_runs': [manual] * 100},
                 {'total_count': 101, 'workflow_runs': [self.make_run(OLD, 'cancelled')]}]
        with patch.object(hourly, 'api', side_effect=pages) as api:
            self.assertIn('already requested', hourly.certify(SOURCE, self.now))
            self.assertIn('page=2', api.call_args.args[0])
            self.assertEqual(api.call_count, 2)
        for data in [{'total_count': 1, 'workflow_runs': []},
                     {'total_count': 0, 'workflow_runs': [manual]},
                     {'total_count': 1, 'workflow_runs': [{**manual, 'prettyref': 'feature'}]}]:
            with patch.object(hourly, 'api', return_value=data), self.assertRaises(ValueError):
                hourly.certify(SOURCE, self.now)
        with patch.object(hourly, 'api', side_effect=TimeoutError('offline')), self.assertRaises(TimeoutError):
            hourly.certify(SOURCE, self.now)
        self.assertFalse(self.dispatches)


class StatusReporting(unittest.TestCase):
    def setUp(self):
        self.env = dict(GITHUB_SHA=SOURCE, GITHUB_REF='refs/heads/main',
                        GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_RUN_NUMBER='123',
                        GITHUB_SERVER_URL='https://git.oklabs.uk')
        guard = patch.object(hourly, 'api')
        self.api = guard.start(); self.addCleanup(guard.stop)
        network = patch('urllib.request.urlopen', side_effect=AssertionError('Offline fixture'))
        network.start(); self.addCleanup(network.stop)

    def test_both_certification_results_are_explicitly_reported_on_the_requested_sha(self):
        for result in ['success', 'failure', 'skipped', 'cancelled']:
            self.api.reset_mock()
            env = dict(self.env, CERTIFICATION_SOURCE=SOURCE, JOB_RESULTS=json.dumps({
                'check': {'result': 'success'}, 'shell-full': {'result': result}}))
            hourly.report_certification(env)
            calls = self.api.call_args_list
            self.assertEqual([call.args[:2] for call in calls], [('/statuses/' + SOURCE, 'POST')] * 2)
            self.assertEqual([call.args[2]['context'] for call in calls], list(hourly.REQUIRED))
            self.assertEqual([call.args[2]['state'] for call in calls],
                             ['success', 'failure' if result == 'cancelled' else result])
            self.assertEqual(calls[0].args[2]['target_url'],
                             'https://git.oklabs.uk/BeFeast/okilum/actions/runs/123')
        # A capture race reports failure on the requested SHA, never success on
        # the newer captured commit, even with seemingly successful job inputs.
        self.api.reset_mock()
        hourly.report_certification({**env, 'GITHUB_SHA': OLD})
        self.assertTrue(all(call.args[0] == '/statuses/' + SOURCE and call.args[2]['state'] == 'failure'
                            for call in self.api.call_args_list))
        for broken in [dict(JOB_RESULTS='{}'), dict(CERTIFICATION_SOURCE='main')]:
            self.api.reset_mock()
            with self.assertRaises(ValueError):
                hourly.report_certification({**env, **broken})
            self.api.assert_not_called()

    def test_stable_0900_shipping_contexts_land_on_each_beta_source_for_schedule_and_dispatch(self):
        for platform, jobs in hourly.SHIPPING_JOBS.items():
            for event in ['schedule', 'workflow_dispatch']:
                self.api.reset_mock()
                results = {job: {'result': 'skipped' if job.endswith('github') else 'success'} for job in jobs}
                env = dict(self.env, SOURCE=SOURCE, GITHUB_EVENT_NAME=event, JOB_RESULTS=json.dumps(results))
                hourly.report_shipping(platform, env)
                calls = self.api.call_args_list
                self.assertEqual(len(calls), len(jobs))
                self.assertEqual([call.args[2]['context'] for call in calls],
                                 [f'{platform}-release / {job} ({event})' for job in jobs])
                self.assertTrue(all(call.args[:2] == ('/statuses/' + SOURCE, 'POST') for call in calls))
                self.assertEqual([call.args[2]['state'] for call in calls], [results[j]['result'] for j in jobs])
                for broken in [dict(SOURCE=OLD), dict(GITHUB_SHA=OLD), dict(GITHUB_EVENT_NAME='pull_request'),
                               dict(GITHUB_REF='refs/heads/feature'), dict(JOB_RESULTS='{}')]:
                    self.api.reset_mock()
                    with self.assertRaises(ValueError):
                        hourly.report_shipping(platform, {**env, **broken})
                    self.api.assert_not_called()

    def test_red_or_cancelled_shipping_job_cannot_be_reported_as_green(self):
        for state in ['failure', 'cancelled']:
            results = {job: {'result': 'success'} for job in hourly.SHIPPING_JOBS['windows']}
            results['release']['result'] = state
            hourly.report_shipping('windows', dict(self.env, SOURCE=SOURCE, JOB_RESULTS=json.dumps(results)))
            release = next(call for call in self.api.call_args_list
                           if call.args[2]['context'] == 'windows-release / release (workflow_dispatch)')
            self.assertEqual(release.args[2]['state'], 'failure')
            self.api.reset_mock()


class ProducerInventory(unittest.TestCase):
    def test_inventory_reads_reuse_uncached_transport_with_only_supplied_read_token(self):
        import appimage_artifact
        transport = appimage_artifact.transport
        response = MagicMock()
        response.__enter__.return_value.read.return_value = b'{"workflow_runs": []}'
        response.__enter__.return_value.headers = {}
        with patch.dict(os.environ, OKILUM_GITHUB_APP_ID='invalid-unrelated-app'), \
                patch.object(transport.urllib.request, 'urlopen', return_value=response) as read:
            for _ in range(2):
                self.assertEqual(hourly.github_api('actions/runs?head_sha=' + SOURCE,
                                                   {'GH_TOKEN': 'read-fixture'}), {'workflow_runs': []})
        requests = [call.args[0] for call in read.call_args_list]
        self.assertEqual(len(requests), 2)  # No App authentication request.
        self.assertNotEqual(requests[0].full_url, requests[1].full_url)
        for request in requests:
            self.assertIn('head_sha=' + SOURCE, request.full_url)
            self.assertEqual(request.get_header('Cache-control'), 'no-cache')
            self.assertEqual(request.get_header('Authorization'), 'Bearer read-fixture')

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

    def test_hourly_certification_runs_the_same_fast_jobs_without_a_merge_trigger(self):
        data = self.load('.forgejo/workflows/fast.yml')
        self.assertEqual(set(data['on']), {'pull_request', 'workflow_dispatch'})
        self.assertIn('certification_source', data['on']['workflow_dispatch']['inputs'])
        self.assertIn("inputs.certification_source != '' && 'hourly-main'", data['concurrency']['group'])
        self.assertEqual(data['concurrency']['cancel-in-progress'], 'true')
        for name in ['check', 'shell-full']:
            job = data['jobs'][name]
            self.assertEqual(job['runs-on'], 'fast-linux')
            self.assertNotIn("!= 'push'", job['if'])
        self.assertEqual(data['jobs']['shell-full']['needs'], ['check'])
        check = data['jobs']['check']['steps']
        validation = next(s for s in check if s.get('run') == 'python3 scripts/releases/hourly_beta.py certify-source')
        self.assertEqual(validation['env']['CERTIFICATION_SOURCE'], '${{ inputs.certification_source }}')
        self.assertLess(check.index(validation), next(i for i, s in enumerate(check)
                        if s.get('name') == 'Prepare local Linux toolchain'))
        self.assertIn("inputs.certification_source == ''", data['jobs']['sync-source-sharing']['if'])
        report = data['jobs']['certification-status']
        self.assertEqual(report['needs'], ['check', 'shell-full'])
        self.assertEqual(report['if'], "always() && inputs.certification_source != ''")
        self.assertEqual(report['steps'][-1]['run'], 'python3 scripts/releases/hourly_beta.py certification-status')
        self.assertEqual(report['steps'][-1]['env']['JOB_RESULTS'], '${{ toJSON(needs) }}')

    def test_stable_selector_shipping_contexts_remain_on_the_beta_source_commit(self):
        # Dispatch/schedule runs need explicit reporting on this Forgejo. Stale
        # schedulers dispatch the source and reporting cannot move its statuses
        # to a newer checkout. Executable reporting controls are above.
        for platform, workflow in hourly.WORKFLOWS.items():
            data = self.load('.forgejo/workflows/' + workflow)
            self.assertEqual(data['name'], platform + '-release')
            self.assertIn('hourly_source', data['on']['workflow_dispatch']['inputs'])
            self.assertRegex(data['jobs']['select']['outputs']['source'], r'^\$\{\{ steps\.\w+\.outputs\.source \}\}$')
            report = data['jobs']['shipping-status']
            self.assertEqual(set(report['needs']), set(hourly.SHIPPING_JOBS[platform]))
            self.assertTrue(set(report['needs']) <= set(data['jobs']) - {'shipping-status'})
            if platform == 'linux':
                self.assertNotIn('release', report['needs'])  # Independent Arch cannot delay AppImage.
            self.assertIn("needs.select.outputs.build == 'true'", report['if'])
            self.assertIn('needs.select.outputs.source == github.sha', report['if'])
            self.assertEqual(report['steps'][-1]['run'], 'python3 scripts/releases/hourly_beta.py shipping-status ' + platform)
            self.assertEqual(report['steps'][-1]['env']['SOURCE'], '${{ needs.select.outputs.source }}')
            self.assertEqual(report['steps'][-1]['env']['JOB_RESULTS'], '${{ toJSON(needs) }}')
            for job in data['jobs'].values():
                for step in job.get('steps', []):
                    if step.get('uses', '').endswith('checkout@v4'):
                        self.assertNotEqual(step.get('with', {}).get('ref'), '${{ inputs.hourly_source }}')

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
