"""Offline beta admission controls; no dispatch, signing or feed publication."""
import copy
from datetime import datetime, timezone
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
RED = [dict(context=c, status='failure') for c in hourly.REQUIRED]


class Admission(unittest.TestCase):
    def setUp(self):
        self.tip = SOURCE
        self.parents = {SOURCE: OLD, OLD: None}
        # Main certification is red, as it was on 04-05.11: beta must not care.
        self.statuses = {SOURCE: copy.deepcopy(RED)}
        self.calls = []
        self.runs = []
        self.now = datetime(2026, 11, 5, 17, 17, tzinfo=timezone.utc)
        self.env = dict(GITHUB_REF='refs/heads/main', GITHUB_RUN_ID='100')
        self.descriptor = Mock(return_value=None)
        def api(path, method='GET', body=None, timeout=None):
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
                return None
            raise AssertionError((path, method, body))
        guard = patch.object(hourly, 'api', side_effect=api)
        guard.start(); self.addCleanup(guard.stop)
        guard = patch('urllib.request.urlopen', side_effect=AssertionError('Offline fixture'))
        guard.start(); self.addCleanup(guard.stop)

    def select(self, platform='macos', event='workflow_dispatch', source=None, **extra):
        return hourly.select(event, source or self.tip, platform, {**self.env, **extra}, self.descriptor)

    def push(self, platform, source=None, **extra):
        return hourly.push_select(source or self.tip, platform, {**self.env, **extra}, self.descriptor)

    def writes(self, path=None):
        return [(p, b) for p, m, b in self.calls if m == 'POST' and (path is None or p == path)]

    def status_reads(self):
        return [path for path, _, _ in self.calls if path.startswith('/commits/')]

    def test_push_builds_the_tip_and_a_superseded_push_skips(self):
        # Oleg 05.11: a push to main starts each OS beta from that exact commit,
        # with no certification and no cron. Manager 13.11: a running beta is
        # never cancelled; a push run that starts after main moved on skips,
        # because the newer push's own run builds the tip.
        newer = 'c' * 40
        self.tip = newer
        self.parents[newer] = SOURCE
        for platform in hourly.WORKFLOWS:
            self.assertEqual(self.push(platform, newer)[:2], (True, newer))
            build, source, reason = self.push(platform, SOURCE)
            self.assertEqual((build, source), (False, SOURCE))
            self.assertIn('superseded by main ' + newer, reason)
        # Selection never reads statuses and never dispatches.
        self.assertFalse(self.status_reads())
        self.assertFalse(self.writes())

    def test_push_builds_when_main_tip_cannot_be_read(self):
        def broken(path, method='GET', body=None, timeout=None):
            raise OSError('offline')
        with patch.object(hourly, 'api', side_effect=broken):
            for platform in hourly.WORKFLOWS:
                build, source, reason = self.push(platform, SOURCE)
                self.assertEqual((build, source), (True, SOURCE))
                self.assertIn('main history unreadable', reason)

    def test_push_outside_recent_main_or_unreadable_history_builds(self):
        # Review of PR 2890: the walk is bounded and every failure builds.
        newer, foreign = 'c' * 40, 'd' * 40
        self.tip = newer
        self.parents[newer] = OLD
        self.parents[OLD] = None
        build, source, reason = self.push('linux', foreign)
        self.assertEqual((build, source), (True, foreign))
        self.assertIn('not within', reason)
        chain = ['%040x' % n for n in range(1, hourly.SUPERSEDE_DEPTH + 3)]
        self.tip = chain[-1]
        for child, parent in zip(chain[1:], chain):
            self.parents[child] = parent
        self.parents[chain[0]] = None
        self.calls.clear()
        build, _, _ = self.push('linux', chain[0])
        self.assertTrue(build, 'a source deeper than the bound builds')
        self.assertLessEqual(len([p for p, _, _ in self.calls if p.startswith('/git/commits/')]),
                             hourly.SUPERSEDE_DEPTH)
        def history_fails(path, method='GET', body=None, timeout=None):
            if path == '/branches/main':
                return {'commit': {'id': newer}}
            raise OSError('history offline')
        with patch.object(hourly, 'api', side_effect=history_fails):
            build, _, reason = self.push('linux', SOURCE)
            self.assertTrue(build)
            self.assertIn('main history unreadable', reason)

    def test_slow_successful_history_falls_back_to_build_within_the_budget(self):
        # Review of #2890: 150 commits behind at 2 s per answer would take 302 s,
        # beyond the 3-minute select job. The wall-clock budget builds instead.
        chain = ['%040x' % n for n in range(1, 152)]
        for child, parent in zip(chain[1:], chain):
            self.parents[child] = parent
        self.parents[chain[0]] = None
        self.tip = chain[-1]
        clock = [1000.0]
        timeouts = []
        original = hourly.api.side_effect
        def slow(path, method='GET', body=None, timeout=None):
            timeouts.append(timeout)
            clock[0] += 2
            return original(path, method, body, timeout)
        self.calls.clear()
        with patch.object(hourly, 'api', side_effect=slow), \
                patch.object(hourly.time, 'monotonic', side_effect=lambda: clock[0]):
            build, source, reason = self.push('linux', chain[0])
        self.assertEqual((build, source), (True, chain[0]))
        self.assertIn('TimeoutError', reason)
        self.assertLessEqual(clock[0] - 1000.0, hourly.SUPERSEDE_BUDGET_SECONDS + 2)
        self.assertTrue(all(0 < t <= hourly.SUPERSEDE_REQUEST_SECONDS for t in timeouts), timeouts)
        # Control: the same chain answered quickly is within the bound and skips.
        self.calls.clear()
        self.assertEqual(self.push('linux', chain[-40])[0], False)

    def test_starting_push_run_cancels_older_queued_pushes_and_keeps_the_newest(self):
        # Forgejo 17 queues cancel-in-progress: false groups FIFO and never replaces
        # a pending run; a burst of merges must not pile up select jobs.
        def run(run_id, status='waiting', event='push', workflow='linux-release.yml', ref='main'):
            return dict(id=run_id, status=status, trigger_event=event, workflow_id=workflow, prettyref=ref)
        self.runs[:] = [run(99, status='running'), run(100), run(101), run(102), run(103),
                        run(104, event='workflow_dispatch'), run(105, workflow='windows-diagnostic.yml'),
                        run(106, ref='feature')]
        self.push('linux')
        self.assertEqual(self.writes(), [('/actions/runs/101/cancel', None), ('/actions/runs/102/cancel', None)])
        query = next(p for p, m, _ in self.calls if p.startswith('/actions/runs?'))
        self.assertIn('workflow_id=linux-release.yml', query)
        self.assertIn('event=push', query)
        self.assertIn('status=waiting', query)
        # One queued run (the newest) is never cancelled.
        self.calls.clear()
        self.runs[:] = [run(99, status='running'), run(103)]
        self.push('linux')
        self.assertFalse(self.writes())
        # A failed inventory or cancel never changes the run's own decision.
        self.runs[:] = [run(101), run(102)]
        original = hourly.api.side_effect
        def cancel_fails(path, method='GET', body=None, timeout=None):
            if path.endswith('/cancel'):
                raise OSError('forbidden')
            return original(path, method, body, timeout)
        with patch.object(hourly, 'api', side_effect=cancel_fails):
            self.assertEqual(self.push('linux')[:2], (True, SOURCE))

    def test_push_of_the_current_beta_head_builds_nothing(self):
        for platform in hourly.WORKFLOWS:
            feed = hourly.feed_platform(platform)
            self.descriptor.side_effect = lambda source, published, feed=feed: dict(source=SOURCE, platform=feed)
            self.assertEqual(self.push(platform)[:2], (False, SOURCE))
            self.assertEqual(self.descriptor.call_args.args, (SOURCE, feed))
            # An older beta head never blocks the pushed commit.
            self.descriptor.side_effect = lambda source, published, feed=feed: dict(source=OLD, platform=feed)
            self.assertEqual(self.push(platform)[:2], (True, SOURCE))
        self.assertEqual(hourly.feed_platform('linux'), 'linux-appimage')
        self.assertFalse(self.status_reads())
        self.assertFalse(self.writes())

    def test_push_admission_fails_closed_off_main_or_with_a_foreign_descriptor(self):
        for change in [dict(GITHUB_REF='refs/heads/feature'), dict(GITHUB_REF='')]:
            with self.assertRaises(ValueError):
                self.push('linux', **change)
        for source in ['main', SOURCE[:7], '']:
            with self.assertRaises(ValueError):
                hourly.push_select(source, 'windows', self.env, self.descriptor)
        self.descriptor.return_value = dict(source=SOURCE, platform='macos')
        with self.assertRaises(ValueError):
            self.push('linux')

    def test_manual_dispatch_builds_main_tip_without_reading_certification(self):
        for platform in hourly.WORKFLOWS:
            self.assertEqual(self.select(platform)[:2], (True, SOURCE))
        self.assertFalse(self.writes())
        self.assertFalse(self.status_reads())

    def test_older_pin_is_superseded_by_newest_main_on_every_platform(self):
        # #2589 (04.11): an old certified source shipped hours after main moved.
        newer = 'c' * 40
        self.tip = newer
        self.parents[newer] = SOURCE
        for platform in hourly.WORKFLOWS:
            build, _, reason = self.select(platform, HOURLY_SOURCE=SOURCE)
            self.assertFalse(build)
            self.assertIn('superseded', reason)
            self.assertEqual(self.select(platform, HOURLY_SOURCE=newer)[:2], (True, newer))
        self.assertFalse(self.writes())
        self.assertFalse(self.status_reads())

    def test_newest_main_ahead_of_each_feed_wins_and_already_published_never_repeats(self):
        newer = 'c' * 40
        self.tip = newer
        self.parents[newer] = SOURCE
        self.descriptor.return_value = dict(source=OLD, platform='macos')
        self.assertEqual(self.select()[:2], (True, newer))
        self.assertFalse(self.select(HOURLY_SOURCE=SOURCE)[0])
        self.descriptor.return_value = dict(source=newer, platform='macos')
        self.assertFalse(self.select()[0])
        self.assertFalse(self.select(HOURLY_SOURCE=SOURCE)[0])
        self.assertFalse(self.writes())

    def test_source_and_build_cannot_go_backwards_when_a_newer_beta_exists(self):
        self.descriptor.return_value = dict(source=SOURCE, platform='macos')
        self.assertFalse(self.select()[0])
        self.assertFalse(self.select(HOURLY_SOURCE=OLD)[0])
        # A feed that is not on first-parent main fails closed.
        self.descriptor.return_value = dict(source='f' * 40, platform='macos')
        with self.assertRaises(ValueError):
            self.select()

    def test_no_beta_path_requests_certification(self):
        self.statuses.clear()
        for platform in hourly.WORKFLOWS:
            self.push(platform)
            self.select(platform)
            self.select(platform, source=OLD)
        self.assertFalse(self.writes('/actions/workflows/fast.yml/dispatches'))
        self.assertFalse(self.status_reads())
        self.assertFalse(hasattr(hourly, 'certify'))
        self.assertFalse(hasattr(hourly, 'green'))

    def test_github_main_and_pinned_refs_admit_without_certification(self):
        newer = 'c' * 40
        self.tip = newer
        self.parents[newer] = SOURCE
        env = dict(self.env, GITHUB_SHA=SOURCE)
        with patch.object(hourly, 'github_produced', return_value=False):
            # A main-ref run builds only main's unpublished tip.
            self.assertFalse(hourly.github_main('linux', env, self.descriptor)[0])
            self.assertTrue(hourly.github_main('linux', dict(env, GITHUB_SHA=newer), self.descriptor)[0])
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
            # An unreachable Forgejo never authorizes, and never crashes, a main build.
            with patch.object(hourly, 'api', side_effect=urllib.error.URLError('timed out')):
                self.assertFalse(hourly.github_main('linux', dict(env, GITHUB_SHA=newer), self.descriptor)[0])
        self.assertTrue(hourly.github_main('linux', dict(env, GITHUB_REF='refs/heads/forgejo-linux-pr/1'),
                                          self.descriptor)[0])
        self.assertFalse(self.status_reads())

    def test_stale_manual_snapshot_dispatches_current_tooling_without_compiling_wrong_source(self):
        for platform, workflow in hourly.WORKFLOWS.items():
            self.assertEqual(self.select(platform, source=OLD)[:2], (False, OLD))
            self.assertEqual(self.writes()[-1], ('/actions/workflows/' + workflow + '/dispatches',
                dict(ref='main', inputs={'hourly_source': SOURCE})))

    def run_coalesce(self, platform, **env):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'outputs'
            env = {**self.env, 'GITHUB_OUTPUT': str(output), 'OKILUM_SOURCE': '', 'OKILUM_RESIGN_BUILD': '',
                   'MACOS_PREVIEW_PR': '', 'HOURLY_SOURCE': '', **env}
            with patch.dict(os.environ, env), patch.object(sys, 'argv', ['coalesce.py', platform]), \
                    patch.object(hourly, 'published_beta', self.descriptor):
                runpy.run_path(str(ROOT / 'scripts/releases/coalesce.py'), run_name='__main__')
            return dict(line.split('=', 1) for line in output.read_text().splitlines())

    def test_coalesce_push_entrypoint_emits_the_pushed_commit_for_each_platform(self):
        newer = 'c' * 40
        self.parents[newer] = SOURCE
        for platform in hourly.WORKFLOWS:
            with self.subTest(platform=platform):
                self.tip = SOURCE
                values = self.run_coalesce(platform, GITHUB_SHA=SOURCE, GITHUB_EVENT_NAME='push')
                self.assertEqual((values['source'], values['build']), (SOURCE, 'true'))
                # Main moved on before this queued push run started: skip.
                self.tip = newer
                values = self.run_coalesce(platform, GITHUB_SHA=SOURCE, GITHUB_EVENT_NAME='push')
                self.assertEqual((values['source'], values['build']), (SOURCE, 'false'))
        # A push only reads main's tip and history: no statuses, no dispatch.
        self.assertFalse(self.status_reads())
        self.assertFalse(self.writes())

    def test_coalesce_dispatch_entrypoint_builds_only_the_newest_main_pin(self):
        newer = 'c' * 40
        self.tip = newer
        self.parents[newer] = SOURCE
        for platform in hourly.WORKFLOWS:
            for pinned, build in [(newer, 'true'), (SOURCE, 'false')]:
                with self.subTest(platform=platform, pinned=pinned):
                    values = self.run_coalesce(platform, GITHUB_SHA=newer, GITHUB_EVENT_NAME='workflow_dispatch',
                                               HOURLY_SOURCE=pinned)
                    self.assertEqual((values['source'], values['build']), (newer, build))

    def test_bad_history_or_network_fails_closed(self):
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


class PublicationGuards(unittest.TestCase):
    def test_pinned_older_product_reaches_beta_without_certification_and_never_regresses(self):
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
            # (prior beta source, ancestor relation of prior to SOURCE, publishes)
            for prior, relation, publishes in [(OLD, 0, True), (newer, 1, False), (SOURCE, 0, False)]:
                client = Mock(); client.call.side_effect = api
                with self.subTest(platform=platform, prior=prior), \
                        patch.object(hourly, 'main_source'), \
                        patch.object(hourly, 'api', side_effect=AssertionError('no certification read')), \
                        patch.object(publication, 'R2') as store, \
                        patch.object(publication.release_notes, 'predecessor', return_value=dict(source=prior, build=5041)), \
                        patch.object(publication.release_notes, 'prepare', return_value=dict(previous_source=prior, previous_build=5041)), \
                        patch.object(publication.release_notes, 'record'), patch.object(publication.prepare, 'complete'), \
                        patch.object(publication.subprocess, 'run', side_effect=lambda command, **kw: SimpleNamespace(
                            returncode=relation if command == ['git', 'merge-base', '--is-ancestor', prior, SOURCE] else 0)) as launch:
                    store.return_value.call.return_value = None
                    publication.publish(client, platform, 12)
                    commands = [c for c in launch.call_args_list if c.args[0][0] == publication.sys.executable]
                    self.assertEqual(bool(commands), publishes)
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

    def test_stable_0900_shipping_contexts_land_on_each_beta_source_for_dispatch(self):
        # Push runs get native `<platform>-release / <job> (push)` statuses on
        # their own commit; explicit reporting is only for dispatches.
        for platform, jobs in hourly.SHIPPING_JOBS.items():
            for event in ['workflow_dispatch']:
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
                for broken in [dict(GITHUB_EVENT_NAME='pull_request'), dict(GITHUB_EVENT_NAME='push'),
                               dict(GITHUB_EVENT_NAME='schedule'),
                               dict(GITHUB_REF='refs/heads/feature'), dict(JOB_RESULTS='{}')]:
                    self.api.reset_mock()
                    with self.assertRaises(ValueError):
                        hourly.report_shipping(platform, {**env, **broken})
                    self.api.assert_not_called()

    def test_certification_only_reports_for_stable_and_never_starts_beta_work(self):
        # Every main push already builds its beta; a certification-triggered
        # dispatch would duplicate it or resurrect a superseded source.
        env = dict(self.env, GITHUB_SHA=OLD, CERTIFICATION_SOURCE=SOURCE)
        for results in [{'check': {'result': 'success'}}, {'check': {'result': 'failure'}},
                        {'shell-full': {'result': 'success'}}, {'shell-full': {'result': 'failure'}}]:
            self.api.reset_mock()
            hourly.report_certification(dict(env, JOB_RESULTS=json.dumps(results)))
            self.assertFalse(any(c.args[0].endswith('/dispatches') for c in self.api.call_args_list))
            self.assertTrue(all(c.args[0] == '/statuses/' + SOURCE for c in self.api.call_args_list))

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
                patch.object(hourly, 'main_source'), \
                patch.object(hourly, 'api', side_effect=AssertionError('no certification read')), \
                patch.object(hourly, 'wait_for_mirror') as mirror, \
                patch.object(hourly, 'github_produced', return_value=False) as produced:
            client.request.return_value = []
            hourly.dispatch_linux(SOURCE)
            self.assertEqual(client.request.call_args.args, ('git/refs', 'POST',
                {'ref': 'refs/heads/' + branch, 'sha': SOURCE}))
            # The ref is created only after GitHub has the pushed commit.
            mirror.assert_called_once_with(client, SOURCE)
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

    def test_mirror_wait_polls_until_the_pushed_commit_exists_and_is_bounded(self):
        import appimage_artifact
        transport = appimage_artifact.transport
        clock = [0.0]
        sleeps = []
        def sleep(seconds):
            sleeps.append(seconds)
            clock[0] += seconds
        client = Mock()
        client.request.side_effect = [transport.Unavailable('GitHub GET git/commits: 404'),
                                      transport.Unavailable('GitHub GET git/commits: 404'), {'sha': SOURCE}]
        hourly.wait_for_mirror(client, SOURCE, wait=600, clock=lambda: clock[0], sleep=sleep)
        self.assertEqual(sleeps, [15, 15])
        self.assertEqual(client.request.call_args.args, ('git/commits/' + SOURCE,))
        # Never reached in time: no ref, no producer.
        client.request.side_effect = transport.Unavailable('GitHub GET git/commits: 404')
        clock[0] = 0.0
        with self.assertRaisesRegex(ValueError, 'mirror'):
            hourly.wait_for_mirror(client, SOURCE, wait=60, clock=lambda: clock[0], sleep=sleep)
        # Quota exhaustion and identity mismatch fail at once.
        for outcome in [transport.RateLimitExhausted('quota'), {'sha': OLD}]:
            client.request.side_effect = [outcome]
            sleeps.clear()
            with self.assertRaises((transport.RateLimitExhausted, ValueError)):
                hourly.wait_for_mirror(client, SOURCE, wait=600, clock=lambda: clock[0], sleep=sleep)
            self.assertEqual(sleeps, [])

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
        # #2589: check and shell-full wait on a bridge while GitHub's dedicated
        # runners execute the exact source's commands, for every event.
        for name in ['check', 'shell-full']:
            job = data['jobs'][name]
            self.assertEqual(job['runs-on'], 'bridge')
            self.assertNotIn('if', job)
            self.assertNotIn('fast-local.sh', str(job))
        self.assertEqual(data['jobs']['shell-full']['needs'], ['check'])
        # Main, nightly and manual certification share one hosted transport.
        for name, workflow, step_name in [
                ('check', 'forgejo-check.yml', 'Run the fast check on GitHub'),
                ('shell-full', 'forgejo-shell-full.yml', 'Run the complete Linux shell binary suite on GitHub')]:
            step = data['jobs'][name]['steps'][-1]
            self.assertEqual(step['name'], step_name)
            self.assertNotIn('if', step)
            self.assertEqual(step['env']['CI_LANE'], name)
            self.assertEqual(step['env']['PR_HEAD_SHA'], '${{ inputs.certification_source || github.sha }}')
            self.assertIn(f'git cat-file -e "HEAD:.github/workflows/{workflow}"', step['run'])
            self.assertIn('python3 "$OKILUM_HOSTED_BRIDGE/github-pr.py"', step['run'])
            hosted = self.load('.github/workflows/' + workflow)
            self.assertEqual(hosted['on'], {'push': {'branches': [f'forgejo-{name}-pr/**']}})
        hosted = self.load('.github/workflows/forgejo-shell-full.yml')
        self.assertIn('bash scripts/ci/test-group.sh shell-full.sh 1920 bash scripts/ci/shell-full.sh',
                      hosted['jobs']['shell-full']['steps'][-2]['run'])
        # The former check job's commands, group limits and step ceilings, unchanged.
        hosted = self.load('.github/workflows/forgejo-check.yml')
        self.assertEqual(set(hosted['jobs']), {'python', 'check'})
        gate = hosted['jobs']['check']['steps'][-2]
        self.assertEqual(gate['name'], 'Build Linux desktop and run fast unit tests')
        self.assertEqual(gate['run'], 'bash scripts/ci/test-group.sh fast.sh 2040 bash scripts/ci/fast.sh')
        self.assertNotIn('if', gate)
        for job in hosted['jobs'].values():
            self.assertEqual(job['runs-on'], 'ubuntu-24.04')
            self.assertNotIn('if', job)
            self.assertNotIn('secrets.', str(job))
        self.assertEqual(hosted['jobs']['check']['env']['CARGO_PROFILE_DEV_DEBUG'], 'line-tables-only')
        for directory in ['ci', 'releases', 'updater']:
            limit = 180 if directory == 'ci' else 90
            command = (f'bash scripts/ci/test-group.sh {directory}-python {limit} '
                       f"python3 -I -m unittest discover -s scripts/{directory} -p 'test_*.py' -v")
            step = next(s for s in hosted['jobs']['python']['steps'] if s.get('run') == command)
            self.assertNotIn('if', step)
            self.assertEqual(step['timeout-minutes'], '5')
        for name in ['check', 'shell-full']:
            steps = data['jobs'][name]['steps']
            validation = next(s for s in steps if 'certification-source ' in s.get('run', ''))
            self.assertIn('bash "$OKILUM_TEST_GROUP" certification-source 90 '
                          'python3 scripts/releases/hourly_beta.py certify-source', validation['run'])
            self.assertEqual(validation['timeout-minutes'], '2')
            self.assertEqual(validation['env']['CERTIFICATION_SOURCE'], '${{ inputs.certification_source }}')
            # Validation precedes the hosted push of that source.
            self.assertLess(steps.index(validation), len(steps) - 1)
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

    def test_beta_producers_run_on_main_push_and_a_running_beta_finishes(self):
        # Oleg 05.11: beta on every main push, no cron, no certification.
        # Manager 13.11: a newer push never cancels a running beta.
        for platform, workflow in hourly.WORKFLOWS.items():
            data = self.load('.forgejo/workflows/' + workflow)
            self.assertEqual(data['on']['push'], {'branches': ['main']})
            self.assertEqual(set(data['on']), {'push', 'workflow_dispatch'})
            prefix = {'linux': 'linux', 'macos': 'macos', 'windows': 'windows'}[platform]
            # Linux and Windows push runs start at once in their own group (no queue
            # behind the previous merge); Mac still queues FIFO and coalesces.
            group = ("${{ github.event_name == 'push' && github.sha || inputs.hourly_source || github.run_id }}" if platform in ('linux', 'windows')
                     else "${{ github.event_name == 'push' && 'main' || inputs.hourly_source || github.run_id }}")
            self.assertEqual(data['concurrency']['group'], prefix + '-beta-' + group)
            self.assertEqual(data['concurrency']['cancel-in-progress'], 'false')
            report = data['jobs']['shipping-status']
            self.assertIn("github.event_name == 'workflow_dispatch'", report['if'])
            self.assertNotIn('schedule', report['if'])
        linux = self.load('.forgejo/workflows/linux-release.yml')
        request = next(s for s in linux['jobs']['appimage-reuse']['steps']
                       if s.get('run') == 'python3 scripts/releases/hourly_beta.py dispatch-linux')
        self.assertIn("github.event_name == 'push'", request['if'])
        self.assertIn("inputs.source == ''", request['if'])
        # The GitHub rolling Beta follows each publication; no polling schedule.
        self.assertEqual(set(self.load('.forgejo/workflows/releases.yml')['on']), {'workflow_dispatch'})
        publish = (ROOT / '.forgejo/workflows/release-publish.yml').read_text()
        self.assertIn("'/actions/workflows/releases.yml/dispatches'", publish)
        # Windows push betas stay unsigned; manual signing is unchanged.
        windows = (ROOT / '.forgejo/workflows/windows-diagnostic.yml').read_text()
        self.assertIn("BACKEND: ${{ github.event_name == 'workflow_dispatch' && inputs.hourly_source == '' "
                      "&& vars.OKILUM_WINDOWS_SIGN_BACKEND || '' }}", windows)

    def test_public_proxies_keep_pr_refs_and_beta_producer_groups(self):
        for name, branch in [('forgejo-linux', 'forgejo-linux-pr/**'), ('forgejo-macos', 'forgejo-pr/**')]:
            path = '.github/workflows/' + name + '.yml'
            data = self.load(path)
            self.assertEqual(data['on']['push']['branches'], [branch])
            self.assertNotIn({'cron': '17 * * * *'}, data['on'].get('schedule', []))
            self.assertIn('workflow_dispatch', data['on'])
            self.assertEqual(data['concurrency']['cancel-in-progress'], 'true')
            self.assertIn('hourly_beta.py', (ROOT / path).read_text())
        linux = self.load('.github/workflows/forgejo-linux.yml')
        self.assertEqual(linux['on']['schedule'], [{'cron': '5 22 * * 2,6'}])  # nightly acceptance only
        # Per-ref: every write-once beta ref finishes even when a newer one starts.
        self.assertEqual(linux['concurrency']['group'], 'forgejo-linux-${{ github.ref }}')
        self.assertNotIn('schedule', self.load('.github/workflows/forgejo-macos.yml')['on'])
        shipping = self.load('.github/workflows/forgejo-macos-shipping.yml')
        self.assertEqual(set(shipping['on']), {'workflow_dispatch'})
        # Beta signing runs share one group: a newer push supersedes an older one.
        # Verify-only PR artifacts keep per-source groups and always finish.
        self.assertEqual(shipping['concurrency']['group'],
                         "macos-shipping-${{ inputs.verify_only == 'true' && inputs.source || 'beta' }}")
        self.assertEqual(shipping['concurrency']['cancel-in-progress'], "${{ inputs.verify_only != 'true' }}")
        # The keychain cleanup still runs when a superseded signing run is cancelled.
        cleanup = [s for s in shipping['jobs']['shipping']['steps'] if 'okilum-keychain-cleanup.py' in s.get('run', '')
                   and 'cleanup' in s.get('run', '').split('okilum-keychain-cleanup.py', 1)[1]]
        self.assertTrue(cleanup)
        self.assertTrue(all(s.get('if') == 'always()' for s in cleanup))
        mac = self.load('.forgejo/workflows/macos-release.yml')
        self.assertTrue(mac['jobs']['release']['if'].startswith('false'))
        hosted = mac['jobs']['hosted-artifact']
        self.assertIn("github.event_name == 'push'", hosted['if'])
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
