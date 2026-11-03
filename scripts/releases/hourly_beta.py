"""Hourly beta admission shared by Forgejo and the public GitHub producers.

Only the current main tip is eligible. Never infer merge health from PR checks:
fast's push jobs run the same required suites on the actual merge commit.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import urllib.request

REPOSITORY = 'BeFeast/okilum'
WORKFLOWS = {'linux': 'linux-release.yml', 'macos': 'macos-release.yml',
             'windows': 'windows-diagnostic.yml'}
REQUIRED = ('fast / check (push)', 'fast / shell-full (push)')


class PublicFeeds:
    def call(self, method, key):
        if method != 'GET' or not key.startswith('okilum/'):
            raise ValueError('Beta admission can only read public feeds')
        result = subprocess.run(['curl', '--silent', '--show-error', '--location',
            '--max-time', '30', '--user-agent', 'curl/8', '--write-out', '\n%{http_code}',
            'https://updates.befeast.com/' + key], check=True, capture_output=True)
        body, status = result.stdout.rsplit(b'\n', 1)
        if status == b'404':
            return None
        if status != b'200':
            raise ValueError('Cannot read current beta feed: HTTP ' + status.decode())
        return body


def published_beta(source, platform):
    from release_notes import predecessor
    head = predecessor(PublicFeeds(), platform, 'beta')
    # A source catalog can describe archive-only work. Only the actual beta
    # feed proves this source reached users and suppresses the next hourly build.
    return {**head, 'platform': platform} if head and head['source'] == source else None


def api(path, method='GET', body=None):
    headers = {'Content-Type': 'application/json', 'User-Agent': 'okilum-hourly-beta'}
    # Public mirror producers need read access only; no Forgejo secret on GitHub.
    if os.environ.get('FORGEJO_TOKEN'):
        headers['Authorization'] = 'token ' + os.environ['FORGEJO_TOKEN']
    base = os.environ.get('FORGEJO_URL', 'https://git.oklabs.uk').rstrip('/')
    request = urllib.request.Request(base + '/api/v1/repos/' + REPOSITORY + path,
        data=json.dumps(body).encode() if body is not None else None,
        headers=headers, method=method)
    with urllib.request.urlopen(request, timeout=30) as response:
        payload = response.read()
    return json.loads(payload) if payload else None


def green(source):
    data = api('/commits/' + source + '/status?limit=100')
    if data['sha'] != source or data['total_count'] != len(data['statuses']):
        raise ValueError('Incomplete or mismatched main status inventory')
    for context in REQUIRED:
        matches = [s for s in data['statuses'] if s['context'] == context]
        if len(matches) != 1 or matches[0]['status'] != 'success':
            return False, f'Waiting for {context} success on newest main {source}'
    return True, 'Required post-merge fast checks succeeded'


def select(event, source, platform, env, descriptor):
    if env.get('GITHUB_REF') != 'refs/heads/main':
        raise ValueError('Hourly beta requires main')
    tip = api('/branches/main')['commit']['id']
    if not re.fullmatch('[0-9a-f]{40}', tip):
        raise ValueError('Invalid newest main identity')
    ready, reason = green(tip)
    if not ready:
        return False, source, reason + '; no fallback; retry next hour'
    # linux.json describes frozen pacman. AppImage is the active Linux beta.
    published = descriptor(tip, 'linux-appimage' if platform == 'linux' else platform)
    if published is not None:
        if published.get('source') != tip or published.get('platform') != (
                'linux-appimage' if platform == 'linux' else platform):
            raise ValueError('Invalid beta publication descriptor')
        return False, source, 'Newest green main already published; no beta work'
    expected = env.get('HOURLY_SOURCE', '')
    if expected and (expected != source or expected != tip):
        return False, source, 'Hourly dispatch superseded; retry newest main next hour'
    if source != tip:
        # Dispatch captures main anew. Its own selector revalidates expected SHA.
        # Thus native shipping statuses and artifact provenance stay on source.
        if event == 'schedule':
            api('/actions/workflows/' + WORKFLOWS[platform] + '/dispatches', 'POST',
                {'ref': 'main', 'inputs': {'hourly_source': tip}})
            return False, source, 'Dispatched current green main; stale tick builds nothing'
        return False, source, 'Manual snapshot superseded; dispatch main again'
    return True, source, 'Build newest green unpublished main ' + source


def github_main(platform, env, descriptor):
    """Read-only admission before a GitHub compiler runner is allocated."""
    if env['GITHUB_REF'] != 'refs/heads/main':
        return True, 'Trusted PR proxy retains its existing push contract'
    # Mirror lag is visible and cannot authorize compilation of an older source.
    source = env['GITHUB_SHA']
    if env.get('HOURLY_SOURCE') and env['HOURLY_SOURCE'] != source:
        return False, 'GitHub dispatch captured a different source; no build'
    tip = api('/branches/main')['commit']['id']
    if source != tip:
        return False, 'GitHub mirror snapshot is behind newest main; retry next hour'
    ready, reason = green(source)
    if not ready:
        return False, reason
    published = descriptor(source, 'linux-appimage' if platform == 'linux' else platform)
    if published is not None:
        if published.get('source') != source or published.get('platform') != (
                'linux-appimage' if platform == 'linux' else platform):
            raise ValueError('Invalid beta publication descriptor')
        return False, 'Newest main beta already published; no compiler work'
    if github_produced(platform, source, env):
        return False, 'Exact-source GitHub producer already completed; reuse its bytes'
    return True, 'Build current green mirror source'


def github_api(path, env):
    request = urllib.request.Request('https://api.github.com/repos/' + REPOSITORY + '/' + path,
        headers={'Authorization': 'Bearer ' + env['GH_TOKEN'],
                 'Accept': 'application/vnd.github+json', 'User-Agent': 'okilum-hourly-beta'})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def github_produced(platform, source, env):
    workflow = 'forgejo-linux.yml' if platform == 'linux' else 'forgejo-macos.yml'
    data = github_api(f'actions/workflows/{workflow}/runs?head_sha={source}&per_page=100', env)
    if data['total_count'] != len(data['workflow_runs']):
        raise ValueError('Incomplete GitHub producer inventory')
    job_name = 'appimage' if platform == 'linux' else 'macos'
    step_name = ('Verify AppImage package' if platform == 'linux'
                 else 'Compile Reader and run quick native regressions')
    for run in data['workflow_runs']:
        if str(run['id']) == env['GITHUB_RUN_ID']:
            continue
        if (run['head_sha'] != source or run['head_branch'] != 'main'
                or run['path'] != '.github/workflows/' + workflow
                or run['event'] not in ('push', 'schedule', 'workflow_dispatch')
                or run['run_attempt'] != 1 or run['conclusion'] == 'cancelled'):
            continue
        jobs = github_api(f"actions/runs/{run['id']}/attempts/1/jobs?per_page=100", env)
        if jobs['total_count'] != len(jobs['jobs']):
            raise ValueError('Incomplete GitHub job inventory')
        if any(job['name'] == job_name and job['conclusion'] == 'success'
               and job['head_sha'] == source and job['run_id'] == run['id']
               and any(step['name'] == step_name and step['conclusion'] == 'success'
                       for step in job['steps']) for job in jobs['jobs']):
            return True
    return False


def dispatch_linux(source):
    """Urgent manual Forgejo beta does not wait for the next GitHub cron."""
    import appimage_artifact
    client = appimage_artifact.transport.GitHub()
    if (not re.fullmatch('[0-9a-f]{40}', source)
            or client.request('git/ref/heads/main')['object']['sha'] != source):
        raise ValueError('GitHub mirror has not reached the requested green source')
    if github_produced('linux', source, {'GH_TOKEN': client.token, 'GITHUB_RUN_ID': ''}):
        print('Original AppImage producer already exists; reuse without dispatch')
        return
    client.request('actions/workflows/forgejo-linux.yml/dispatches', 'POST',
                   {'ref': 'main', 'inputs': {'source': source}})


if __name__ == '__main__':
    import sys
    if sys.argv[1] == 'dispatch-linux':
        dispatch_linux(os.environ['SOURCE'])
        sys.exit(0)
    build, reason = github_main(sys.argv[1], os.environ, published_beta)
    print(reason)
    with Path(os.environ['GITHUB_OUTPUT']).open('a') as output:
        output.write(f'build={str(build).lower()}\n')
