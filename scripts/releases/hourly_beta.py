"""Beta admission shared by the Forgejo and public GitHub producers.

Oleg 05.11: every push to main starts each platform's beta build from that
exact commit. Manager 13.11: a newer push never cancels a running build; it
queues, older queued push runs are cancelled when one starts, and a stale one
that starts anyway skips. Beta has no certification gate: merges are gated by
the worker's exact-HEAD check, and the platform compile/package/sign run is
the beta gate. Fast certification stays source-pinned and on demand; it gates
Stable only (`fast / shell-full` on the exact candidate).
(The module keeps its historical name; nothing here runs hourly any more.)
"""
import json
import os
from pathlib import Path
import re
import subprocess
import time
import urllib.request

REPOSITORY = 'BeFeast/okilum'
WORKFLOWS = {'linux': 'linux-release.yml', 'macos': 'macos-release.yml',
             'windows': 'windows-diagnostic.yml'}
# Source-pinned certification contexts. Stable requires shell-full on its exact
# candidate; Beta never reads either of them.
REQUIRED = ('fast / check (workflow_dispatch)', 'fast / shell-full (workflow_dispatch)')
CERTIFICATION_JOBS = dict(zip(('check', 'shell-full'), REQUIRED))
# The Forgejo push and the GitHub mirror of that push start together, and the
# mirror shares the capacity-1 publish runner with release selection.
MIRROR_WAIT_SECONDS = 600
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
    # feed proves this source reached users and suppresses a duplicate build.
    return {**head, 'platform': platform} if head else None


def feed_platform(platform):
    """Linux beta is the AppImage feed; frozen pacman never suppresses or admits it."""
    return 'linux-appimage' if platform == 'linux' else platform


def api(path, method='GET', body=None, timeout=30):
    headers = {'Content-Type': 'application/json', 'User-Agent': 'okilum-hourly-beta'}
    # Public mirror producers need read access only; no Forgejo secret on GitHub.
    if os.environ.get('FORGEJO_TOKEN'):
        headers['Authorization'] = 'token ' + os.environ['FORGEJO_TOKEN']
    base = os.environ.get('FORGEJO_URL', 'https://git.oklabs.uk').rstrip('/')
    request = urllib.request.Request(base + '/api/v1/repos/' + REPOSITORY + path,
        data=json.dumps(body).encode() if body is not None else None,
        headers=headers, method=method)
    with urllib.request.urlopen(request, timeout=timeout) as response:
        payload = response.read()
    return json.loads(payload) if payload else None


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
    """Main's tip, unless it already is this platform's beta.

    The published beta must be a first-parent ancestor of the tip, so a feed
    that is ahead of or off main fails closed instead of being replaced.
    """
    published_platform = feed_platform(platform)
    published = descriptor(tip, published_platform)
    floor = published['source'] if published else None
    if published and (published.get('platform') != published_platform
                      or not re.fullmatch('[0-9a-f]{40}', floor)):
        raise ValueError('Invalid beta publication descriptor')
    if tip == floor:
        return None
    if floor and floor not in main_history(tip):
        raise ValueError('Published beta is outside first-parent main history')
    return tip if not expected or expected == tip else None


SUPERSEDE_DEPTH = 200
# Wall-clock budget for the push superseded check, well inside the 3-minute
# select job (review of #2890): slow but successful responses must not exhaust it.
SUPERSEDE_BUDGET_SECONDS = 60
SUPERSEDE_REQUEST_SECONDS = 10


def budget_api(deadline, path, method='GET', body=None):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError('superseded check budget exhausted')
    result = api(path, method, body, timeout=min(remaining, SUPERSEDE_REQUEST_SECONDS))
    if time.monotonic() > deadline:
        raise TimeoutError('superseded check budget exhausted')
    return result


def superseded_by(tip, source, deadline):
    """Whether source is among the last SUPERSEDE_DEPTH first-parent commits of tip."""
    commit = tip
    for _ in range(SUPERSEDE_DEPTH):
        if commit == source:
            return True
        if not commit:
            return False
        data = budget_api(deadline, '/git/commits/' + commit)
        if data['sha'] != commit:
            raise ValueError('Mismatched main history')
        commit = data['parents'][0]['sha'] if data['parents'] else None
    return False


def coalesce_queued_pushes(platform, env, deadline):
    """Cancel older queued push runs of this platform's beta, keep the newest.

    Forgejo 17 queues a cancel-in-progress: false group FIFO and never replaces a
    pending run (models/actions/task.go getConcurrencyCondition), so a burst of
    merges piles up checkout/select jobs. The run that starts cancels every
    waiting push run except the newest; running runs are never touched. Best
    effort: a failure here never changes this run's own decision.
    """
    workflow = WORKFLOWS[platform]
    data = budget_api(deadline, '/actions/runs?workflow_id=' + workflow
                      + '&event=push&status=waiting&limit=50&page=1')
    queued = [run for run in data.get('workflow_runs') or []
              if run.get('workflow_id') == workflow and run.get('trigger_event') == 'push'
              and run.get('status') == 'waiting' and run.get('prettyref') == 'main'
              and type(run.get('id')) is int and str(run['id']) != env.get('GITHUB_RUN_ID')]
    if len(queued) < 2:
        return []
    newest = max(queued, key=lambda run: run['id'])
    cancelled = []
    for run in sorted(queued, key=lambda run: run['id']):
        if run is not newest:
            budget_api(deadline, f"/actions/runs/{run['id']}/cancel", 'POST')
            cancelled.append(run['id'])
    return cancelled


def push_select(source, platform, env, descriptor):
    """A main push builds exactly the pushed commit (Oleg 05.11).

    No certification. A running beta is never cancelled by a newer push
    (manager 13.11: merges kept restarting the Linux beta). A push run that
    starts after main has moved on skips: the newer push's own queued run
    builds the tip. It also skips a source that already is this platform's
    beta. If main's tip cannot be read, it builds as before rather than skip.
    Publishers still refuse a build older than the feed under their lock.
    """
    if env.get('GITHUB_REF') != 'refs/heads/main' or not re.fullmatch('[0-9a-f]{40}', source or ''):
        raise ValueError('Push beta requires an exact main commit')
    published_platform = feed_platform(platform)
    published = descriptor(source, published_platform)
    if published and published.get('platform') != published_platform:
        raise ValueError('Invalid beta publication descriptor')
    if published and published.get('source') == source:
        return False, source, f'Pushed main {source} already is the {platform} beta'
    deadline = time.monotonic() + SUPERSEDE_BUDGET_SECONDS
    try:
        cancelled = coalesce_queued_pushes(platform, env, deadline)
        if cancelled:
            print(f'Cancelled older queued {platform} push runs: {cancelled}')
    except Exception as error:  # coalescing is an optimisation, never a gate
        print(f'Queued push runs left as they are ({type(error).__name__})')
    # Bounded so a deep queue or a slow API cannot exhaust the 3-minute select
    # job (review of PR 2890): any lookup failure, an exhausted time budget, or a
    # source not among the last SUPERSEDE_DEPTH first-parent commits builds
    # rather than skips (a foreign or out-of-window source builds, it does not
    # fail closed; publishers still refuse a build older than the feed).
    try:
        tip = budget_api(deadline, '/branches/main')['commit']['id']
        if not re.fullmatch('[0-9a-f]{40}', tip or ''):
            return True, source, f'Build pushed main {source} (main tip invalid)'
        if tip == source:
            return True, source, f'Build pushed main {source}'
        superseded = superseded_by(tip, source, deadline)
    except Exception as error:  # an unreadable tip or history must not skip a beta
        return True, source, f'Build pushed main {source} (main history unreadable: {type(error).__name__})'
    if superseded:
        return False, source, f'Pushed main {source} is superseded by main {tip}; that push builds it'
    return True, source, f'Build pushed main {source} (not within {SUPERSEDE_DEPTH} commits of main {tip})'


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
            'description': 'Job ' + result, 'target_url': target})


def report_certification(env):
    """Record a source-pinned certification result for the Stable selector.

    It never starts beta work: every main push already builds its own beta,
    and a certification dispatch would only duplicate or resurrect one.
    """
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


def report_shipping(platform, env):
    source = env['SOURCE']
    event = env['GITHUB_EVENT_NAME']
    results = json.loads(env['JOB_RESULTS'])
    # Push runs carry native Forgejo statuses with the same context names
    # (`<platform>-release / <job> (push)`); only dispatches need reporting.
    if (env['GITHUB_REF'] != 'refs/heads/main'
            or event != 'workflow_dispatch'
            or set(results) != set(SHIPPING_JOBS[platform])):
        raise ValueError('Shipping statuses require the exact beta source run')
    main_source(source)
    report_statuses(source, [(f'{platform}-release / {job} ({event})', results[job]['result'])
                             for job in SHIPPING_JOBS[platform]], env)


def select(event, source, platform, env, descriptor):
    """Manual main dispatch (recovery or urgent rebuild): newest main, no certification."""
    if env.get('GITHUB_REF') != 'refs/heads/main':
        raise ValueError('Manual beta requires main')
    tip = api('/branches/main')['commit']['id']
    if not re.fullmatch('[0-9a-f]{40}', tip):
        raise ValueError('Invalid newest main identity')
    pinned = env.get('HOURLY_SOURCE', '')
    selected = candidate(tip, platform, descriptor, pinned)
    if selected is None:
        return False, source, (f'Newest main {tip} superseded {pinned}' if pinned and pinned != tip
                               else f'Newest main {tip} is already the beta')
    # Compare order from the current tip. Walking backwards from an older
    # product to find a newer workflow would needlessly scan the entire repo.
    newer_than_workflow = (selected != source and next(
        commit for commit in main_history(tip) if commit in (source, selected)) == selected)
    if (event == 'schedule' or (selected != source and not pinned)
            or newer_than_workflow):
        api('/actions/workflows/' + WORKFLOWS[platform] + '/dispatches', 'POST',
            {'ref': 'main', 'inputs': {'hourly_source': selected}})
        return False, source, f'Dispatched newest main {selected} with current delivery tooling'
    return True, selected, f'Build newest main {selected}'


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
        # it does so after main_source() on the exact source. The Forgejo
        # publisher re-admits the bytes against the feed under its lock.
        published = descriptor(source, feed_platform(platform))
        if published is not None and published.get('source') == source:
            return False, 'Pinned source is already the beta head; no compiler work'
    else:
        try:
            tip = api('/branches/main')['commit']['id']
        except OSError as error:
            return False, f'Forgejo admission is unreachable from GitHub ({error}); no build'
        if candidate(tip, platform, descriptor, expected) != source:
            return False, 'Source is not the unpublished main tip; no compiler work'
    if github_produced(platform, source, env):
        return False, 'Exact-source GitHub producer already completed; reuse its bytes'
    return True, 'Build exact mirror source'


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


def wait_for_mirror(client, source, *, wait=MIRROR_WAIT_SECONDS, clock=time.monotonic, sleep=time.sleep):
    """A push-started beta can reach this point before github-mirror pushed main.

    A GitHub ref can only point at a commit GitHub already has. Poll the exact
    commit, bounded; quota exhaustion and a mismatched object still fail at once.
    """
    import appimage_artifact
    transport = appimage_artifact.transport
    deadline = clock() + wait
    while True:
        try:
            commit = client.request('git/commits/' + source)
        except transport.RateLimitExhausted:
            raise
        except transport.Unavailable:
            if clock() >= deadline:
                raise ValueError('GitHub mirror has not received the pushed source; no AppImage producer')
            sleep(15)
            continue
        if not isinstance(commit, dict) or commit.get('sha') != source:
            raise ValueError('GitHub mirror returned a different commit identity')
        return


def dispatch_linux(source):
    """Start the exact-source GitHub AppImage producer for a pushed or manual beta."""
    import appimage_artifact
    client = appimage_artifact.transport.GitHub()
    main_source(source)
    if github_produced('linux', source, {'GH_TOKEN': client.token, 'GITHUB_RUN_ID': ''}, pending=True):
        print('Original AppImage producer already exists; reuse without dispatch')
        return
    # GitHub dispatch captures a mutable ref. A write-once source branch keeps
    # run/job/artifact head_sha equal to the product SHA after main advances.
    branch = 'forgejo-linux-pr/hourly-' + source
    refs = client.request('git/matching-refs/heads/' + branch)
    exact = [ref for ref in refs if ref['ref'] == 'refs/heads/' + branch]
    if not exact:
        wait_for_mirror(client, source)
        # The push trigger starts the exact-source producer immediately. Its
        # concurrency group cancels an unfinished producer of an older source.
        client.request('git/refs', 'POST', {'ref': 'refs/heads/' + branch, 'sha': source})
        return
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
