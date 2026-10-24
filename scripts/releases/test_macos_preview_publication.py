"""Protected publication authorizes only the verified Mac preview path."""
import copy
import io
import json
from pathlib import Path
import unittest
from unittest.mock import Mock, patch
import zipfile

import publication as p
import release_notes as notes

SHA, MAIN = 'a' * 40, 'b' * 40
REPO, REF = 'BeFeast/okilum', 'feat/preview'


class PreviewPublication(unittest.TestCase):
    def setUp(self):
        self.run = dict(id=12, index_in_repo=100, workflow_id='macos-release.yml',
            prettyref=REF, commit_sha=SHA, is_fork_pull_request=False,
            trigger_event='workflow_dispatch', status='success', created='2026-10-22T12:00:00Z',
            event_payload=json.dumps({'inputs': {'preview_pr': '7', 'preview_head': SHA}}))
        self.pr = dict(number=7, state='open', draft=False, merged=False,
            title='feat(reader): Faster navigation', body='User-visible: Jump to projects', labels=[],
            head=dict(sha=SHA, ref=REF, repo=dict(full_name=REPO)),
            base=dict(ref='main', repo=dict(full_name=REPO)))
        self.status = dict(sha=SHA, statuses=[dict(context='ci / macos (pull_request)', status='success')])
        self.client = Mock()
        self.client.call.side_effect = self.call
        self.pr_reads = 0
        self.change_after_download = False
        self.store = Mock()
        self.store.call.return_value = None

    def call(self, method, path, *args, **kwargs):
        self.assertEqual(method, 'GET')
        if path == '/actions/runs/12':
            return self.run
        if path == '/branches/main':
            return {'commit': {'id': MAIN}}
        if path == '/pulls/7':
            self.pr_reads += 1
            pr = copy.deepcopy(self.pr)
            if self.change_after_download and self.pr_reads > 1:
                pr['head']['sha'] = MAIN
            return pr
        if path == '/commits/' + SHA + '/status':
            return self.status
        if path == '/actions/runs/12/artifacts':
            return [dict(name='macos-publication', expired=False, run_id=12, id=3)]
        if path == '/actions/artifacts/3/zip':
            stream = io.BytesIO()
            with zipfile.ZipFile(stream, 'w') as archive:
                archive.writestr('release.json', '{}')
            return stream.getvalue()
        raise AssertionError(path)

    def test_validation_keeps_default_main_boundary_closed(self):
        with self.assertRaises(ValueError):
            p.eligible(self.run, 'macos', MAIN)
        pr = p.validate_preview(self.client, 'macos', self.run)
        self.assertEqual(pr['number'], 7)
        self.assertTrue(p.eligible(self.run, 'macos', MAIN, pr))
        for platform in ('windows', 'linux'):
            with self.assertRaises(ValueError):
                p.validate_preview(self.client, platform, self.run)
            with self.assertRaises(ValueError):
                p.eligible(self.run, platform, MAIN, pr)

    def test_draft_fork_wrong_head_or_branch_and_closed_pr_refuse(self):
        original = copy.deepcopy(self.pr)
        for change in [dict(draft=True), dict(state='closed'), dict(merged=True),
                       dict(head=dict(original['head'], sha=MAIN)),
                       dict(head=dict(original['head'], ref='other')),
                       dict(head=dict(original['head'], repo=dict(full_name='fork/repo'))),
                       dict(head=dict(original['head'], repo=None)),
                       dict(base=dict(original['base'], ref='other'))]:
            self.pr = dict(original, **change)
            with self.assertRaises(ValueError):
                p.validate_preview(self.client, 'macos', self.run)

    def test_run_provenance_and_expiry_refuse_before_reading_pr(self):
        for change in [dict(workflow_id='ci.yml'), dict(prettyref='main'),
                       dict(commit_sha=MAIN), dict(trigger_event='pull_request'),
                       dict(is_fork_pull_request=True), dict(created='2026-10-26T03:00:00Z'),
                       dict(event_payload={'inputs': {'preview_pr': '7', 'preview_head': MAIN}}),
                       dict(event_payload={'inputs': {'preview_pr': '7', 'preview_head': SHA, 'source': MAIN}})]:
            self.client.call.reset_mock()
            with self.assertRaises(ValueError):
                p.validate_preview(self.client, 'macos', dict(self.run, **change))
            self.client.call.assert_not_called()

    def test_missing_or_failed_exact_head_macos_refuses(self):
        for status in [dict(sha=MAIN, statuses=self.status['statuses']),
                       dict(sha=SHA, statuses=[]),
                       dict(sha=SHA, statuses=[dict(context='ci / macos (pull_request)', status='failure')])]:
            self.status = status
            with self.assertRaises(ValueError):
                p.validate_preview(self.client, 'macos', self.run)

    def test_draft_refuses_before_store_or_artifact_access(self):
        self.pr['draft'] = True
        with patch.object(p, 'R2') as store, patch.object(p.subprocess, 'run') as execute:
            with self.assertRaises(ValueError):
                p.publish(self.client, 'macos', 12)
            store.assert_not_called()
            execute.assert_not_called()
        self.assertFalse(any('/artifacts' in call.args[1] for call in self.client.call.call_args_list))

    def test_completed_preview_uses_single_pr_notes_and_skips_stable_completion(self):
        captured = {}
        def execute(command, **kwargs):
            self.assertIn('macos-artifact.py', str(command[1]))
            captured['notes'] = json.loads(Path(kwargs['env']['OKILUM_RELEASE_NOTES']).read_text())
            self.assertEqual(kwargs['env']['GITHUB_SHA'], SHA)
        with patch.object(p, 'R2', return_value=self.store), patch.object(p.subprocess, 'run', side_effect=execute), \
             patch.object(p.prepare, 'complete') as complete:
            p.publish(self.client, 'macos', 12)
            complete.assert_not_called()
        payload = captured['notes']
        self.assertEqual(payload['preview'], {'pr': 7})
        self.assertEqual(payload['items'], [{'pr': 7, 'kind': 'feature', 'text': 'Jump to projects'}])
        self.assertEqual(self.store.put.call_args.args[0], f'okilum/releases/{SHA}/macos/beta/notes.json')
        self.assertEqual(self.pr_reads, 2)

    def test_waiting_for_source_completion_does_not_repeat_pr_api_queries(self):
        reads = []
        def call(method, path, *args, **kwargs):
            if path == '/actions/runs/12':
                reads.append(path)
                if len(reads) == 1:
                    return dict(self.run, status='running')
            return self.call(method, path, *args, **kwargs)
        self.client.call.side_effect = call
        with patch.object(p, 'R2', return_value=self.store), patch.object(p.subprocess, 'run'), \
             patch.object(p.prepare, 'complete'), patch.object(p.time, 'sleep') as sleep:
            p.publish(self.client, 'macos', 12)
        self.assertEqual(len(reads), 2)
        self.assertEqual(self.pr_reads, 2, 'Validate only at success and after download')
        sleep.assert_called_once_with(5)

    def test_superseded_during_download_never_publishes(self):
        self.change_after_download = True
        with patch.object(p, 'R2', return_value=self.store), patch.object(p.subprocess, 'run') as execute:
            with self.assertRaises(ValueError):
                p.publish(self.client, 'macos', 12)
            execute.assert_not_called()
        self.store.put.assert_not_called()

    def test_preview_never_rolls_back_newer_main_build(self):
        with patch.object(p, 'R2', return_value=self.store), patch.object(p, 'published_build', return_value=6000), \
             patch.object(p.subprocess, 'run') as execute:
            p.publish(self.client, 'macos', 12)
            execute.assert_not_called()
        self.store.put.assert_not_called()

    def test_notes_title_fallback_kind_and_hidden_prefix_are_explicit(self):
        for title, kind, text in [('feat: New palette', 'feature', 'New palette'),
                                  ('fix: Open files', 'fix', 'Open files'),
                                  ('ci: Preview builds', 'fix', 'Preview builds')]:
            pr = dict(self.pr, title=title, body='', labels=['enhancement'])
            payload = notes.prepare_preview(self.store, pr, 5100, SHA)
            self.assertEqual(payload['items'], [{'pr': 7, 'kind': kind, 'text': text}])
            self.assertEqual(payload['preview'], {'pr': 7})
        with self.assertRaises(ValueError):
            notes.prepare_preview(self.store, self.pr, 5100, MAIN)
