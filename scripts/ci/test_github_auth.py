"""Authentication isolation, refresh, secret handling and explicit quota failures."""
import base64
import contextlib
import datetime
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import urllib.error
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location('auth_test', Path(__file__).with_name('github_auth.py'))
auth = importlib.util.module_from_spec(spec)
spec.loader.exec_module(auth)
spec = importlib.util.spec_from_file_location('auth_transport_test', Path(__file__).with_name('github-macos.py'))
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)
APP = dict(OKILUM_GITHUB_APP_ID='123', OKILUM_GITHUB_APP_INSTALLATION_ID='456',
           OKILUM_GITHUB_APP_KEY='private-key-sentinel', MIRROR_TOKEN='pat-sentinel')


def response(data, headers=None):
    stream = io.BytesIO(json.dumps(data).encode())
    stream.headers = headers or {}
    return stream


def token_result(token, expires):
    return {'token': token, 'expires_at': datetime.datetime.fromtimestamp(
        expires, datetime.timezone.utc).isoformat().replace('+00:00', 'Z')}


class CredentialTests(unittest.TestCase):
    def test_pat_fallback_without_app_configuration_never_mints(self):
        request = Mock()
        c = auth.Credentials(env={'MIRROR_TOKEN': 'legacy'}, opener=request)
        self.assertEqual(c.token, 'legacy')
        self.assertFalse(c.is_app)
        request.assert_not_called()

    def test_partial_or_invalid_app_configuration_never_uses_pat(self):
        for key in auth.APP_FIELDS:
            with self.subTest(key=key), self.assertRaises(auth.CredentialError):
                auth.Credentials(env={'MIRROR_TOKEN': 'legacy', key: APP[key]})
        for key in auth.APP_FIELDS[:2]:
            with self.subTest(key=key), self.assertRaises(auth.CredentialError):
                auth.Credentials(env=dict(APP, **{key: '../invalid'}))

    def test_app_precedes_pat_caches_and_refreshes_for_long_queued_jobs(self):
        now = [1000]
        opener = Mock(side_effect=[response(token_result('installation-one', 4600)),
                                  response(token_result('installation-two', 7901))])
        with patch.object(auth, 'signed_jwt', return_value='jwt-sentinel') as sign:
            c = auth.Credentials(env=APP, clock=lambda: now[0], opener=opener)
            self.assertEqual(c.token, 'installation-one')
            now[0] = 4299
            self.assertEqual(c.token, 'installation-one')
            self.assertEqual(opener.call_count, 1)
            now[0] = 4301
            self.assertEqual(c.token, 'installation-two')
            self.assertEqual(sign.call_count, 2)
        request = opener.call_args.args[0]
        self.assertEqual(request.method, 'POST')
        self.assertEqual(request.full_url, 'https://api.github.com/app/installations/456/access_tokens')
        self.assertEqual(request.get_header('Authorization'), 'Bearer jwt-sentinel')
        self.assertEqual(json.loads(request.data), {'repositories': ['okilum'], 'permissions': auth.PERMISSIONS})

    def test_mint_failure_never_falls_back_or_echoes_response_secrets(self):
        error = urllib.error.HTTPError('https://api.github.com', 403, 'Forbidden',
            {'X-RateLimit-Remaining': '0', 'X-RateLimit-Reset': '12345'},
            io.BytesIO(b'{"message":"API rate limit exceeded private-key-sentinel jwt-sentinel"}'))
        with patch.object(auth, 'signed_jwt', return_value='jwt-sentinel'):
            c = auth.Credentials(env=APP, clock=lambda: 1000, opener=Mock(side_effect=error))
            with self.assertRaises(auth.RateLimitError) as caught:
                _ = c.token
        text = str(caught.exception)
        self.assertIn('quota exhausted', text)
        self.assertIn('reset_epoch=12345', text)
        for secret in ['private-key-sentinel', 'jwt-sentinel', 'pat-sentinel']:
            self.assertNotIn(secret, text)

    def test_expired_malformed_or_missing_installation_token_is_rejected(self):
        for result in [{}, token_result('', 4600), token_result('token', 1100),
                       {'token': 'token', 'expires_at': 'invalid'}]:
            with self.subTest(result=result), patch.object(auth, 'signed_jwt', return_value='jwt'):
                with self.assertRaises(auth.CredentialError):
                    _ = auth.Credentials(env=APP, clock=lambda: 1000,
                                         opener=Mock(return_value=response(result))).token

    def test_quota_logged_once_without_tokens_and_zero_fails_before_push(self):
        opener = Mock(return_value=response({'resources': {'core': {'remaining': 0, 'limit': 5000, 'reset': 12345}}}))
        c = auth.Credentials(env={'MIRROR_TOKEN': 'secret-pat'}, opener=opener)
        output = io.StringIO()
        with contextlib.redirect_stdout(output), self.assertRaises(auth.RateLimitError):
            c.log_quota()
        self.assertEqual(opener.call_args.args[0].full_url, 'https://api.github.com/rate_limit')
        c.note_quota({'X-RateLimit-Remaining': '0'})
        self.assertEqual(output.getvalue().count('quota remaining='), 1)
        self.assertNotIn('secret-pat', output.getvalue())

    def test_exec_keeps_installation_token_but_drops_app_private_material(self):
        fake = Mock(token='installation-sentinel')
        with patch.object(auth, 'Credentials', return_value=fake), patch.dict(os.environ, APP, clear=True), \
             patch.object(sys, 'argv', ['github_auth.py', 'exec', '--', 'bash', 'mirror.sh']), \
             patch.object(auth.os, 'execvpe') as execute:
            auth.main()
        command, args, env = execute.call_args.args
        self.assertEqual((command, args), ('bash', ['bash', 'mirror.sh']))
        self.assertEqual(env['MIRROR_TOKEN'], 'installation-sentinel')
        for key in auth.APP_FIELDS:
            self.assertNotIn(key, env)
        fake.log_quota.assert_called_once()

    @unittest.skipUnless(shutil.which('openssl'), 'openssl required on CI runners')
    def test_rs256_jwt_is_cryptographically_valid_and_within_ten_minutes(self):
        private = subprocess.check_output(['openssl', 'genpkey', '-algorithm', 'RSA',
            '-pkeyopt', 'rsa_keygen_bits:2048'], stderr=subprocess.DEVNULL)
        public = subprocess.check_output(['openssl', 'pkey', '-pubout'], input=private, stderr=subprocess.DEVNULL)
        token = auth.signed_jwt('123', private.decode(), 1000)
        header, payload, signature = token.split('.')
        decode = lambda s: base64.urlsafe_b64decode(s + '=' * (-len(s) % 4))
        self.assertEqual(json.loads(decode(header)), {'alg': 'RS256', 'typ': 'JWT'})
        self.assertEqual(json.loads(decode(payload)), {'iat': 940, 'exp': 1540, 'iss': '123'})
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); (root/'public').write_bytes(public); (root/'signature').write_bytes(decode(signature))
            subprocess.run(['openssl', 'dgst', '-sha256', '-verify', str(root/'public'),
                '-signature', str(root/'signature')], input=(header+'.'+payload).encode(),
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)

    def test_openssl_failure_does_not_echo_key_or_stderr(self):
        error = subprocess.CalledProcessError(1, 'openssl', stderr=b'private-key-sentinel')
        with patch.object(auth.subprocess, 'run', side_effect=error), self.assertRaises(auth.CredentialError) as caught:
            auth.signed_jwt('123', 'private-key-sentinel', 1000)
        self.assertNotIn('private-key-sentinel', str(caught.exception))


class MirrorTests(unittest.TestCase):
    def test_mirror_uses_askpass_without_credentials_in_git_arguments(self):
        root = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            fake = directory / 'git'
            fake.write_text("""#!/usr/bin/env python3
import json, os, subprocess, sys
from pathlib import Path
args = sys.argv[1:]
assert os.environ['MIRROR_TOKEN'] not in ' '.join(args)
with Path(os.environ['GIT_TEST_LOG']).open('a') as log:
    log.write(json.dumps(args) + '\\n')
if 'push' in args:
    askpass = os.environ['GIT_ASKPASS']
    assert subprocess.check_output([askpass, 'Username']).strip() == b'x-access-token'
    assert subprocess.check_output([askpass, 'Password']).strip().decode() == os.environ['MIRROR_TOKEN']
""")
            fake.chmod(0o700)
            log = directory / 'git.jsonl'
            env = dict(os.environ, PATH=str(directory)+os.pathsep+os.environ['PATH'],
                       MIRROR_TOKEN='test-installation-secret', OKILUM_GITHUB_AUTH_RESOLVED='1',
                       RELEASE_TAG='beta', GIT_TEST_LOG=str(log))
            result = subprocess.run(['bash', str(root/'scripts/releases/mirror.sh')],
                                    env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            push = calls[-1]
            self.assertIn('https://github.com/BeFeast/okilum.git', push)
            self.assertIn('refs/remotes/origin/main:refs/heads/main', push)
            self.assertIn('+refs/tags/beta:refs/tags/beta', push)
            self.assertNotIn('test-installation-secret', log.read_text()+result.stdout+result.stderr)


class BridgeQuotaTests(unittest.TestCase):
    def test_exhausted_quota_is_named_and_never_retried_or_followed_by_cleanup_api(self):
        error = urllib.error.HTTPError('https://api.github.com', 403, 'Forbidden',
            {'X-RateLimit-Remaining': '0', 'X-RateLimit-Reset': '12345'},
            io.BytesIO(b'{"message":"API rate limit exceeded"}'))
        with patch.object(bridge.urllib.request, 'urlopen', side_effect=error) as request:
            api = bridge.GitHub('secret')
            sleep = Mock()
            with self.assertRaises(bridge.RateLimitExhausted) as caught:
                bridge.hosted_request(api, 'actions/runs', sleep=sleep)
            with self.assertRaises(bridge.RateLimitExhausted):
                api.request('git/refs/heads/owned', 'DELETE')
        self.assertEqual(request.call_count, 1)
        sleep.assert_not_called()
        self.assertIn('quota exhausted', str(caught.exception))

    def test_secondary_limit_body_is_recognized_even_with_core_remaining(self):
        error = urllib.error.HTTPError('url', 403, 'Forbidden', {'X-RateLimit-Remaining': '40'},
                                     io.BytesIO(b'{"message":"You have exceeded a secondary rate limit"}'))
        self.assertIsInstance(auth.http_error(error, 'GitHub GET'), auth.RateLimitError)

    def test_successive_api_requests_log_remaining_quota_only_once(self):
        responses = [response({}, {'X-RateLimit-Remaining': '100', 'X-RateLimit-Limit': '5000'}),
                     response({}, {'X-RateLimit-Remaining': '99', 'X-RateLimit-Limit': '5000'})]
        output = io.StringIO()
        with patch.object(bridge.urllib.request, 'urlopen', side_effect=responses), contextlib.redirect_stdout(output):
            api = bridge.GitHub('secret')
            api.request('actions/runs'); api.request('actions/runs')
        self.assertEqual(output.getvalue().count('quota remaining='), 1)
        self.assertNotIn('secret', output.getvalue())

    def test_polling_uses_jitter_between_thirty_and_sixty_seconds(self):
        values = [bridge.poll_delay() for _ in range(50)]
        self.assertTrue(all(30 <= x <= 60 for x in values))
        self.assertGreater(len(set(values)), 1)

    def test_every_pat_workflow_step_also_receives_all_app_secrets(self):
        root = Path(__file__).resolve().parents[2]
        for workflow in (root/'.forgejo/workflows').glob('*.yml'):
            text = workflow.read_text()
            for suffix in text.split('MIRROR_TOKEN: ${{ secrets.OKILUM_GITHUB_MIRROR }}')[1:]:
                with self.subTest(workflow=workflow.name):
                    for field in auth.APP_FIELDS:
                        self.assertIn(field + ': ${{ secrets.' + field + ' }}', '\n'.join(suffix.splitlines()[:4]))
        self.assertNotIn('OKILUM_GITHUB_APP_KEY', '\n'.join(p.read_text() for p in (root/'.github/workflows').glob('*.yml')))


if __name__ == '__main__':
    unittest.main()
