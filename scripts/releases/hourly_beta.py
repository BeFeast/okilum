"""Hourly beta admission shared by Forgejo and the public GitHub producers.

Admission follows certified main history, independently of a moving branch tip.
Never infer merge health from PR checks: certify the exact product checkout.
NEWEST_MAIN_BETA platforms skip certification and always take main's tip.
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
# Beta gates on the shipping check only. shell-full is the full runtime suite:
# it is still certified and reported on the same source, but it informs Stable
# and never holds Beta (03.11 rule: CI never gates development).
BETA_REQUIRED = REQUIRED[:1]
# Oleg 04.11: these betas build the newest first-parent main with no
# certification gate; a newer merge supersedes an older unfinished one. The
# hosted compile/sign/notarize run is the gate. Fast certification still runs
# and informs Stable. (#2589: an old certified source shipped hours after main.)
NEWEST_MAIN_BETA = frozenset({'macos'})
CERTIFICATION_JOBS = dict(zip(('check', 'shell-full'), REQUIRED))
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
    return {**head, 'platform': platform} if head else None


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
    for context in BETA_REQUIRED:
        matches = [s for s in data['statuses'] if s['context'] == context]
        if len(matches) != 1 or matches[0]['status'] != 'success':
            return False, f'Waiting for {context} success on main source {source}'
    return True, 'Required post-merge fast checks succeeded'


def beta_ready(source, platform):
    """Whether this platform's beta may ship the source (newest-main lanes always may)."""
    return platform in NEWEST_MAIN_BETA or green(source)[0]


def main_history(tip):
    """Walk first parents only: green PR branch heads are never beta candidates."""
    seen = set()
    while tip:
        if not re.fullmatch('[0-9a-f]{40}', tip) or tip in seen or len(seen) >= 2000:
            raise ValueError('Invalid or excessive main history')
        seen.add(tip)
        yield tip
        commit = api('/git/commits/' + tip)
        if commit['sha'] != tip:
            raise ValueError('Mismatched main history')
        tip = commit['parents'][0]['sha'] if commit['parents'] else None


def main_source(source, tip=None):
    tip = tip or api('/branches/main')['commit']['id']
    if source not in main_history(tip):
        raise ValueError('Source is not a first-parent main commit')


def candidate(tip, platform, descriptor, expected=''):
    published_platform = 'linux-appimage' if platform == 'linux' else platform
    published = descriptor(tip, published_platform)
    floor = published['source'] if published else None
    if published and (published.get('platform') != published_platform
                      or not re.fullmatch('[0-9a-f]{40}', floor)):
        raise ValueError('Invalid beta publication descriptor')
    selected = None
    admitted = lambda: selected if not expected or expected == selected else None
    for source in main_history(tip):
        if source == floor:
            return admitted()
        if selected is None and beta_ready(source, platform):
            selected = source
            if floor is None:
                return admitted()
    if floor:
        raise ValueError('Published beta is outside first-parent main history')
    return admitted()


def certify(source, now=None):
    """One automatic request per UTC hour, owned only by the Linux cron tick.

    Forgejo owns the dispatch history, including failed/cancelled capture races;
    no disposable runner cache is used as operational state. New main merges do
    not dispatch anything. Advancing main never invalidates an older source.
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
    # This Forgejo does not emit native statuses for workflow_dispatch runs.
    # Pending also makes a failed dispatch fail closed until the next tick.
    for context in REQUIRED:
        api('/statuses/' + source, 'POST', {'context': context, 'state': 'pending',
            'description': 'Hourly main certification queued'})
    api('/actions/workflows/fast.yml/dispatches', 'POST',
        {'ref': 'main', 'inputs': {'certification_source': source}})
    return 'Dispatched fast certification for newest main ' + source + '; retry next hour'


def certification_source(env):
    """Use reviewed workflow tooling to pin and validate the product checkout."""
    source = env['CERTIFICATION_SOURCE']
    if (not re.fullmatch('[0-9a-f]{40}', source)
            or env['GITHUB_EVENT_NAME'] != 'workflow_dispatch'
            or env['GITHUB_REF'] != 'refs/heads/main'):
        raise ValueError('Certification source or event mismatched; no compilation')
    main_source(source)
    checkout = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    if checkout != env['GITHUB_SHA']:
        raise ValueError('Certification workflow checkout mismatched')
    # Keep this validator from the workflow revision even when certifying a
    # pre-fix product source. Both compiler jobs execute the exact product tree.
    subprocess.run(['git', 'fetch', '--no-tags', '--depth=1', 'origin', source], check=True)
    subprocess.run(['git', 'checkout', '--detach', source], check=True)
    if subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip() != source:
        raise ValueError('Certification product checkout mismatched')
    print('Certifying exact main source ' + source)


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
    # check reports as soon as it finishes; shell-full reports separately later.
    if set(results) not in ({'check'}, {'shell-full'}):
        raise ValueError('Incomplete certification job results')
    if (env['GITHUB_REF'] != 'refs/heads/main'
            or env['GITHUB_EVENT_NAME'] != 'workflow_dispatch'):
        raise ValueError('Certification requires a trusted main dispatch')
    main_source(source)
    report_statuses(source, [(CERTIFICATION_JOBS[job], result['result'])
        for job, result in results.items()], env)
    if results.get('check', {}).get('result') == 'success':
        # Publication is independently re-admitted against the current feed.
        # A failed dispatch is recovered by the next ordinary platform tick.
        for workflow in WORKFLOWS.values():
            api('/actions/workflows/' + workflow + '/dispatches', 'POST',
                {'ref': 'main', 'inputs': {'hourly_source': source}})


def report_shipping(platform, env):
    source = env['SOURCE']
    event = env['GITHUB_EVENT_NAME']
    results = json.loads(env['JOB_RESULTS'])
    if (env['GITHUB_REF'] != 'refs/heads/main'
            or event not in ('schedule', 'workflow_dispatch')
            or set(results) != set(SHIPPING_JOBS[platform])):
        raise ValueError('Shipping statuses require the exact beta source run')
    main_source(source)
    report_statuses(source, [(f'{platform}-release / {job} ({event})', results[job]['result'])
                             for job in SHIPPING_JOBS[platform]], env)


def select(event, source, platform, env, descriptor):
    if env.get('GITHUB_REF') != 'refs/heads/main':
        raise ValueError('Hourly beta requires main')
    tip = api('/branches/main')['commit']['id']
    if not re.fullmatch('[0-9a-f]{40}', tip):
        raise ValueError('Invalid newest main identity')
    pinned = env.get('HOURLY_SOURCE', '')
    if platform in NEWEST_MAIN_BETA:
        label = 'newest main'
        reason = (f'Newest main {tip} superseded {pinned}' if pinned and pinned != tip
                  else f'Newest main {tip} is already the beta')
    else:
        label = 'certified unpublished main'
        ready, reason = green(tip)
        if not ready and platform == 'linux' and event == 'schedule':
            reason += '; ' + certify(tip)
        reason += '; no unpublished certified main source'
    selected = candidate(tip, platform, descriptor, pinned)
    if selected is None:
        return False, source, reason + '; retry next hour'
    # Compare order from the current tip. Walking backwards from an older
    # product to find a newer workflow would needlessly scan the entire repo.
    newer_than_workflow = (selected != source and next(
        commit for commit in main_history(tip) if commit in (source, selected)) == selected)
    if (event == 'schedule' or (selected != source and not pinned)
            or newer_than_workflow):
        api('/actions/workflows/' + WORKFLOWS[platform] + '/dispatches', 'POST',
            {'ref': 'main', 'inputs': {'hourly_source': selected}})
        return False, source, f'Dispatched {label} {selected} with current delivery tooling'
    return True, selected, f'Build {label} {selected}'


def github_main(platform, env, descriptor):
    """Read-only admission before a GitHub compiler runner is allocated."""
    source = env['GITHUB_SHA']
    pinned_ref = 'refs/heads/forgejo-linux-pr/hourly-' + source
    if env['GITHUB_REF'].startswith('refs/heads/forgejo-linux-pr/hourly-') and env['GITHUB_REF'] != pinned_ref:
        return False, 'Pinned hourly ref does not match its source'
    if env['GITHUB_REF'] not in ('refs/heads/main', pinned_ref):
        return True, 'Trusted PR proxy retains its existing push contract'
    expected = env.get('HOURLY_SOURCE', '') or source
    if expected != source:
        return False, 'GitHub dispatch captured a different source; no build'
    if env['GITHUB_REF'] == pinned_ref:
        # Forgejo is LAN-only: GitHub-hosted runners cannot read its API. Only
        # Forgejo's dispatch_linux creates or dispatches this write-once ref, and
        # it does so after main_source() and green() on the exact source. The
        # Forgejo publisher re-admits the bytes against the feed under its lock.
        published = descriptor(source, 'linux-appimage' if platform == 'linux' else platform)
        if published is not None and published.get('source') == source:
            return False, 'Pinned source is already the beta head; no compiler work'
    else:
        try:
            tip = api('/branches/main')['commit']['id']
        except OSError as error:
            return False, f'Forgejo admission is unreachable from GitHub ({error}); no build'
        if candidate(tip, platform, descriptor, expected) != source:
            return False, 'Source is uncertified or already published; no compiler work'
    if github_produced(platform, source, env):
        return False, 'Exact-source GitHub producer already completed; reuse its bytes'
    return True, 'Build certified mirror source'


def github_api(path, env):
    import appimage_artifact
    transport = appimage_artifact.transport
    # Reuse the existing uncached Actions transport and quota/error handling.
    # The public selector has read-only workflow credentials, even if the caller
    # also has App configuration in its environment (the urgent Forgejo path).
    credentials = transport.auth.Credentials(env={'MIRROR_TOKEN': env['GH_TOKEN']})
    return transport.GitHub(credentials).request(path)


def github_produced(platform, source, env, *, pending=False):
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
        if (run['head_sha'] != source or run['head_branch'] not in ('main', 'forgejo-linux-pr/hourly-' + source)
                or run['path'] != '.github/workflows/' + workflow
                or run['event'] not in ('push', 'schedule', 'workflow_dispatch')
                or run['run_attempt'] != 1 or run['conclusion'] == 'cancelled'):
            continue
        if pending and run.get('status') in ('queued', 'requested', 'waiting', 'pending'):
            return True
        jobs = github_api(f"actions/runs/{run['id']}/attempts/1/jobs?per_page=100", env)
        if jobs['total_count'] != len(jobs['jobs']):
            raise ValueError('Incomplete GitHub job inventory')
        if pending and any(job['name'] == job_name and job.get('status') in (
                'queued', 'in_progress', 'waiting', 'pending') for job in jobs['jobs']):
            return True
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
    main_source(source)
    if not green(source)[0]:
        raise ValueError('AppImage dispatch requires a certified green main source')
    if github_produced('linux', source, {'GH_TOKEN': client.token, 'GITHUB_RUN_ID': ''}, pending=True):
        print('Original AppImage producer already exists; reuse without dispatch')
        return
    # GitHub dispatch captures a mutable ref. A write-once source branch keeps
    # run/job/artifact head_sha equal to the product SHA after main advances.
    branch = 'forgejo-linux-pr/hourly-' + source
    refs = client.request('git/matching-refs/heads/' + branch)
    exact = [ref for ref in refs if ref['ref'] == 'refs/heads/' + branch]
    if not exact:
        client.request('git/refs', 'POST', {'ref': 'refs/heads/' + branch, 'sha': source})
        return  # The push trigger starts the exact-source producer immediately.
    if len(exact) != 1 or exact[0]['object']['sha'] != source:
        raise ValueError('Pinned hourly producer ref has changed')
    client.request('actions/workflows/forgejo-linux.yml/dispatches', 'POST',
                   {'ref': branch, 'inputs': {'source': source}})


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
