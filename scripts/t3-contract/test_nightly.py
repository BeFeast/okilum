"""Pure harness controls: no server, network, provider or shared builder."""
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import Mock, patch

import nightly


class ContractHarness(unittest.TestCase):
    def test_latest_is_published_nightly_not_list_order_or_stable(self):
        releases = [
            {'tag_name': 'v1.0.0', 'draft': False, 'published_at': '2030'},
            {'tag_name': 'v0.1.4-nightly.20261027.3783', 'draft': False, 'published_at': '2026-10-27'},
            {'tag_name': 'v0.1.4-nightly.20261026.3782', 'draft': False, 'published_at': '2026-10-26'},
            {'tag_name': 'v0.1.4-nightly.20261028.3784', 'draft': True, 'published_at': '2026-10-28'},
        ]
        self.assertEqual(nightly.latest(releases)['tag_name'], releases[1]['tag_name'])
        with self.assertRaises(ValueError):
            nightly.latest(releases[:1])

    def test_archive_escape_is_rejected_with_positive_control(self):
        def archive(name):
            buffer = io.BytesIO()
            with tarfile.open(fileobj=buffer, mode='w:gz') as packed:
                info = tarfile.TarInfo(name); info.size = 2
                packed.addfile(info, io.BytesIO(b'ok'))
            return buffer.getvalue()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            nightly.extract(archive('file'), root / 'good')
            self.assertEqual((root / 'good/file').read_bytes(), b'ok')
            with self.assertRaises(tarfile.FilterError):
                nightly.extract(archive('../escaped'), root / 'bad')
            self.assertFalse((root / 'escaped').exists())

    def test_non_hosted_execution_is_rejected_before_network_or_spawn(self):
        with patch.dict(os.environ, {'GITHUB_ACTIONS': 'true', 'RUNNER_ENVIRONMENT': 'self-hosted'}), \
                patch.object(nightly, 'request') as request, patch.object(nightly.subprocess, 'Popen') as spawn:
            with self.assertRaisesRegex(RuntimeError, 'GitHub-hosted'):
                nightly.run(Path('/not-created'))
            request.assert_not_called(); spawn.assert_not_called()

    def test_bootstrap_pairs_in_private_home_and_terminates_child_on_mismatch(self):
        for version, success in [('test-version', True), ('wrong-version', False)]:
            with self.subTest(version=version), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                process = Mock(pid=12345)
                process.poll.return_value = None
                session = io.BytesIO(b'{"access_token":"ephemeral","token_type":"Bearer"}')
                with patch.dict(os.environ, GITHUB_TOKEN='private', PROVIDER_API_KEY='private',
                                AWS_ACCESS_KEY_ID='private', SSH_PRIVATE_KEY='private',
                                SSH_AUTH_SOCK='/private/agent', T3CODE_MODE='private'), \
                        patch.object(nightly.subprocess, 'Popen', return_value=process) as spawn, \
                        patch.object(nightly, 'request', side_effect=[
                            json.dumps({'serverVersion': version}).encode(),
                            b'{"authenticated":true}']) as request, \
                        patch.object(nightly, 'native_roundtrip', return_value={
                            'paired': True, 'snapshot': True, 'resume': True,
                            'resume_without_snapshot': True}) as command, \
                        patch.object(nightly.urllib.request, 'urlopen', return_value=session), \
                        patch.object(nightly.os, 'killpg') as kill:
                    if success:
                        result = nightly.bootstrap(Path('/not-executed'), 'vtest-version', root, Path('/probe'))
                        self.assertTrue(result['native_client']['paired'])
                        self.assertIn('dispatchCommand', result['deferred_operations'])
                        self.assertNotIn('ephemeral', json.dumps(result))
                        self.assertEqual(command.call_count, 1)
                    else:
                        with self.assertRaisesRegex(ValueError, 'version mismatch'):
                            nightly.bootstrap(Path('/not-executed'), 'vtest-version', root, Path('/probe'))
                        command.assert_not_called()
                    child_env = spawn.call_args.kwargs['env']
                    self.assertNotIn('GITHUB_TOKEN', child_env)
                    self.assertNotIn('PROVIDER_API_KEY', child_env)
                    for key in ['AWS_ACCESS_KEY_ID', 'SSH_PRIVATE_KEY', 'SSH_AUTH_SOCK', 'T3CODE_MODE']:
                        self.assertNotIn(key, child_env)
                    self.assertEqual(child_env['HOME'], str(root / 'home'))
                    self.assertTrue(spawn.call_args.kwargs['start_new_session'])
                    kill.assert_called_once_with(12345, nightly.signal.SIGTERM)
                    process.wait.assert_called_once_with(timeout=15)

    def test_cleanup_timeout_preserves_primary_bootstrap_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            process = Mock(pid=12345)
            process.poll.return_value = None
            process.wait.side_effect = [nightly.subprocess.TimeoutExpired('server', 15),
                                        nightly.subprocess.TimeoutExpired('server', 5)]
            with patch.object(nightly.subprocess, 'Popen', return_value=process), \
                    patch.object(nightly, 'request', return_value=b'{"serverVersion":"wrong"}'), \
                    patch.object(nightly.os, 'killpg') as kill:
                with self.assertRaisesRegex(RuntimeError,
                        r'Bootstrap failed .*version mismatch.*cleanup failed .*TimeoutExpired'):
                    nightly.bootstrap(Path('/not-executed'), 'vexpected', Path(directory), Path('/probe'))
                self.assertEqual(kill.call_count, 2)
                self.assertEqual(process.wait.call_count, 2)

    def test_native_probe_gets_secret_only_on_stdin_and_requires_all_receipts(self):
        valid = {'paired': True, 'snapshot': True, 'resume': True, 'resume_without_snapshot': True}
        with patch.object(nightly, 'fixture_thread', return_value='idle-thread'), \
                patch.object(nightly, 'command', return_value='{"credential":"one-time-secret"}'), \
                patch.object(nightly.subprocess, 'run', return_value=Mock(
                    returncode=0, stdout=json.dumps(valid))) as launch:
            actual = nightly.native_roundtrip(Path('/server'), Path('/base'), {},
                                              'http://127.0.0.1:1', Path('/probe'))
            self.assertEqual(actual, valid)
            self.assertEqual(launch.call_args.args[0], ['/probe'])
            self.assertEqual(json.loads(launch.call_args.kwargs['input']), {
                'origin': 'http://127.0.0.1:1', 'credential': 'one-time-secret', 'thread_id': 'idle-thread'})
            self.assertNotIn('one-time-secret', json.dumps(launch.call_args.kwargs['env']))
            launch.return_value.stdout = '{"paired":true}'
            with self.assertRaisesRegex(ValueError, 'Incomplete'):
                nightly.native_roundtrip(Path('/server'), Path('/base'), {},
                                         'http://127.0.0.1:1', Path('/probe'))
            launch.return_value.returncode = 1
            launch.return_value.stderr = 'one-time-secret'
            with self.assertRaisesRegex(ValueError, 'probe failed') as raised:
                nightly.native_roundtrip(Path('/server'), Path('/base'), {},
                                         'http://127.0.0.1:1', Path('/probe'))
            self.assertNotIn('one-time-secret', str(raised.exception))

    def test_mcp_setup_creates_only_idle_fixture_never_claims_native_dispatch(self):
        def response(value):
            result = Mock()
            result.__enter__ = Mock(return_value=result)
            result.__exit__ = Mock(return_value=False)
            result.headers = {'Content-Type': 'application/json'}
            result.read.return_value = json.dumps(value).encode()
            return result
        for run_id in [None, 'forbidden-agent-run']:
            with self.subTest(run_id=run_id), patch.object(nightly, 'command', return_value='setup-secret'), \
                    patch.object(nightly.urllib.request, 'urlopen', side_effect=[
                        response({'jsonrpc': '2.0', 'id': 1, 'result': {}}), response({}),
                        response({'jsonrpc': '2.0', 'id': 2, 'result': {'structuredContent': {
                            'threadId': 'idle-thread', 'runId': run_id}}})]) as http:
                if run_id is None:
                    self.assertEqual(nightly.fixture_thread(Path('/server'), Path('/base'), {},
                                     'http://127.0.0.1:1'), 'idle-thread')
                else:
                    with self.assertRaisesRegex(ValueError, 'idle thread'):
                        nightly.fixture_thread(Path('/server'), Path('/base'), {}, 'http://127.0.0.1:1')
                payload = json.loads(http.call_args.args[0].data)
                self.assertEqual(payload['params']['name'], 't3_thread_launch')
                args = payload['params']['arguments']
                self.assertTrue(args['scratch'])
                self.assertNotIn('message', args)
                self.assertNotIn('attachments', args)

    def test_schema_snapshot_covers_all_deferred_wire_directions_and_auth(self):
        value = json.loads((Path(__file__).parent / 'snapshot.json').read_text())
        for method in ['subscribeShell', 'subscribeThread', 'dispatchCommand']:
            for direction in ['input', 'output']:
                self.assertIn(method + '.' + direction, value)
        self.assertIn('ExecutionEnvironmentCapabilities', value)
        self.assertIn('ServerAuthDescriptor', value)
        self.assertIn('AuthTokenExchangeRequest', value)

    def test_report_updates_same_issue_and_retains_failure_diff_and_version(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            (output / 'result.json').write_text(json.dumps({'status': 'FAIL', 'version': 'test-nightly',
                                                          'schema_drift': True}))
            (output / 'schema.diff').write_text('-old\n+new\n')
            with patch.dict(os.environ, GITHUB_REPOSITORY='owner/repo', GITHUB_TOKEN='private', GITHUB_RUN_ID='42'), \
                    patch.object(nightly, 'request', return_value=json.dumps([
                        {'number': 7, 'body': nightly.MARKER, 'state': 'closed'}]).encode()) as request:
                nightly.report(output)
                self.assertEqual(request.call_count, 2)
                update = request.call_args
                self.assertTrue(update.args[0].endswith('/issues/7'))
                self.assertEqual(update.kwargs['method'], 'PATCH')
                self.assertEqual(update.kwargs['data']['state'], 'open')
                body = update.kwargs['data']['body']
                self.assertIn('test-nightly', body); self.assertIn('-old\n+new', body)
                self.assertNotIn('private', body)
            (output / 'result.json').write_text('{"status":"PASS"}')
            with patch.object(nightly, 'request') as request:
                nightly.report(output)
                request.assert_not_called()

    def test_duplicate_incidents_are_an_error_not_another_issue(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            (output / 'result.json').write_text('{"status":"ERROR"}')
            with patch.dict(os.environ, GITHUB_REPOSITORY='owner/repo', GITHUB_TOKEN='private'), \
                    patch.object(nightly, 'request', return_value=json.dumps([
                        {'number': 1, 'body': nightly.MARKER}, {'number': 2, 'body': nightly.MARKER}]).encode()) as request:
                with self.assertRaisesRegex(ValueError, 'Multiple'):
                    nightly.report(output)
                self.assertEqual(request.call_count, 1)


if __name__ == '__main__':
    unittest.main()
