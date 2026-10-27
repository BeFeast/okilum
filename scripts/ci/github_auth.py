#!/usr/bin/env python3
"""Repository-scoped GitHub App credentials, with an explicit legacy PAT fallback."""
import base64
import datetime
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

API = 'https://api.github.com'
APP_FIELDS = ('OKILUM_GITHUB_APP_ID', 'OKILUM_GITHUB_APP_INSTALLATION_ID', 'OKILUM_GITHUB_APP_KEY')
PERMISSIONS = {'contents': 'write', 'workflows': 'write', 'actions': 'write'}


class CredentialError(RuntimeError):
    pass


class RateLimitError(CredentialError):
    pass


def http_error(error, operation):
    """Never echo response bodies: they can contain credential-bearing input."""
    body = error.read(65536).decode('utf-8', 'replace').lower()
    remaining = error.headers.get('X-RateLimit-Remaining', '') if error.headers else ''
    reset = error.headers.get('X-RateLimit-Reset', '') if error.headers else ''
    reset = reset if re.fullmatch(r'[0-9]+', reset) else 'unknown'
    if error.code in (403, 429) and ('secondary rate limit' in body or 'abuse detection' in body):
        return RateLimitError(f'{operation}: GitHub secondary rate limit (HTTP {error.code})')
    if error.code in (403, 429) and (remaining == '0' or 'rate limit exceeded' in body):
        return RateLimitError(f'{operation}: GitHub API quota exhausted (remaining=0, reset_epoch={reset}, HTTP {error.code})')
    reason = {401: 'authentication rejected', 403: 'permission denied', 429: 'rate limited'}.get(error.code, 'request failed')
    return CredentialError(f'{operation}: HTTP {error.code} ({reason})')


def signed_jwt(app_id, private_key, now):
    def encode(value):
        return base64.urlsafe_b64encode(json.dumps(value, separators=(',', ':')).encode()).rstrip(b'=')
    unsigned = encode({'alg': 'RS256', 'typ': 'JWT'}) + b'.' + encode({
        'iat': int(now) - 60, 'exp': int(now) + 540, 'iss': app_id})
    # Unlinked private fd, inherited only by openssl. No key text/path in argv,
    # logs, checkout or persistent runner files; subprocess errors stay generic.
    with tempfile.TemporaryFile() as key:
        key.write(private_key.encode()); key.flush(); key.seek(0)
        try:
            result = subprocess.run(['openssl', 'dgst', '-sha256', '-sign', f'/dev/fd/{key.fileno()}'],
                                    input=unsigned, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    pass_fds=(key.fileno(),), timeout=30, check=True)
        except (OSError, subprocess.SubprocessError):
            raise CredentialError('GitHub App JWT signing failed (openssl/key unavailable or invalid)') from None
    return (unsigned + b'.' + base64.urlsafe_b64encode(result.stdout).rstrip(b'=')).decode()


class Credentials:
    def __init__(self, fallback=None, *, env=None, clock=time.time, opener=None):
        env = os.environ if env is None else env
        self.app_id, self.installation_id, self.private_key = (env.get(k, '') for k in APP_FIELDS)
        present = [bool(env.get(k)) for k in APP_FIELDS]
        if any(present) and not all(present):
            raise CredentialError('Incomplete GitHub App configuration: set all three App secrets or none')
        self.is_app = all(present)
        if self.is_app and not all(re.fullmatch(r'[1-9][0-9]*', v) for v in (self.app_id, self.installation_id)):
            raise CredentialError('GitHub App and installation IDs must be positive integers')
        self._token = '' if self.is_app else (fallback if fallback is not None else env.get('MIRROR_TOKEN', ''))
        if not self.is_app and not self._token:
            raise CredentialError('GitHub App secrets and legacy MIRROR_TOKEN are unavailable')
        self.expires = 0
        self.clock, self.opener = clock, opener or urllib.request.urlopen
        self.quota_logged = False

    @property
    def token(self):
        if self.is_app and (not self._token or self.clock() >= self.expires - 300):
            jwt = signed_jwt(self.app_id, self.private_key, self.clock())
            request = urllib.request.Request(f'{API}/app/installations/{self.installation_id}/access_tokens',
                method='POST', headers={'Authorization': 'Bearer ' + jwt,
                    'Accept': 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28'},
                data=json.dumps({'repositories': ['okilum'], 'permissions': PERMISSIONS}).encode())
            try:
                with self.opener(request, timeout=30) as response:
                    result = json.load(response)
                token = result['token']
                expires = datetime.datetime.fromisoformat(result['expires_at'].replace('Z', '+00:00')).timestamp()
                if not isinstance(token, str) or not token or expires <= self.clock() + 300:
                    raise ValueError('Invalid token lifetime')
            except urllib.error.HTTPError as error:
                raise http_error(error, 'GitHub App installation token') from None
            except (OSError, ValueError, KeyError, TypeError):
                raise CredentialError('GitHub App installation token unavailable or invalid') from None
            self._token, self.expires = token, expires
        return self._token

    def note_quota(self, headers):
        if self.quota_logged:
            return
        remaining = headers.get('X-RateLimit-Remaining', '') if headers else ''
        limit = headers.get('X-RateLimit-Limit', '') if headers else ''
        if isinstance(remaining, str) and remaining.isdecimal():
            limit = limit if isinstance(limit, str) and limit.isdecimal() else '?'
            print(f'GitHub API core quota remaining={remaining}/{limit} auth={"App" if self.is_app else "PAT"}', flush=True)
            self.quota_logged = True

    def log_quota(self):
        if self.quota_logged:
            return
        request = urllib.request.Request(API + '/rate_limit', headers={
            'Authorization': 'Bearer ' + self.token, 'Accept': 'application/vnd.github+json'})
        try:
            with self.opener(request, timeout=30) as response:
                core = json.load(response)['resources']['core']
            remaining, limit, reset = (core[k] for k in ('remaining', 'limit', 'reset'))
            if any(type(x) is not int for x in (remaining, limit, reset)):
                raise ValueError('Malformed quota')
        except urllib.error.HTTPError as error:
            raise http_error(error, 'GitHub quota read') from None
        except (OSError, ValueError, KeyError, TypeError):
            raise CredentialError('GitHub API quota read unavailable or invalid') from None
        print(f'GitHub API core quota remaining={remaining}/{limit} auth={"App" if self.is_app else "PAT"}', flush=True)
        self.quota_logged = True
        if remaining == 0:
            raise RateLimitError(f'GitHub API quota exhausted (remaining=0, reset_epoch={reset})')


def main():
    if len(sys.argv) < 4 or sys.argv[1:3] != ['exec', '--']:
        raise CredentialError('Usage: github_auth.py exec -- command [args]')
    credentials = Credentials()
    credentials.log_quota()
    env = dict(os.environ, MIRROR_TOKEN=credentials.token, OKILUM_GITHUB_AUTH_RESOLVED='1')
    for key in APP_FIELDS:
        env.pop(key, None)
    os.execvpe(sys.argv[3], sys.argv[3:], env)


if __name__ == '__main__':
    try:
        main()
    except CredentialError as error:
        print(f'GitHub authentication unavailable: {error}', file=sys.stderr)
        raise SystemExit(1)
