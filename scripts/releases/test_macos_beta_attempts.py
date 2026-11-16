"""A main SHA has one Mac producer, across push and delayed manual runs (#2891)."""
from datetime import datetime, timezone
import unittest
from unittest.mock import patch

import coalesce

SOURCE, NEW = 'a' * 40, 'b' * 40


class MainAttempts(unittest.TestCase):
    def setUp(self):
        self.env = dict(GITHUB_REF='refs/heads/main', GITHUB_RUN_ID='100')
        self.now = datetime(2026, 11, 17, tzinfo=timezone.utc)
        self.runs = []
        guard = patch.object(coalesce, 'macos_api', side_effect=self.api)
        self.api_mock = guard.start()
        self.addCleanup(guard.stop)
        # Exercise the common admission after either selector chooses this source.
        for name in ('push_select', 'select'):
            guard = patch.object(coalesce.hourly_beta, name,
                                 return_value=(True, SOURCE, 'Build main'))
            guard.start()
            self.addCleanup(guard.stop)

    def api(self, method, path, body=None):
        self.assertEqual(method, 'GET')
        self.assertIn('head_sha=' + SOURCE, path)
        self.assertIn('ref=refs%2Fheads%2Fmain', path)
        return dict(total_count=len(self.runs), workflow_runs=self.runs)

    def attempt(self, event, rid=99, status='running', inputs=None):
        return dict(id=rid, workflow_id='macos-release.yml', commit_sha=SOURCE,
                    prettyref='main', is_fork_pull_request=False,
                    trigger_event=event, status=status,
                    event_payload={'inputs': inputs or {}})

    def select(self, event):
        return coalesce.select_macos(event, SOURCE, self.env, self.now)

    def test_push_and_manual_share_the_oldest_attempt_even_before_publication(self):
        # Queued, running, signed awaiting publication, or any unknown state.
        for previous in ('push', 'workflow_dispatch'):
            for status in ('waiting', 'blocked', 'running', 'success', 'unknown'):
                self.runs = [self.attempt(previous, status=status)]
                for event in ('push', 'workflow_dispatch'):
                    with self.subTest(previous=previous, status=status, event=event):
                        build, source, local, reason = self.select(event)
                        self.assertEqual((build, source, local), (False, SOURCE, False))
                        self.assertIn('already attempted', reason)

    def test_finished_failed_or_cancelled_attempt_allows_manual_recovery(self):
        # A failed or cancelled run never handed a signed build to publication,
        # so the tip keeps an explicit manual path to a beta.
        for previous in ('push', 'workflow_dispatch'):
            for status in ('failure', 'cancelled'):
                with self.subTest(previous=previous, status=status):
                    self.runs = [self.attempt(previous, status=status)]
                    self.assertTrue(self.select('workflow_dispatch')[0])
        # A live attempt still owns the SHA next to a failed one.
        self.runs = [self.attempt('push', rid=98, status='failure'),
                     self.attempt('workflow_dispatch', rid=99, status='running')]
        self.assertFalse(self.select('workflow_dispatch')[0])

    def test_newer_duplicate_never_makes_the_oldest_run_back_off(self):
        for event in ('push', 'workflow_dispatch'):
            self.runs = [self.attempt('push', rid=100),
                         self.attempt('workflow_dispatch', rid=101)]
            self.assertTrue(self.select(event)[0])
            self.env['GITHUB_RUN_ID'] = '101'
            self.assertFalse(self.select(event)[0])
            self.env['GITHUB_RUN_ID'] = '100'

    def test_skipped_coordinators_do_not_own_the_captured_head(self):
        # Mutable main moved from the requested SHA before the dispatch started.
        # That pinned run skips, leaving this SHA eligible for its own push.
        for inputs in ({'hourly_source': NEW}, {'source': NEW}, {'preview_pr': '7'}):
            self.runs = [self.attempt('workflow_dispatch', inputs=inputs)]
            self.assertTrue(self.select('push')[0])
        for status in ('success', 'failure', 'running'):
            # A main schedule run only dispatched a pinned run; it never signed.
            self.runs = [self.attempt('schedule', status=status)]
            self.assertTrue(self.select('push')[0])
        self.runs = [self.attempt('workflow_dispatch', inputs={'hourly_source': SOURCE})]
        self.assertFalse(self.select('push')[0])

    def test_skipped_sources_do_not_read_attempts(self):
        for event, name in (('push', 'push_select'), ('workflow_dispatch', 'select')):
            with patch.object(coalesce.hourly_beta, name,
                              return_value=(False, SOURCE, 'Superseded')):
                self.assertFalse(self.select(event)[0])
        self.api_mock.assert_not_called()

    def test_incomplete_or_unreadable_inventory_cannot_admit_a_second_producer(self):
        self.api_mock.side_effect = None
        self.api_mock.return_value = dict(total_count=2, workflow_runs=[self.attempt('push')])
        with self.assertRaises(ValueError):
            self.select('push')
        self.api_mock.side_effect = TimeoutError('offline')
        with self.assertRaises(TimeoutError):
            self.select('workflow_dispatch')


if __name__ == '__main__':
    unittest.main()
