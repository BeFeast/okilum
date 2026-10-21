"""Exercise the standalone shell helper with offline curl/ssh positive controls."""
import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

HELPER = Path(__file__).with_name('okilum-sign-code.sh')
SHA = 'd' * 40
APPROVED = dict(id=123, index_in_repo=99, workflow_id='windows-diagnostic.yml',
                ref='refs/heads/main', commit_sha=SHA, owner='BeFeast', repo='okilum')
RUN = dict(id=123, index_in_repo=99, workflow_id='windows-diagnostic.yml',
           prettyref='main', commit_sha=SHA, trigger_event='schedule', status='running',
           is_fork_pull_request=False, is_ref_deleted=False,
           repository=dict(full_name='BeFeast/okilum', name='okilum', fork=False,
                           owner=dict(login='BeFeast')))


def page(*runs):
    return dict(total_count=len(runs), workflow_runs=list(runs))


class GuardTests(unittest.TestCase):
    def run_helper(self, responses, allowlist=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / '.config/forgejo').mkdir(parents=True)
            (root / '.cache').mkdir()
            (root / '.config/forgejo/token.env').write_text('FORGEJO_TOKEN=offline-test\n')
            (root / 'responses').write_text(json.dumps(responses))
            curl = root / 'curl'
            curl.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['HOME'])
with (root / 'calls').open('a') as log:
    log.write(('POST ' if '-X' in sys.argv else 'GET ') + sys.argv[-1] + '\\n')
responses = json.loads((root / 'responses').read_text())
if not responses:
    sys.exit(91)
response = responses.pop(0)
(root / 'responses').write_text(json.dumps(responses))
if response == 'timeout':
    sys.exit(28)
print(json.dumps(response))
''')
            curl.chmod(0o755)
            ssh = root / 'ssh'
            ssh.write_text('#!/bin/sh\necho UNEXPECTED-LOGIN >&2\nexit 92\n')
            ssh.chmod(0o755)
            env = dict(os.environ, HOME=str(root), XDG_RUNTIME_DIR=str(root),
                       PATH=str(root) + os.pathsep + os.environ['PATH'])
            env.pop('OKILUM_SIGN_CANCEL_ALLOWLIST', None)
            if allowlist is not None:
                env['OKILUM_SIGN_CANCEL_ALLOWLIST'] = json.dumps(allowlist)
            result = subprocess.run(['bash', str(HELPER), SHA, '14766'], env=env,
                                    input='invalid-otp\n', text=True, capture_output=True, timeout=5)
            calls = (root / 'calls').read_text() if (root / 'calls').exists() else ''
            self.assertNotIn('UNEXPECTED-LOGIN', result.stderr)
            return result, calls.splitlines()

    def assert_stopped(self, responses, allowlist=None, posts=0):
        result, calls = self.run_helper(responses, allowlist)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn('OTP was not read', result.stderr)
        self.assertEqual(sum(c.startswith('POST ') for c in calls), posts)
        self.assertFalse(any('variables' in c or 'dispatch' in c for c in calls))
        return calls

    def test_default_empty_allowlist_cancels_nothing(self):
        result, calls = self.run_helper([page(), page()])
        # Positive control: reaching OTP validation exits 2; guard failures exit 1.
        self.assertEqual(result.returncode, 2)
        self.assertIn('code must be 6 digits', result.stdout)
        self.assertTrue(all(c.startswith('GET ') for c in calls))

    def test_empty_allowlist_stops_on_active_run(self):
        self.assert_stopped([page(RUN)])

    def test_exact_match_revalidates_before_post_and_before_otp(self):
        result, calls = self.run_helper([page(RUN), page(RUN), RUN, {}, page()], [APPROVED])
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(len(calls), 5)
        self.assertIn('status=unknown&status=waiting&status=running&status=blocked&limit=50&page=1', calls[0])
        self.assertNotIn('status=queued', calls[0])
        self.assertTrue(calls[2].endswith('/actions/runs/123'))
        self.assertEqual(calls[3], 'POST https://git.oklabs.uk/api/v1/repos/BeFeast/okilum/actions/runs/123/cancel')
        self.assertIn('code must be 6 digits', result.stdout)

    def test_each_identity_field_must_match(self):
        mutations = dict(id=124, index_in_repo=100, workflow_id='other.yml', prettyref='other',
                         commit_sha='e' * 40, trigger_event='workflow_dispatch',
                         is_fork_pull_request=True, is_ref_deleted=True, status='unknown')
        for key, value in mutations.items():
            with self.subTest(key=key):
                changed = dict(RUN, **{key: value})
                self.assert_stopped([page(changed)], [APPROVED])

    def test_foreign_owner_repo_or_fork(self):
        for key, value in [('full_name', 'foreign/okilum'), ('name', 'other'),
                           ('owner', {'login': 'foreign'}), ('fork', True)]:
            with self.subTest(key=key):
                changed = copy.deepcopy(RUN)
                changed['repository'][key] = value
                self.assert_stopped([page(changed)], [APPROVED])

    def test_new_run_before_post_stops_without_cancelling(self):
        new = dict(RUN, id=124, index_in_repo=100)
        self.assert_stopped([page(RUN), page(RUN, new)], [APPROVED])

    def test_new_run_after_post_stops_before_otp(self):
        new = dict(RUN, id=124, index_in_repo=100)
        self.assert_stopped([page(RUN), page(RUN), RUN, {}, page(new)], [APPROVED], posts=1)

    def test_detail_mismatch_stops_before_post(self):
        self.assert_stopped([page(RUN), page(RUN), dict(RUN, commit_sha='e' * 40)], [APPROVED])

    def test_inventory_timeout_at_each_boundary(self):
        for responses, posts in [(['timeout'], 0), ([page(RUN), 'timeout'], 0),
                                  ([page(RUN), page(RUN), RUN, {}, 'timeout'], 1)]:
            with self.subTest(responses=responses):
                self.assert_stopped(responses, [APPROVED], posts)

    def test_incomplete_inventory(self):
        self.assert_stopped([dict(total_count=2, workflow_runs=[RUN])], [APPROVED])
        self.assert_stopped([dict(workflow_runs=[])])

    def test_missing_approved_target(self):
        self.assert_stopped([page()], [APPROVED])

    def test_cancel_failure_is_terminal(self):
        self.assert_stopped([page(RUN), page(RUN), RUN, 'timeout'], [APPROVED], posts=1)

    def test_pending_cancellation_stops_before_otp(self):
        self.assert_stopped([page(RUN), page(RUN), RUN, {}, page(RUN)], [APPROVED], posts=1)

    def test_invalid_or_unbounded_allowlist_never_calls_api(self):
        for entries in [{}, [dict(APPROVED, ref='main')], [APPROVED] * 11,
                        [APPROVED, APPROVED], [dict(APPROVED, id=True)]]:
            with self.subTest(entries=entries):
                self.assertEqual(self.assert_stopped([], entries), [])


if __name__ == '__main__':
    unittest.main()
