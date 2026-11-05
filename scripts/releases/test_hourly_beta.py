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
import urllib.error
from unittest.mock import MagicMock, Mock, patch

import yaml
import coalesce
import hourly_beta as hourly

ROOT = Path(__file__).resolve().parents[2]
SOURCE, OLD = 'a' * 40, 'b' * 40
# Lanes still gated by fast certification; NEWEST_MAIN_BETA lanes ship main's tip.
CERTIFIED = [platform for platform in hourly.WORKFLOWS if platform not in hourly.NEWEST_MAIN_BETA]


class Admission(unittest.TestCase):
    def setUp(self):
        self.tip = SOURCE
        self.parents = {SOURCE: OLD, OLD: None}
        self.statuses = {SOURCE: [dict(context=c, status='success') for c in hourly.REQUIRED]}
        self.calls = []
        self.runs = []
        self.now = datetime(2026, 11, 3, 17, 17, tzinfo=timezone.utc)
        self.env = dict(GITHUB_REF='refs/heads/main', GITHUB_RUN_ID='100')
        self.descriptor = Mock(return_value=None)
        def api(path, method='GET', body=None):
            self.calls.append((path, method, body))
            if path == '/branches/main':
                return {'commit': {'id': self.tip}}
            if path.startswith('/git/commits/'):
                source = path.rsplit('/', 1)[1]
                parent = self.parents[source]
                return dict(sha=source, parents=[dict(sha=parent)] if parent else [])
            if path.startswith('/commits/'):
                source = path.split('/')[2]
                statuses = self.statuses.get(source, [])
                return dict(sha=source, total_count=len(statuses), statuses=statuses)
            if path.startswith('/actions/runs?'):
                return dict(total_count=len(self.runs), workflow_runs=self.runs)
            if method == 'POST':
                if path == '/actions/workflows/fast.yml/dispatches':
                    self.runs.insert(0, dict(workflow_id='fast.yml', prettyref='main',
                        trigger_event='workflow_dispatch', is_fork_pull_request=False,
                        created=self.now.isoformat(), status='running',
                        event_payload={'inputs': body['inputs']}))
                return None
            raise AssertionError((path, method, body))
        guard = patch.object(hourly, 'api', side_effect=api)
        guard.start(); self.addCleanup(guard.stop)
        guard = patch.object(hourly, 'datetime', wraps=datetime)
        clock = guard.start(); self.addCleanup(guard.stop)
        clock.now.return_value = self.now
        guard = patch('urllib.request.urlopen', side_effect=AssertionError('Offline fixture'))
        guard.start(); self.addCleanup(guard.stop)

    def select(self, platform='macos', event='workflow_dispatch', source=None, **extra):
        return hourly.select(event, source or self.tip, platform, {**self.env, **extra}, self.descriptor)

    def writes(self, path=None):
        return [(p, b) for p, m, b in self.calls if m == 'POST' and (path is None or p == path)]

    def test_positive_control_builds_each_platform_with_both_certification_checks(self):
        for platform in hourly.WORKFLOWS:
            self.assertEqual(self.select(platform)[:2], (True, SOURCE))
        self.assertFalse(self.writes())

    def test_main_advances_during_certification_and_each_platform_builds_certified_source(self):
        newer = 'c' * 40
        self.tip = newer
        self.parents[newer] = SOURCE
        for platform in CERTIFIED:
            self.assertEqual(self.select(platform, HOURLY_SOURCE=SOURCE)[:2], (True, SOURCE))
        # Mac beta ships the newest main: an older pin is superseded, not built.
        self.assertEqual(self.select('macos', HOURLY_SOURCE=SOURCE)[:2], (False, newer))
        self.assertFalse(any(path == '/git/commits/' + OLD for path, _, _ in self.calls))
        # Also recover from a missed completion dispatch at the next ordinary tick.
        for platform in hourly.WORKFLOWS:
            self.assertEqual(self.select(platform, event='schedule')[:2], (False, newer))
        self.assertEqual(len(self.writes('/actions/workflows/fast.yml/dispatches')), 1)
        self.assertEqual(self.writes('/actions/workflows/fast.yml/dispatches')[0][1],
                         dict(ref='main', inputs={'certification_source': newer}))

    def test_newest_green_ahead_of_each_feed_wins_and_already_published_never_repeats(self):
        newer = 'c' * 40
        self.tip = newer
        self.parents[newer] = SOURCE
        self.statuses[newer] = copy.deepcopy(self.statuses[SOURCE])
        self.descriptor.return_value = dict(source=OLD, platform='macos')
        self.assertEqual(self.select()[:2], (True, newer))
        # A queued pin remains valid across red main merges, but a newer
        # certified candidate wins admission once it is available.
        self.assertFalse(self.select(HOURLY_SOURCE=SOURCE)[0])
        self.descriptor.return_value = dict(source=newer, platform='macos')
        self.assertFalse(self.select()[0])
        self.assertFalse(self.select(HOURLY_SOURCE=SOURCE)[0])
        self.assertFalse(self.writes())

    def test_mac_beta_ships_newest_main_not_the_older_certified_source(self):
        # #2589 (04.11): main moved past the last certified source and its fast
        # check went red. Mac beta then shipped the hours-old certified a9034db9
        # and Sparkle replaced a newer local build with it.
        newer = 'c' * 40
        self.tip = newer
        self.parents[newer] = SOURCE
        self.statuses[newer] = [dict(context=hourly.REQUIRED[0], status='failure')]
        self.descriptor.return_value = dict(source=OLD, platform='macos')
        self.assertEqual(self.select('macos', event='schedule')[:2], (False, newer))
        self.assertEqual(self.writes()[-1], ('/actions/workflows/macos-release.yml/dispatches',
                                             dict(ref='main', inputs={'hourly_source': newer})))
        self.assertEqual(self.select('macos', HOURLY_SOURCE=newer)[:2], (True, newer))
        # An older pin (including a certification dispatch) is superseded.
        build, _, reason = self.select('macos', HOURLY_SOURCE=SOURCE)
        self.assertFalse(build)
        self.assertIn('superseded', reason)
        # Certified lanes keep their gate on the same history.
        self.descriptor.side_effect = lambda tip, platform: dict(source=OLD, platform=platform)
        for platform in CERTIFIED:
            self.assertEqual(self.select(platform, HOURLY_SOURCE=SOURCE)[:2], (True, SOURCE))
        self.descriptor.side_effect = None
        # The Mac lane never reads statuses or requests certification.
        self.assertFalse(self.writes('/actions/workflows/fast.yml/dispatches'))
        self.calls.clear()
        self.select('macos', HOURLY_SOURCE=newer)
        self.assertFalse([path for path, _, _ in self.calls if path.startswith('/commits/')])
        # Once the tip is the beta nothing repeats, and the feed never goes backwards.
        self.descriptor.return_value = dict(source=newer, platform='macos')
        self.assertFalse(self.select('macos')[0])
        self.assertFalse(self.select('macos', event='schedule')[0])
        self.descriptor.return_value = dict(source='f' * 40, platform='macos')
        with self.assertRaises(ValueError):
            self.select('macos')

    def test_source_and_build_cannot_go_backwards_when_a_newer_beta_exists(self):
        self.statuses[OLD] = copy.deepcopy(self.statuses[SOURCE])
        self.statuses[SOURCE][0]['status'] = 'failure'
        self.descriptor.return_value = dict(source=SOURCE, platform='macos')
        self.assertFalse(self.select()[0])
        self.assertFalse(self.select(HOURLY_SOURCE=OLD)[0])
        self.descriptor.return_value = dict(source='f' * 40, platform='macos')
        with self.assertRaises(ValueError):
            self.select()

    def test_red_missing_pending_duplicate_or_pr_checks_never_authorize_beta(self):
        original = copy.deepcopy(self.statuses[SOURCE])
        for platform in CERTIFIED:
            for state in ['failure', 'pending', 'error', 'cancelled', 'skipped']:
                self.statuses[SOURCE] = copy.deepcopy(original)
                self.statuses[SOURCE][0]['status'] = state
                self.assertFalse(self.select(platform)[0])
            for statuses in [[], original[1:], original * 2,
                             [dict(context=c.replace('workflow_dispatch', 'pull_request'), status='success')
                              for c in hourly.REQUIRED]]:
                self.statuses[SOURCE] = statuses
                self.assertFalse(self.select(platform)[0])
        self.assertFalse(self.writes())

    def test_red_or_missing_shell_full_never_holds_beta_once_check_is_green(self):
        original = copy.deepcopy(self.statuses[SOURCE])
        for statuses in [original[:1], [original[0], dict(original[1], status='failure')],
                         [original[0], dict(original[1], status='pending')]]:
            self.statuses[SOURCE] = copy.deepcopy(statuses)
            self.assertEqual(self.select()[:2], (True, SOURCE))
        self.assertFalse(self.writes())

    def test_only_linux_tick_certifies_at_most_once_in_an_hour(self):
        self.statuses.clear()
        for _ in range(2):
            for platform in hourly.WORKFLOWS:
                self.assertFalse(self.select(platform, event='schedule')[0])
        self.assertEqual(len(self.writes('/actions/workflows/fast.yml/dispatches')), 1)
        self.tip = OLD
        self.assertFalse(self.select('linux', event='schedule')[0])
        self.assertEqual(len(self.writes('/actions/workflows/fast.yml/dispatches')), 1)
        self.runs[0]['status'] = 'failure'
        self.assertFalse(self.select('linux', event='schedule')[0])
        for platform in CERTIFIED:
            self.assertFalse(self.select(platform, event='workflow_dispatch')[0])
        # Mac beta builds red main without asking for certification.
        self.assertEqual(self.select('macos', event='workflow_dispatch')[:2], (True, OLD))
        self.assertEqual(len(self.writes('/actions/workflows/fast.yml/dispatches')), 1)

    def test_pinned_and_red_candidates_never_trigger_certification_from_github_or_manual(self):
        self.statuses.clear()
        env = dict(self.env, GITHUB_SHA=SOURCE)
        self.assertFalse(hourly.github_main('linux', env, self.descriptor)[0])
        self.assertFalse(self.select('linux', event='workflow_dispatch', HOURLY_SOURCE=SOURCE)[0])
        # Red main is still the Mac beta, and the Mac lane never requests certification.
        with patch.object(hourly, 'github_produced', return_value=False):
            self.assertTrue(hourly.github_main('macos', env, self.descriptor)[0])
        self.assertEqual(self.select('macos', event='workflow_dispatch', HOURLY_SOURCE=SOURCE)[:2],
                         (True, SOURCE))
        self.assertFalse(self.writes())

    def test_github_uses_certified_ancestor_while_ordinary_pr_proxy_keeps_contract(self):
        newer = 'c' * 40
        self.tip = newer
        self.parents[newer] = SOURCE
        env = dict(self.env, GITHUB_SHA=SOURCE)
        with patch.object(hourly, 'github_produced', return_value=False):
            self.assertTrue(hourly.github_main('linux', env, self.descriptor)[0])
            pinned = dict(env, GITHUB_REF='refs/heads/forgejo-linux-pr/hourly-' + SOURCE)
            # Forgejo is LAN-only: the pinned ref is admitted without its API.
            # Forgejo verified the source before it created/dispatched the ref.
            self.calls.clear()
            self.assertTrue(hourly.github_main('linux', pinned, self.descriptor)[0])
            self.assertTrue(hourly.github_main('linux', dict(pinned, HOURLY_SOURCE=SOURCE), self.descriptor)[0])
            self.assertFalse(self.calls)
            self.assertFalse(hourly.github_main('linux', dict(pinned, HOURLY_SOURCE=OLD), self.descriptor)[0])
            self.descriptor.return_value = dict(source=SOURCE, platform='linux-appimage')
            self.assertFalse(hourly.github_main('linux', pinned, self.descriptor)[0])
            self.descriptor.return_value = None
            self.statuses[SOURCE][0]['status'] = 'failure'
            self.assertFalse(hourly.github_main('linux', env, self.descriptor)[0])
            # An unreachable Forgejo never authorizes, and never crashes, a main build.
            with patch.object(hourly, 'api', side_effect=urllib.error.URLError('timed out')):
                self.assertFalse(hourly.github_main('linux', env, self.descriptor)[0])
        self.assertTrue(hourly.github_main('linux', dict(env, GITHUB_REF='refs/heads/forgejo-linux-pr/1'),
                                          self.descriptor)[0])

    def test_stale_scheduler_dispatches_current_tooling_without_compiling_wrong_source(self):
        for platform, workflow in hourly.WORKFLOWS.items():
            self.assertEqual(self.select(platform, source=OLD)[:2], (False, OLD))
            self.assertEqual(self.writes()[-1], ('/actions/workflows/' + workflow + '/dispatches',
                dict(ref='main', inputs={'hourly_source': SOURCE})))

    def test_coalesce_entrypoints_emit_selected_ancestor_for_each_platform(self):
        newer = 'c' * 40
        self.tip = newer
        self.parents[newer] = SOURCE
        # Certified lanes build the pinned certified ancestor; the Mac lane
        # builds only main's tip and lets an older pin fall through.
        cases = [(platform, SOURCE, SOURCE, 'true') for platform in CERTIFIED] + [
            ('macos', newer, newer, 'true'), ('macos', SOURCE, newer, 'false')]
        for platform, pinned, source, build in cases:
            with self.subTest(platform=platform, pinned=pinned), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / 'outputs'
                env = dict(self.env, GITHUB_SHA=newer, GITHUB_EVENT_NAME='workflow_dispatch',
                    GITHUB_OUTPUT=str(output), OKILUM_SOURCE='', OKILUM_RESIGN_BUILD='',
                    MACOS_PREVIEW_PR='', HOURLY_SOURCE=pinned)
                with patch.dict(os.environ, env), patch.object(sys, 'argv', ['coalesce.py', platform]), \
                        patch.object(hourly, 'published_beta', self.descriptor):
                    runpy.run_path(str(ROOT / 'scripts/releases/coalesce.py'), run_name='__main__')
                values = dict(line.split('=', 1) for line in output.read_text().splitlines())
                self.assertEqual(values['source'], source)
                self.assertEqual(values['build'], build)

    def test_bad_status_inventory_history_or_network_fails_closed(self):
        for data in [dict(sha=OLD, total_count=0, statuses=[]),
                     dict(sha=SOURCE, total_count=1, statuses=[])]:
            with patch.object(hourly, 'api', return_value=data), self.assertRaises(ValueError):
                hourly.green(SOURCE)
        with patch.object(hourly, 'api', side_effect=TimeoutError('offline')), self.assertRaises(TimeoutError):
            self.select()
        self.parents[OLD] = SOURCE
        with self.assertRaises(ValueError):
            list(hourly.main_history(SOURCE))

    def test_certification_validates_pinned_checkout_after_main_and_run_snapshot_advance(self):
        self.tip = 'c' * 40
        self.parents[self.tip] = SOURCE
        env = dict(self.env, CERTIFICATION_SOURCE=SOURCE, GITHUB_SHA=self.tip,
                   GITHUB_EVENT_NAME='workflow_dispatch')
        with patch.object(hourly.subprocess, 'check_output', side_effect=[self.tip, SOURCE]), \
                patch.object(hourly.subprocess, 'run') as run:
            hourly.certification_source(env)
            self.assertEqual(run.call_args_list[-1].args[0], ['git', 'checkout', '--detach', SOURCE])
        for change in [dict(CERTIFICATION_SOURCE='main'), dict(GITHUB_REF='refs/heads/feature'),
                       dict(GITHUB_EVENT_NAME='pull_request')]:
            with self.subTest(change=change), self.assertRaises(ValueError):
                hourly.certification_source(dict(env, **change))
        with patch.object(hourly.subprocess, 'check_output', return_value=SOURCE), self.assertRaises(ValueError):
            hourly.certification_source(env)
        with patch.object(hourly.subprocess, 'check_output', side_effect=[self.tip, self.tip]), \
                patch.object(hourly.subprocess, 'run'), self.assertRaises(ValueError):
            hourly.certification_source(env)

    def test_pending_certification_and_paginated_hourly_inventory_cannot_duplicate_requests(self):
        self.statuses.clear()
        self.select('linux', event='schedule')
        self.runs[0]['created'] = (self.now - timedelta(hours=1)).isoformat()
        self.assertFalse(self.select('linux', event='schedule')[0])
        self.assertEqual(len(self.writes('/actions/workflows/fast.yml/dispatches')), 1)
        manual = dict(self.runs[0], created=self.now.isoformat(), event_payload={'inputs': {}})
        pages = [dict(total_count=101, workflow_runs=[manual] * 100),
                 dict(total_count=101, workflow_runs=[dict(self.runs[0], created=self.now.isoformat())])]
        with patch.object(hourly, 'api', side_effect=pages) as api:
            self.assertIn('already requested', hourly.certify(SOURCE, self.now))
            self.assertIn('page=2', api.call_args.args[0])
        for data in [dict(total_count=1, workflow_runs=[]), dict(total_count=0, workflow_runs=[manual])]:
            with patch.object(hourly, 'api', return_value=data), self.assertRaises(ValueError):
                hourly.certify(SOURCE, self.now)


class PublicationGuards(unittest.TestCase):
    def test_certified_older_product_reaches_beta_and_red_or_regressive_product_does_not(self):
        import io
        import zipfile
        from types import SimpleNamespace
        import publication
        newer = 'c' * 40
        data = io.BytesIO()
        with zipfile.ZipFile(data, 'w') as archive:
            archive.writestr('payload.zip', b'fixture')
            archive.writestr('release-source.txt', SOURCE)
            archive.writestr('release.json', json.dumps({'SOURCE_SHA': SOURCE}))
        for platform in ['macos', 'windows']:
            run = dict(workflow_id=hourly.WORKFLOWS[platform], prettyref='main',
                is_fork_pull_request=False, trigger_event='workflow_dispatch', status='success',
                commit_sha=newer, index_in_repo=42, event_payload={'inputs': {'hourly_source': SOURCE}})
            names = list(publication.ARTIFACTS[platform])
            artifacts = [dict(id=i, name=n, expired=False, run_id=12) for i, n in enumerate(names)]
            def api(method, path, raw=False):
                if path == '/actions/runs/12': return run
                if path == '/branches/main': return {'commit': {'id': newer}}
                if path.endswith('/artifacts'): return artifacts
                if path.endswith('/zip'): return data.getvalue()
                raise AssertionError(path)
            for state, prior, relation, publishes in [
                    (True, OLD, 0, True), (False, OLD, 0, False),
                    (True, newer, 1, False), (True, SOURCE, 0, False)]:
                client = Mock(); client.call.side_effect = api
                with self.subTest(platform=platform, green=state, prior=prior), \
                        patch.object(hourly, 'main_source'), patch.object(hourly, 'green', return_value=(state, 'gate')), \
                        patch.object(publication, 'R2') as store, \
                        patch.object(publication.release_notes, 'predecessor', return_value=dict(source=prior, build=5041)), \
                        patch.object(publication.release_notes, 'prepare', return_value=dict(previous_source=prior, previous_build=5041)), \
                        patch.object(publication.release_notes, 'record'), patch.object(publication.prepare, 'complete'), \
                        patch.object(publication.subprocess, 'run', side_effect=lambda command, **kw: SimpleNamespace(
                            returncode=relation if command == ['git', 'merge-base', '--is-ancestor', prior, SOURCE] else 0)) as launch:
                    store.return_value.call.return_value = None
                    # The newest-main Mac lane never waits for certification.
                    newest = platform in hourly.NEWEST_MAIN_BETA
                    if not state and not newest:
                        with self.assertRaisesRegex(ValueError, 'certified green'):
                            publication.publish(client, platform, 12)
                    else:
                        publication.publish(client, platform, 12)
                    commands = [c for c in launch.call_args_list if c.args[0][0] == publication.sys.executable]
                    self.assertEqual(bool(commands), publishes or (newest and prior == OLD))
                    if publishes:
                        self.assertEqual(commands[0].kwargs['env']['GITHUB_SHA'], SOURCE)
                        self.assertNotIn('--archive-only', commands[0].args[0])


class StatusReporting(unittest.TestCase):
    def setUp(self):
        self.env = dict(GITHUB_SHA=SOURCE, GITHUB_REF='refs/heads/main',
                        GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_RUN_NUMBER='123',
                        GITHUB_SERVER_URL='https://git.oklabs.uk')
        guard = patch.object(hourly, 'main_source')
        guard.start(); self.addCleanup(guard.stop)
        guard = patch.object(hourly, 'api')
        self.api = guard.start(); self.addCleanup(guard.stop)
        network = patch('urllib.request.urlopen', side_effect=AssertionError('Offline fixture'))
        network.start(); self.addCleanup(network.stop)

    def test_both_certification_results_are_explicitly_reported_on_the_requested_sha(self):
        for result in ['success', 'failure', 'skipped', 'cancelled']:
            for job, context in zip(['check', 'shell-full'], hourly.REQUIRED):
                self.api.reset_mock()
                env = dict(self.env, CERTIFICATION_SOURCE=SOURCE,
                           JOB_RESULTS=json.dumps({job: {'result': result}}))
                hourly.report_certification(env)
                calls = [c for c in self.api.call_args_list if c.args[0].startswith('/statuses/')]
                self.assertEqual([call.args[:2] for call in calls], [('/statuses/' + SOURCE, 'POST')])
                self.assertEqual([call.args[2]['context'] for call in calls], [context])
                self.assertEqual([call.args[2]['state'] for call in calls],
                                 ['failure' if result == 'cancelled' else result])
                self.assertEqual(calls[0].args[2]['target_url'],
                                 'https://git.oklabs.uk/BeFeast/okilum/actions/runs/123')
        # The dispatch snapshot may advance; results still belong to the pinned checkout.
        self.api.reset_mock()
        hourly.report_certification({**env, 'GITHUB_SHA': OLD})
        self.assertTrue(all(call.args[0] == '/statuses/' + SOURCE for call in self.api.call_args_list))
        both = json.dumps({j: {'result': 'success'} for j in ['check', 'shell-full']})
        for broken in [dict(JOB_RESULTS='{}'), dict(JOB_RESULTS=both), dict(CERTIFICATION_SOURCE='main')]:
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
                calls = [c for c in self.api.call_args_list if c.args[0].startswith('/statuses/')]
                self.assertEqual(len(calls), len(jobs))
                self.assertEqual([call.args[2]['context'] for call in calls],
                                 [f'{platform}-release / {job} ({event})' for job in jobs])
                self.assertTrue(all(call.args[:2] == ('/statuses/' + SOURCE, 'POST') for call in calls))
                self.assertEqual([call.args[2]['state'] for call in calls], [results[j]['result'] for j in jobs])
                for broken in [dict(GITHUB_EVENT_NAME='pull_request'),
                               dict(GITHUB_REF='refs/heads/feature'), dict(JOB_RESULTS='{}')]:
                    self.api.reset_mock()
                    with self.assertRaises(ValueError):
                        hourly.report_shipping(platform, {**env, **broken})
                    self.api.assert_not_called()

    def test_green_certification_immediately_dispatches_all_platforms_even_after_main_moves(self):
        env = dict(self.env, GITHUB_SHA=OLD, CERTIFICATION_SOURCE=SOURCE,
                   JOB_RESULTS=json.dumps({'check': {'result': 'success'}}))
        hourly.report_certification(env)
        dispatches = [c.args for c in self.api.call_args_list if c.args[0].endswith('/dispatches')]
        self.assertEqual(dispatches, [('/actions/workflows/' + w + '/dispatches', 'POST',
            {'ref': 'main', 'inputs': {'hourly_source': SOURCE}}) for w in hourly.WORKFLOWS.values()])
        # Red check, or any shell-full result, never dispatches release work.
        for results in [{'check': {'result': 'failure'}}, {'shell-full': {'result': 'success'}},
                        {'shell-full': {'result': 'failure'}}]:
            self.api.reset_mock()
            hourly.report_certification(dict(env, JOB_RESULTS=json.dumps(results)))
            self.assertFalse(any(c.args[0].endswith('/dispatches') for c in self.api.call_args_list))

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

    def test_linux_producer_pins_source_ref_after_main_advance_and_reuses_existing_bytes(self):
        import appimage_artifact
        client = Mock(token='fixture')
        branch = 'forgejo-linux-pr/hourly-' + SOURCE
        with patch.object(appimage_artifact.transport, 'GitHub', return_value=client), \
                patch.object(hourly, 'main_source'), patch.object(hourly, 'green', return_value=(True, 'green')), \
                patch.object(hourly, 'github_produced', return_value=False) as produced:
            client.request.return_value = []
            hourly.dispatch_linux(SOURCE)
            self.assertEqual(client.request.call_args.args, ('git/refs', 'POST',
                {'ref': 'refs/heads/' + branch, 'sha': SOURCE}))
            client.reset_mock()
            client.request.return_value = [dict(ref='refs/heads/' + branch, object={'sha': SOURCE})]
            hourly.dispatch_linux(SOURCE)
            self.assertEqual(client.request.call_args.args, ('actions/workflows/forgejo-linux.yml/dispatches',
                'POST', {'ref': branch, 'inputs': {'source': SOURCE}}))
            client.reset_mock()
            produced.return_value = True
            hourly.dispatch_linux(SOURCE)
            client.request.assert_not_called()
            produced.return_value = False
            client.request.return_value = [dict(ref='refs/heads/' + branch, object={'sha': OLD})]
            with self.assertRaises(ValueError):
                hourly.dispatch_linux(SOURCE)

    def test_queued_source_producer_prevents_a_second_dispatch_before_jobs_exist(self):
        run = dict(id=9, head_sha=SOURCE, head_branch='main', path='.github/workflows/forgejo-linux.yml',
                   event='schedule', run_attempt=1, conclusion=None, status='queued')
        with patch.object(hourly, 'github_api', return_value={'total_count': 1, 'workflow_runs': [run]}) as api:
            self.assertTrue(hourly.github_produced('linux', SOURCE, {'GITHUB_RUN_ID': ''}, pending=True))
            self.assertEqual(api.call_count, 1)

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

    def test_main_retains_full_certification_without_pr_triggers(self):
        data = self.load('.forgejo/workflows/fast.yml')
        self.assertEqual(set(data['on']), {'schedule', 'workflow_dispatch'})
        self.assertIn('certification_source', data['on']['workflow_dispatch']['inputs'])
        self.assertEqual(data['concurrency']['group'],
                         "fast-${{ github.event_name }}-${{ github.ref }}-${{ inputs.certification_source != '' && github.run_id || 'ordinary' }}")
        self.assertEqual(data['concurrency']['cancel-in-progress'],
                         "${{ github.event_name == 'workflow_dispatch' && inputs.certification_source == '' }}")
        for name in ['check', 'shell-full']:
            job = data['jobs'][name]
            self.assertEqual(job['runs-on'], 'fast-linux')
            self.assertNotIn("!= 'push'", job.get('if', ''))
        self.assertEqual(data['jobs']['shell-full']['needs'], ['check'])
        # Main, nightly and manual certification share bounded test entrypoints.
        for name, command in [
            ('check', 'fast-local-check 2100 bash scripts/ci/fast-local.sh'),
            ('shell-full', 'fast-local-shell-full 1980 bash scripts/ci/fast-local.sh shell-full'),
        ]:
            step = data['jobs'][name]['steps'][-1]
            self.assertNotIn('if', step)
            self.assertIn('bash "${OKILUM_TEST_GROUP:-scripts/ci/test-group.sh}" ' + command, step['run'])
        for directory in ['ci', 'releases', 'updater']:
            limit = 180 if directory == 'ci' else 90
            command = ('bash "${OKILUM_TEST_GROUP:-scripts/ci/test-group.sh}" ' + f'{directory}-python {limit} '
                       f"python3 -I -m unittest discover -s scripts/{directory} -p 'test_*.py' -v")
            step = next(s for s in data['jobs']['check']['steps'] if s.get('run') == command)
            self.assertNotIn('if', step)
            self.assertEqual(step['timeout-minutes'], '5')
        for name in ['check', 'shell-full']:
            steps = data['jobs'][name]['steps']
            validation = next(s for s in steps if 'certification-source ' in s.get('run', ''))
            self.assertIn('bash "$OKILUM_TEST_GROUP" certification-source 90 '
                          'python3 scripts/releases/hourly_beta.py certify-source', validation['run'])
            self.assertEqual(validation['timeout-minutes'], '2')
            self.assertEqual(validation['env']['CERTIFICATION_SOURCE'], '${{ inputs.certification_source }}')
            self.assertLess(steps.index(validation), next(i for i, s in enumerate(steps)
                            if s.get('name') == 'Prepare local Linux toolchain'))
        self.assertIn("inputs.certification_source == ''", data['jobs']['sync-source-sharing']['if'])
        # Beta's gate reports right after check; shell-full reports on its own.
        for name, needs in [('certification-status', ['check']), ('shell-full-status', ['shell-full'])]:
            report = data['jobs'][name]
            self.assertEqual(report['needs'], needs)
            self.assertEqual(report['if'], "always() && inputs.certification_source != ''")
            self.assertEqual(report['steps'][-1]['run'], 'python3 scripts/releases/hourly_beta.py certification-status')
            self.assertEqual(report['steps'][-1]['env']['JOB_RESULTS'], '${{ toJSON(needs) }}')

    def test_no_forgejo_workflow_can_enqueue_pr_jobs(self):
        for path in (ROOT / '.forgejo/workflows').glob('*.y*ml'):
            with self.subTest(workflow=path.name):
                events = self.load(path.relative_to(ROOT)).get('on', {})
                self.assertNotIn('pull_request', events)
                self.assertNotIn('pull_request_target', events)
        jobs = self.load('.forgejo/workflows/fast.yml')['jobs']
        self.assertNotIn('if', jobs['shell-full'])
        self.assertEqual(jobs['sync-source-sharing']['if'], "inputs.certification_source == ''")

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
            self.assertNotIn('needs.select.outputs.source == github.sha', report['if'])
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
                self.assertEqual(hourly.published_beta(SOURCE, platform), dict(source=OLD, build=700, platform=platform))
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
