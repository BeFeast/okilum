"""Hourly beta admission shared by Forgejo and the public GitHub producers.

Only the current main tip is eligible. Never infer merge health from PR checks:
the hourly Linux selector dispatches fast's required suites on the merge commit.
"""
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import subprocess
import urllib.request

REPOSITORY = 'BeFeast/okilum'
WORKFLOWS = {'linux': 'linux-release.yml', 'macos': 'macos-release.yml',
             'windows': 'windows-diagnostic.yml'}
REQUIRED = ('fast / check (workflow_dispatch)', 'fast / shell-full (workflow_dispatch)')
SHIPPING_JOBS = {
    # Stable Linux uses AppImage; frozen/independent Arch cannot delay its gate.
    'linux': ('select', 'appimage-reuse', 'appimage-publication'),
    'macos': ('select', 'release', 'hosted-artifact'),
    'windows': ('select', 'release', 'resign', 'package', 'windows-release-github'),
}


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


def certify(source, now=None):
    """One automatic request per UTC hour, owned only by the Linux cron tick.

    Forgejo owns the dispatch history, including failed/cancelled capture races;
    no disposable runner cache is used as operational state. New main merges do
    not dispatch anything. The next tick supersedes an older running source.
    """
    now = now or datetime.now(timezone.utc)
    hour = now.replace(minute=0, second=0, microsecond=0)
    page, seen = 1, 0
    while True:
        data = api('/actions/runs?workflow_id=fast.yml&event=workflow_dispatch'
                   '&ref=refs%2Fheads%2Fmain&limit=100&page=' + str(page))
        runs = data['workflow_runs']
        if (not isinstance(runs, list) or type(data['total_count']) is not int
                or data['total_count'] < seen + len(runs)):
            raise ValueError('Incomplete fast certification inventory')
        older = False
        for run in runs:
            if (run['workflow_id'] != 'fast.yml' or run['prettyref'] != 'main'
                    or run['trigger_event'] != 'workflow_dispatch' or run['is_fork_pull_request']):
                raise ValueError('Unexpected fast certification identity')
            created = datetime.fromisoformat(run['created'].replace('Z', '+00:00'))
            payload = run['event_payload']
            if isinstance(payload, str):
                payload = json.loads(payload)
            requested = payload.get('inputs', {}).get('certification_source', '')
            if requested:
                if created >= hour:
                    return 'Certification already requested this UTC hour; retry next hour'
                if requested == source and run['status'] in ('waiting', 'running', 'pending'):
                    return 'Newest main certification is still pending; retry next hour'
            older |= created < hour
        seen += len(runs)
        # Runs are newest first. Inspect a whole page so a still-running source
        # from the preceding hour is not needlessly restarted.
        if older or seen == data['total_count']:
            break
        if not runs:
            raise ValueError('Incomplete fast certification inventory')
        page += 1
    # Main may have moved while inventory was read. Do not certify an old tip.
    if api('/branches/main')['commit']['id'] != source:
        return 'Main advanced before certification dispatch; retry newest main next hour'
    # This Forgejo does not emit native statuses for workflow_dispatch runs.
    # Pending also makes a failed dispatch fail closed until the next tick.
    for context in REQUIRED:
        api('/statuses/' + source, 'POST', {'context': context, 'state': 'pending',
            'description': 'Hourly main certification queued'})
    api('/actions/workflows/fast.yml/dispatches', 'POST',
        {'ref': 'main', 'inputs': {'certification_source': source}})
    return 'Dispatched fast certification for newest main ' + source + '; retry next hour'


def certification_source(env):
    """The run and checkout must both capture the requested main commit."""
    source = env['CERTIFICATION_SOURCE']
    if (not re.fullmatch('[0-9a-f]{40}', source)
            or env['GITHUB_EVENT_NAME'] != 'workflow_dispatch'
            or env['GITHUB_REF'] != 'refs/heads/main'
            or env['GITHUB_SHA'] != source
            or api('/branches/main')['commit']['id'] != source):
        raise ValueError('Certification source superseded or mismatched; no compilation')
    print('Certifying exact newest main ' + source)


def report_statuses(source, contexts, env):
    """Explicit dispatch/schedule statuses, with the original shipping context names."""
    if not re.fullmatch('[0-9a-f]{40}', source):
        raise ValueError('Invalid status source')
    target = (env['GITHUB_SERVER_URL'].rstrip('/') + '/' + REPOSITORY
              + '/actions/runs/' + env['GITHUB_RUN_NUMBER'])
    for context, result in contexts:
        if result not in ('success', 'failure', 'cancelled', 'skipped'):
            raise ValueError('Unexpected job result')
        api('/statuses/' + source, 'POST', {'context': context,
            'state': 'failure' if result == 'cancelled' else result,
            'description': 'Hourly job ' + result, 'target_url': target})


def report_certification(env):
    source = env['CERTIFICATION_SOURCE']
    results = json.loads(env['JOB_RESULTS'])
    if set(results) != {'check', 'shell-full'}:
        raise ValueError('Incomplete certification job results')
    # A branch-capture race must fail the requested source, never certify the
    # newer commit the dispatch happened to capture. Superseded successes on old
    # main are harmless: admission always checks the newest main SHA.
    matched = (env['GITHUB_SHA'] == source and env['GITHUB_REF'] == 'refs/heads/main'
               and env['GITHUB_EVENT_NAME'] == 'workflow_dispatch')
    report_statuses(source, [(context, results[job]['result'] if matched else 'failure')
        for context, job in zip(REQUIRED, ('check', 'shell-full'))], env)


def report_shipping(platform, env):
    source = env['SOURCE']
    event = env['GITHUB_EVENT_NAME']
    results = json.loads(env['JOB_RESULTS'])
    if (source != env['GITHUB_SHA'] or env['GITHUB_REF'] != 'refs/heads/main'
            or event not in ('schedule', 'workflow_dispatch')
            or set(results) != set(SHIPPING_JOBS[platform])):
        raise ValueError('Shipping statuses require the exact beta source run')
    report_statuses(source, [(f'{platform}-release / {job} ({event})', results[job]['result'])
                             for job in SHIPPING_JOBS[platform]], env)


def select(event, source, platform, env, descriptor):
    if env.get('GITHUB_REF') != 'refs/heads/main':
        raise ValueError('Hourly beta requires main')
    tip = api('/branches/main')['commit']['id']
    if not re.fullmatch('[0-9a-f]{40}', tip):
        raise ValueError('Invalid newest main identity')
    ready, reason = green(tip)
    if not ready:
        # One scheduled coordinator owns certification for every platform.
        # Source-pinned retries, manual dispatches and GitHub readers never fan out.
        if platform == 'linux' and event == 'schedule':
            reason += '; ' + certify(tip)
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
    import appimage_artifact
    transport = appimage_artifact.transport
    # Reuse the existing uncached Actions transport and quota/error handling.
    # The public selector has read-only workflow credentials, even if the caller
    # also has App configuration in its environment (the urgent Forgejo path).
    credentials = transport.auth.Credentials(env={'MIRROR_TOKEN': env['GH_TOKEN']})
    return transport.GitHub(credentials).request(path)


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
    if sys.argv[1] == 'certify-source':
        certification_source(os.environ)
        sys.exit(0)
    if sys.argv[1] == 'certification-status':
        report_certification(os.environ)
        sys.exit(0)
    if sys.argv[1] == 'shipping-status':
        report_shipping(sys.argv[2], os.environ)
        sys.exit(0)
    if sys.argv[1] == 'dispatch-linux':
        dispatch_linux(os.environ['SOURCE'])
        sys.exit(0)
    build, reason = github_main(sys.argv[1], os.environ, published_beta)
    print(reason)
    with Path(os.environ['GITHUB_OUTPUT']).open('a') as output:
        output.write(f'build={str(build).lower()}\n')
