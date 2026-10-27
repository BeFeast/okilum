#!/usr/bin/env python3
"""Hosted-only T3 nightly bootstrap + schema check; native round trips deferred (#1787)."""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

UPSTREAM = 'pingdotgg/t3code'
MARKER = '<!-- okilum-t3-nightly-contract -->'
ROOT = Path(__file__).resolve().parents[2]


def request(url, *, data=None, token=None, method=None):
    headers = {'User-Agent': 'okilum-t3-contract', 'Accept': 'application/vnd.github+json'}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    if data is not None:
        headers['Content-Type'] = 'application/json'
        data = json.dumps(data).encode()
    with urllib.request.urlopen(urllib.request.Request(
            url, data=data, headers=headers, method=method), timeout=60) as response:
        return response.read()


def latest(releases):
    candidates = [r for r in releases if not r['draft'] and re.fullmatch(
        r'v\d+\.\d+\.\d+-nightly\.[0-9.]+', r['tag_name'])]
    if not candidates:
        raise ValueError('No official nightly release in response')
    return max(candidates, key=lambda r: r['published_at'])


def extract(data, destination):
    archive = destination.parent / (destination.name + '.tar.gz')
    archive.write_bytes(data)
    destination.mkdir()
    with tarfile.open(archive) as packed:
        # data filter rejects traversal and links outside destination.
        packed.extractall(destination, filter='data')


def command(args, *, cwd=None, env=None, timeout=180):
    return subprocess.check_output(args, cwd=cwd, env=env, timeout=timeout,
                                   stderr=subprocess.STDOUT, text=True)


def snapshot(upstream, output):
    workspace = (upstream / 'pnpm-workspace.yaml').read_text()
    matches = re.findall(r'^  effect: ([0-9A-Za-z.+-]+)$', workspace, re.M)
    if len(matches) != 1:
        raise ValueError('Cannot identify exact upstream Effect version')
    # Only the contracts runtime dependency; no upstream lifecycle/build scripts.
    dependencies = upstream.parent / 'schema-dependencies'
    dependencies.mkdir()
    (dependencies / 'package.json').write_text('{"private":true}')
    command(['npm', 'install', '--prefix', str(dependencies), '--ignore-scripts',
             '--no-audit', '--no-fund', '--workspaces=false', '--package-lock=false',
             '--no-save', 'effect@' + matches[0]], timeout=300)
    (upstream / 'node_modules').symlink_to(dependencies / 'node_modules', target_is_directory=True)
    command(['node', str(ROOT / 'scripts/t3-contract/schema.mjs'), str(upstream), str(output)])
    return matches[0]


def bootstrap(binary, version, directory):
    home = directory / 'home'
    home.mkdir(mode=0o700)
    base = home / 't3'
    env = {**os.environ, 'HOME': str(home), 'T3CODE_HOME': str(base),
           'XDG_CONFIG_HOME': str(home / '.config'), 'XDG_CACHE_HOME': str(home / '.cache')}
    # No provider credentials or inherited configuration enters the disposable server.
    for key in list(env):
        if any(word in key for word in ('TOKEN', 'SECRET', 'API_KEY', 'PASSWORD')):
            del env[key]
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'
    # Logs can contain pairing credentials, so keep them private and never upload them.
    log = directory / 'private-server.log'
    descriptor = None
    with log.open('w') as stream:
        process = subprocess.Popen([str(binary), 'serve', '--base-dir', str(base),
                                    '--host', '127.0.0.1', '--port', str(port)],
                                   cwd=home, env=env, stdout=stream, stderr=stream,
                                   start_new_session=True)
        try:
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError('Disposable server exited before descriptor readiness')
                try:
                    descriptor = json.loads(request(origin + '/.well-known/t3/environment'))
                    break
                except (OSError, ValueError):
                    time.sleep(0.25)
            if descriptor is None:
                raise TimeoutError('Disposable server readiness exceeded 60s')
            if descriptor['serverVersion'] != version.removeprefix('v'):
                raise ValueError('Downloaded server descriptor version mismatch')
            pairing = json.loads(command([str(binary), 'auth', 'pairing', 'create',
                                          '--base-dir', str(base), '--json'], env=env))
            body = urllib.parse.urlencode({
                'grant_type': 'urn:ietf:params:oauth:grant-type:token-exchange',
                'subject_token': pairing['credential'],
                'subject_token_type': 'urn:t3:params:oauth:token-type:environment-bootstrap',
                'requested_token_type': 'urn:ietf:params:oauth:token-type:access_token',
                'scope': 'orchestration:read orchestration:operate',
                'client_label': 'Okilum disposable nightly contract',
            }).encode()
            req = urllib.request.Request(origin + '/oauth/token', data=body,
                                         headers={'Content-Type': 'application/x-www-form-urlencoded'})
            with urllib.request.urlopen(req, timeout=15) as response:
                session = json.load(response)
            if not session.get('access_token') or session.get('token_type') != 'Bearer':
                raise ValueError('Pairing exchange did not issue a Bearer session')
            state = json.loads(request(origin + '/api/auth/session', token=session['access_token']))
            if state.get('authenticated') is not True:
                raise ValueError('Paired session is not authenticated')
            return {'serverVersion': descriptor['serverVersion'], 'paired': True,
                    'client_round_trip': 'NOT_ENABLED: waiting for #1787 slice 2'}
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)


def run(output):
    if os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted':
        raise RuntimeError('Server contract probe runs only on GitHub-hosted Actions')
    receipt = {'client_round_trip': 'NOT_ENABLED: waiting for #1787 slice 2'}
    output.mkdir(parents=True, exist_ok=True)
    try:
        command(['python3', '-m', 'unittest', 'discover', '-s',
                 str(ROOT / 'scripts/t3-contract'), '-p', 'test_*.py', '-v'])
        receipt['harness_regressions'] = 'PASS'
        release = latest(json.loads(request(f'https://api.github.com/repos/{UPSTREAM}/releases?per_page=30')))
        receipt['version'] = release['tag_name']
        receipt['release_url'] = release['html_url']
        asset = next(a for a in release['assets'] if a['name'] == f"t3-{release['tag_name'][1:]}-linux-x64.tar.gz")
        data = request(asset['browser_download_url'])
        digest = 'sha256:' + hashlib.sha256(data).hexdigest()
        if asset.get('digest') != digest:
            raise ValueError('Release asset SHA256 missing or mismatched')
        receipt['asset_digest'] = digest
        with tempfile.TemporaryDirectory(prefix='okilum-t3-contract-') as temporary:
            root = Path(temporary)
            extract(data, root / 'binary')
            binaries = [p for p in (root / 'binary').rglob('t3') if p.is_file() and os.access(p, os.X_OK)]
            if len(binaries) != 1:
                raise ValueError('Expected one official T3 executable')
            # Bootstrap is independent of schema drift; run both and retain both outcomes.
            try:
                receipt['bootstrap'] = bootstrap(binaries[0], release['tag_name'], root)
            except Exception as error:
                receipt['bootstrap_error'] = type(error).__name__ + ': ' + str(error)
            source = request(f"https://codeload.github.com/{UPSTREAM}/tar.gz/refs/tags/{release['tag_name']}")
            receipt['source_archive_sha256'] = hashlib.sha256(source).hexdigest()
            extract(source, root / 'source')
            sources = list((root / 'source').iterdir())
            if len(sources) != 1 or not sources[0].is_dir():
                raise ValueError('Unexpected upstream source archive layout')
            receipt['effect_version'] = snapshot(sources[0], output / 'actual-schema.json')
        expected = (ROOT / 'scripts/t3-contract/snapshot.json').read_text()
        actual = (output / 'actual-schema.json').read_text()
        delta = ''.join(difflib.unified_diff(expected.splitlines(True), actual.splitlines(True),
                                           fromfile='committed', tofile=release['tag_name']))
        (output / 'schema.diff').write_text(delta)
        receipt['schema_drift'] = bool(delta)
        receipt['status'] = 'FAIL' if delta or receipt.get('bootstrap_error') else 'PASS'
    except Exception as error:
        # Do not include subprocess output: CLI diagnostics may contain one-time credentials.
        receipt['status'] = 'ERROR'
        receipt['error'] = type(error).__name__ + ': ' + (
            'subprocess failed' if isinstance(error, subprocess.SubprocessError) else str(error))
    (output / 'result.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return receipt


def report(output):
    result = json.loads((output / 'result.json').read_text())
    if result['status'] == 'PASS':
        return
    repository = os.environ['GITHUB_REPOSITORY']
    token = os.environ['GITHUB_TOKEN']
    base = f'https://api.github.com/repos/{repository}'
    # One normal Actions issue inventory read, not workflow/run polling.
    issues = json.loads(request(base + '/issues?state=all&labels=t3-contract&per_page=100', token=token))
    if len(issues) >= 100:
        raise ValueError('Incident inventory may be truncated; refusing another issue')
    matches = [i for i in issues if MARKER in (i.get('body') or '') and 'pull_request' not in i]
    if len(matches) > 1:
        raise ValueError('Multiple T3 contract incident issues; refusing another')
    delta = (output / 'schema.diff').read_text() if (output / 'schema.diff').exists() else '(schema generation unavailable)'
    run_url = f"https://github.com/{repository}/actions/runs/{os.environ['GITHUB_RUN_ID']}"
    body = (MARKER + '\nNightly T3 protocol contract observation (#1789). Client round trips remain OFF until #1787 slice 2.\n\n'
            + '```json\n' + json.dumps(result, indent=2) + '\n```\n\n' + run_url
            + '\n\n```diff\n' + delta[:45000] + '\n```\nFull diff and actual schema: workflow artifact.\n')
    if matches:
        request(base + f"/issues/{matches[0]['number']}", token=token,
                method='PATCH', data={'body': body, 'state': 'open'})
    else:
        # The label must exist; GitHub ignores nonexistent labels on issue creation.
        try:
            request(base + '/labels/t3-contract', token=token)
        except urllib.error.HTTPError as error:
            if error.code != 404:
                raise
            request(base + '/labels', token=token, data={'name': 't3-contract', 'color': 'd876e3'})
        request(base + '/issues', token=token, data={
            'title': 'T3 nightly protocol contract drift or bootstrap failure',
            'body': body, 'labels': ['t3-contract']})


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['run', 'report'])
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.command == 'report':
        report(args.output)
    else:
        result = run(args.output.resolve())
        print(json.dumps(result, indent=2))
        raise SystemExit(0 if result['status'] == 'PASS' else 1)
