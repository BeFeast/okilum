"""PR preview selection uses the current trusted green head, before the M4 expiry."""
import copy
import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('preview', Path(__file__).with_name('coalesce.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
SHA, NEW = 'a' * 40, 'b' * 40
REPO, REF = 'BeFeast/okilum', 'feat/preview'


class PreviewTests(unittest.TestCase):
    def setUp(self):
        self.pr = dict(state='open', merged=False, draft=False,
            head=dict(sha=SHA, ref=REF, repo=dict(full_name=REPO)),
            base=dict(ref='main', repo=dict(full_name=REPO)))
        self.env = dict(GITHUB_REPOSITORY=REPO, GITHUB_REF='refs/heads/' + REF,
            GITHUB_RUN_ID='100', MACOS_PR_PREVIEWS_ENABLED='true', MACOS_PREVIEW_PR='7', MACOS_PREVIEW_HEAD=SHA)
        self.now = m.datetime(2026, 10, 22, tzinfo=m.timezone.utc)
        self.status = dict(sha=SHA, statuses=[dict(context='ci / macos (pull_request)', status='success')])
        self.runs = dict(total_count=0, workflow_runs=[])
        self.calls = []
        self.api_patch = patch.object(m, 'macos_api', side_effect=self.api)
        self.api_mock = self.api_patch.start()
        self.addCleanup(self.api_patch.stop)
        self.desc_patch = patch.object(m, 'descriptor', return_value=None)
        self.desc = self.desc_patch.start()
        self.addCleanup(self.desc_patch.stop)

    def api(self, method, path, body=None):
        self.calls.append((method, path, body))
        if path == '/pulls/7':
            return self.pr
        if '/commits/' in path:
            return self.status
        if '/actions/runs?' in path:
            return self.runs
        if method == 'POST':
            return None
        raise AssertionError(path)

    def select(self, event='pull_request', source=SHA):
        return m.select_macos(event, source, self.env, self.now)

    def attempt(self, rid=99, sha=SHA, requested=SHA):
        return dict(id=rid, workflow_id='macos-release.yml', commit_sha=sha,
            prettyref=REF, is_fork_pull_request=False, trigger_event='workflow_dispatch',
            status='success', event_payload=json.dumps({'inputs': {'preview_pr': '7', 'preview_head': requested}}))

    def test_disabled_by_default_blocks_dispatch_and_signer_before_api_access(self):
        for enabled in (None, '', 'false', 'TRUE'):
            if enabled is None:
                self.env.pop('MACOS_PR_PREVIEWS_ENABLED', None)
            else:
                self.env['MACOS_PR_PREVIEWS_ENABLED'] = enabled
            for event in ('pull_request', 'workflow_dispatch', 'schedule'):
                self.assertFalse(self.select(event)[0])
        self.api_mock.assert_not_called()
        self.desc.assert_not_called()

    def test_coordinator_dispatches_exact_requested_head_without_a_signer(self):
        self.assertFalse(self.select()[0])
        self.assertEqual(self.calls[-1], ('POST', '/actions/workflows/macos-release.yml/dispatches',
            {'ref': REF, 'inputs': {'preview_pr': '7', 'preview_head': SHA}}))
        self.desc.assert_called_once_with(SHA, 'macos')

    def test_serialized_dispatch_builds_its_exact_head_on_local_m4(self):
        self.assertEqual(self.select('workflow_dispatch')[:3], (True, SHA, True))
        self.assertNotIn('POST', [c[0] for c in self.calls])

    def test_draft_fork_deleted_repo_wrong_base_and_closed_prs_skip(self):
        good = copy.deepcopy(self.pr)
        for change in [dict(draft=True), dict(state='closed'), dict(merged=True),
                       dict(head=dict(good['head'], repo=dict(full_name='fork/repo'))),
                       dict(head=dict(good['head'], repo=None)),
                       dict(base=dict(good['base'], ref='feature')),
                       dict(base=dict(good['base'], repo=dict(full_name='fork/repo')))]:
            self.pr = dict(good, **change)
            self.calls.clear()
            self.assertFalse(self.select()[0])
            self.assertEqual(len(self.calls), 1)
        self.desc.assert_not_called()

    def test_newest_head_wins_at_coordinator_and_serialized_build(self):
        self.pr['head']['sha'] = NEW
        for event in ('pull_request', 'workflow_dispatch'):
            self.assertFalse(self.select(event)[0])
        self.pr['head']['sha'] = SHA
        self.assertFalse(self.select('workflow_dispatch', NEW)[0])
        self.env['GITHUB_REF'] = 'refs/heads/wrong'
        self.assertFalse(self.select('workflow_dispatch')[0])
        self.desc.assert_not_called()

    def test_only_exact_head_unique_successful_macos_context_qualifies(self):
        good = copy.deepcopy(self.status)
        for status in ('pending', 'failure', 'error', 'skipped', ''):
            self.status = dict(good, statuses=[dict(good['statuses'][0], status=status)])
            self.assertFalse(self.select()[0])
        for status in [dict(good, sha=NEW), dict(good, statuses=[]),
                       dict(good, statuses=good['statuses'] * 2),
                       dict(good, statuses=[dict(context='ci / linux (pull_request)', status='success')])]:
            self.status = status
            self.assertFalse(self.select()[0])
        self.desc.assert_not_called()

    def test_linux_failure_does_not_delay_green_macos_preview(self):
        self.status['statuses'].append(dict(context='ci / linux (pull_request)', status='failure'))
        self.select()
        self.assertEqual(self.calls[-1][0], 'POST')

    def test_published_head_and_prior_attempt_do_not_repeat_notarization(self):
        self.desc.return_value = dict(source=SHA, platform='macos')
        self.assertFalse(self.select()[0])
        self.assertEqual(len(self.calls), 2)
        self.desc.return_value = None
        self.runs = dict(total_count=1, workflow_runs=[self.attempt()])
        self.assertFalse(self.select()[0])
        self.assertIn('backoff', self.select()[3])

    def test_oldest_queued_run_owns_the_head_and_race_skip_does_not_poison_new_head(self):
        self.runs = dict(total_count=2, workflow_runs=[self.attempt(100), self.attempt(101)])
        self.assertTrue(self.select('workflow_dispatch')[0])
        self.env['GITHUB_RUN_ID'] = '101'
        self.assertFalse(self.select('workflow_dispatch')[0])
        # A previous dispatch captured SHA after the branch moved from NEW.
        # It refused to sign; SHA remains eligible for its own green request.
        self.runs = dict(total_count=1, workflow_runs=[self.attempt(requested=NEW)])
        self.assertTrue(self.select('workflow_dispatch')[0])

    def test_expiry_is_noop_even_for_already_queued_dispatch_without_network(self):
        self.now = m.MACOS_BETA_UNTIL
        for event in ('pull_request', 'workflow_dispatch'):
            self.assertFalse(self.select(event)[0])
        self.api_mock.assert_not_called()
        self.desc.assert_not_called()

    def test_identity_and_network_failures_fail_closed(self):
        self.env['MACOS_PREVIEW_PR'] = 'not-a-number'
        with self.assertRaises(ValueError):
            self.select()
        self.env['MACOS_PREVIEW_PR'] = '7'
        self.api_mock.side_effect = TimeoutError('offline')
        with self.assertRaises(TimeoutError):
            self.select()
        self.desc.assert_not_called()

    def test_workflow_trigger_is_independent_and_signing_is_not_cancelled(self):
        ci = (ROOT / '.forgejo/workflows/ci.yml').read_text()
        block = ci.split('  macos-preview:\n', 1)[1].split('  macos:\n', 1)[0]
        self.assertIn('needs: [native-scope, macos]', block)
        self.assertIn('if: false', block)
        self.assertIn('MACOS_PR_PREVIEWS_ENABLED: ${{ vars.OKILUM_MACOS_PR_PREVIEWS }}', block)
        self.assertNotIn('needs.linux', block)
        self.assertIn('ref: main', block)
        self.assertIn('actions: write', block)
        workflow = (ROOT / '.forgejo/workflows/macos-release.yml').read_text()
        self.assertIn('cancel-in-progress: false', workflow)
        self.assertIn('MACOS_PR_PREVIEWS_ENABLED: ${{ vars.OKILUM_MACOS_PR_PREVIEWS }}', workflow)
        self.assertIn('MACOS_PREVIEW_HEAD: ${{ inputs.preview_head }}', workflow)
        self.assertIn("needs.select.outputs.preview == 'true'", workflow)
