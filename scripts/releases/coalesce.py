#!/usr/bin/env python3
"""Select periodic release work without occupying a compiler runner while idle."""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import subprocess
import urllib.request
from urllib.parse import quote


def needed(event, published, source, platform):
    if event == 'push':
        # Defensive no-op: secondary release workflows have no push trigger.
        return False
    if event in ('workflow_dispatch', 'pull_request'):
        return True
    if event != 'schedule':
        raise ValueError('Unexpected release event')
    if published is None:
        return True
    if published['source'] != source or published['platform'] != platform:
        raise ValueError('Invalid publication descriptor')
    return False


def main_tip():
    result = subprocess.run(['git', 'ls-remote', '--exit-code', 'origin',
                             'refs/heads/main'], check=True, capture_output=True,
                            text=True, timeout=15)
    fields = result.stdout.split()
    if (len(fields) != 2 or fields[1] != 'refs/heads/main'
            or len(fields[0]) != 40
            or any(c not in '0123456789abcdef' for c in fields[0])):
        raise ValueError('Cannot identify current main for scheduled build')
    return fields[0]


def scheduled_source(event, source, platform):
    """The commit an automatic Windows/Linux tick builds: main as it is now.

    A tick carries the main snapshot from when it fired. Merges land faster than the
    tick reaches its runner, so skipping an outdated snapshot starved beta for hours;
    the tick builds the current main tip instead (unless that is already published).
    """
    if event != 'schedule' or platform not in ('windows', 'linux', 'macos'):
        return source
    return main_tip()


# Temporary owner-approved M4 beta window (#1476); no operational revert needed.
MACOS_BETA_UNTIL = datetime(2026, 10, 26, 3, tzinfo=timezone.utc)


def macos_slot(now, cron=''):
    if now < MACOS_BETA_UNTIL:
        return 'frequent'
    # Forgejo versions without event.schedule expose only the tick time. Accept
    # the original daily slot, including the overlapping ten-minute tick.
    if cron == '30 10 * * *' or (not cron and now.hour == 10 and 30 <= now.minute < 40):
        return 'daily'
    return None


def macos_api(method, path, body=None):
    # Same existing workflow GITHUB_TOKEN and dispatch API as publication.py.
    # No retries, new credentials, runner state or host probes.
    base = os.environ.get('FORGEJO_URL', 'https://git.oklabs.uk').rstrip('/')
    repo = os.environ['GITHUB_REPOSITORY']
    if repo != 'BeFeast/okilum':
        raise ValueError('Automatic Mac beta requires the product repository')
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(base + '/api/v1/repos/' + repo + path,
                data=data, method=method, headers={
                    'Authorization': 'token ' + os.environ['FORGEJO_TOKEN'],
                    'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
        payload = response.read()
    return json.loads(payload) if payload else None


def macos_attempted(source, current_run, before_current=False, ref='main'):
    # One automatic attempt per main SHA: any prior main dispatch (including a
    # failed build or a successful build awaiting publication) backs off until
    # the next SHA. Never repeat notarization while publication is still pending.
    data = macos_api('GET', '/actions/runs?workflow_id=macos-release.yml'
                     '&ref=' + quote('refs/heads/' + ref, safe='') + '&head_sha=' + source +
                     '&limit=50&page=1')
    runs = data['workflow_runs']
    if (not isinstance(runs, list) or type(data['total_count']) is not int
            or data['total_count'] != len(runs) or len(runs) > 50):
        raise ValueError('Incomplete Mac attempt inventory')
    for run in runs:
        if (run['workflow_id'] != 'macos-release.yml' or run['commit_sha'] != source
                or run['prettyref'] != ref or run['is_fork_pull_request']
                or run['trigger_event'] not in ('workflow_dispatch', 'schedule')):
            raise ValueError('Unexpected Mac attempt identity')
        if type(run['id']) is not int or run['id'] < 1 or not current_run.isdigit():
            raise ValueError('Invalid Mac run identity')
        if ref != 'main':
            payload = run['event_payload']
            if isinstance(payload, str):
                payload = json.loads(payload)
            # Dispatch resolves a mutable branch. A run that captured a newer
            # commit than requested exits without signing and is not an attempt
            # for that new head, which must remain eligible when its CI is green.
            if payload.get('inputs', {}).get('preview_head') != source:
                continue
        # The oldest dispatched run owns this SHA. A newer queued duplicate must
        # not make the first run back off too (otherwise neither would build).
        if before_current and run['id'] >= int(current_run):
            continue
        if (str(run['id']) != current_run
                and (run['trigger_event'] == 'workflow_dispatch' or run['status'] == 'failure')):
            return True
    return False


def select_macos_preview(event, source, env, now):
    """Only the current, green, non-draft same-repo PR head can enter the signer.

    The coordinator dispatches the PR branch, so the build's workflow SHA stays
    equal to the artifact source. Revalidate once the serialized build starts:
    queued stale heads exit before reserving the M4, without cancelling notary work.
    """
    skip = lambda reason: (False, source, False, reason)
    if now >= MACOS_BETA_UNTIL:
        return skip('Mac PR preview window expired; request a build explicitly')
    number = env.get('MACOS_PREVIEW_PR', '')
    if (event not in ('pull_request', 'workflow_dispatch')
            or not re.fullmatch('[1-9][0-9]*', number)):
        raise ValueError('Invalid Mac preview request')
    repo = env['GITHUB_REPOSITORY']
    if repo != 'BeFeast/okilum':
        raise ValueError('Mac previews require the product repository')
    pr = macos_api('GET', '/pulls/' + number)
    if (pr['state'] != 'open' or pr['merged'] or pr['draft']
            or (pr['head']['repo'] or {}).get('full_name') != repo
            or pr['base']['repo']['full_name'] != repo or pr['base']['ref'] != 'main'):
        return skip('Mac PR preview skipped: closed, draft or untrusted PR')
    head, ref = pr['head']['sha'], pr['head']['ref']
    if (not re.fullmatch('[0-9a-f]{40}', head) or not ref or ref == 'main'):
        raise ValueError('Invalid Mac preview source identity')
    expected = env.get('MACOS_PREVIEW_HEAD')
    if head != expected or (event == 'workflow_dispatch' and source != expected) or (event == 'workflow_dispatch'
                            and env.get('GITHUB_REF') != 'refs/heads/' + ref):
        return skip('Mac PR preview superseded: newest PR head wins')
    source = head
    status = macos_api('GET', '/commits/' + head + '/status')
    contexts = [s for s in status['statuses'] if s['context'] == 'ci / macos (pull_request)']
    if status['sha'] != head or len(contexts) != 1 or contexts[0]['status'] != 'success':
        return skip('Mac PR preview waiting for exact-head macOS success')
    if not needed('schedule', descriptor(head, 'macos'), head, 'macos'):
        return skip('Mac PR head already published')
    if macos_attempted(head, env['GITHUB_RUN_ID'],
                       before_current=event == 'workflow_dispatch', ref=ref):
        return skip('Mac PR preview backoff: head already attempted')
    if event == 'pull_request':
        macos_api('POST', '/actions/workflows/macos-release.yml/dispatches',
                  {'ref': ref, 'inputs': {'preview_pr': number, 'preview_head': head}})
        return skip('Dispatched signed Mac PR preview; coordinator does not use macos runner')
    return True, source, True, 'Build signed Mac PR preview #' + number + ' · ' + head[:8]


def select_macos(event, source, env, now):
    if env.get('MACOS_PREVIEW_PR'):
        return select_macos_preview(event, source, env, now)
    tick = env.get('MACOS_BETA_TICK', '')
    if event != 'schedule' and not tick:
        return needed(event, None, source, 'macos'), source, False, 'Manual Mac selection'
    if env.get('GITHUB_REF') != 'refs/heads/main':
        raise ValueError('Automatic Mac beta requires main')
    if tick and (event != 'workflow_dispatch' or tick not in ('frequent', 'daily')):
        raise ValueError('Invalid automatic Mac tick')
    slot = macos_slot(now, env.get('RELEASE_SCHEDULE', '')) if event == 'schedule' else tick
    if slot is None or (slot == 'frequent' and now >= MACOS_BETA_UNTIL):
        return False, source, False, 'Mac beta window expired; waiting for daily 10:30 UTC slot'
    source = scheduled_source(event, source, 'macos')
    if not needed('schedule', descriptor(source, 'macos'), source, 'macos'):
        return False, source, False, 'Mac main tip already published'
    if macos_attempted(source, env['GITHUB_RUN_ID'], before_current=event == 'workflow_dispatch'):
        return False, source, False, 'Mac beta backoff: prior attempt for this SHA; waiting for new main'
    if event == 'schedule':
        macos_api('POST', '/actions/workflows/macos-release.yml/dispatches',
                  {'ref': 'main', 'inputs': {'beta_tick': slot}})
        return False, source, False, 'Dispatched unpublished Mac main tip; tick does not use macos runner'
    # Dispatch captures main in run.commit_sha; do NOT check out a newer SHA.
    # Publication keeps its strict artifact SOURCE_SHA == run.commit_sha check.
    return True, source, now < MACOS_BETA_UNTIL, 'Build source-pinned automatic Mac beta'


def descriptor(source, platform):
    url = f'https://updates.befeast.com/okilum/releases/{source}/{platform}.json'
    # curl uses the same public endpoint and transport as installed update clients.
    result = subprocess.run(['curl', '--silent', '--show-error', '--location',
                             '--max-time', '30', '--write-out', '\n%{http_code}', url],
                            check=True, capture_output=True, text=True)
    body, status = result.stdout.rsplit('\n', 1)
    if status == '404':
        return None
    if status != '200':
        raise ValueError(f'Cannot check published release: HTTP {status}')
    return json.loads(body)


def explicit_source(value, platform, ref, event):
    """A requested older main commit (#1040): built once, published to the archive only.

    Returns None when no source was requested. Refuses anything that is not a full
    SHA on main's history: the checkout is main with full history, so a commit that
    only exists on a branch is unknown here or not an ancestor of HEAD.
    """
    value = (value or '').strip()
    if not value:
        return None
    if platform == 'macos':
        raise ValueError('An explicit source is not available for the macOS lane yet')
    if event != 'workflow_dispatch' or ref != 'refs/heads/main':
        raise ValueError('An explicit source is built only by a manual run on main')
    if not re.fullmatch('[0-9a-f]{40}', value):
        raise ValueError('The source must be a full 40-character commit SHA')
    ancestor = subprocess.run(['git', 'merge-base', '--is-ancestor', value, 'HEAD'],
                              capture_output=True, text=True, timeout=30)
    if ancestor.returncode != 0:
        raise ValueError(f'{value} is not a commit on main')
    return value


def check_resign(resign, requested, platform):
    """Re-sign (#1104): the bytes of the published Windows build of this commit, signed and
    published under a new build number; nothing is compiled. Only that exact build qualifies."""
    if platform != 'windows' or not requested or not resign.isdigit():
        raise ValueError('Re-signing needs the Windows lane, an explicit source and a build number')
    published = descriptor(requested, 'windows')
    if published is None or published['build'] != int(resign):
        raise ValueError(f'Build {resign} is not the published Windows build of {requested}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('platform', choices=['macos', 'windows', 'linux'])
    args = parser.parse_args()
    event, source = os.environ['GITHUB_EVENT_NAME'], os.environ['GITHUB_SHA']
    requested = explicit_source(os.environ.get('OKILUM_SOURCE'), args.platform,
                                os.environ.get('GITHUB_REF', ''), event)
    resign = (os.environ.get('OKILUM_RESIGN_BUILD') or '').strip()
    local = False
    if args.platform == 'macos':
        try:
            build, source, local, reason = select_macos(event, source, dict(os.environ),
                                                        datetime.now(timezone.utc))
        except Exception as error:
            if os.environ.get('GITHUB_STEP_SUMMARY'):
                with Path(os.environ['GITHUB_STEP_SUMMARY']).open('a') as summary:
                    summary.write('Mac beta selection failed: ' + type(error).__name__ + '; no build dispatched.\n')
            raise
        print(reason)
        if os.environ.get('GITHUB_STEP_SUMMARY'):
            with Path(os.environ['GITHUB_STEP_SUMMARY']).open('a') as summary:
                summary.write(reason + '\n')
    elif resign:
        check_resign(resign, requested, args.platform)
        build, source = True, requested
        print(f'Re-sign Windows build {resign} of {requested}')
    elif requested:
        # An older main commit: build it unless its platform build already exists.
        build = descriptor(requested, args.platform) is None
        source = requested
        print(f'Explicit source {requested}: ' + ('build' if build else 'already published'))
    else:
        tip = scheduled_source(event, source, args.platform)
        if tip != source:
            print(f'Scheduled snapshot {source[:8]} is behind main; building main tip {tip[:8]}')
            source = tip
        published = descriptor(source, args.platform) if event == 'schedule' else None
        build = needed(event, published, source, args.platform)
        print('Build newest commit' if build else 'No build: coalescing or already published')
    with Path(os.environ['GITHUB_OUTPUT']).open('a') as output:
        output.write(f'build={str(build).lower()}\n')
        output.write(f'source={source}\n')
        output.write(f'resign={resign}\n')
        if args.platform == 'macos':
            output.write(f'local={str(local).lower()}\n')
            preview = bool(build and os.environ.get('MACOS_PREVIEW_PR'))
            output.write(f'preview={str(preview).lower()}\n')
