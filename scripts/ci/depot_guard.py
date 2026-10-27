#!/usr/bin/env python3
"""Inactive-by-default Depot pilot guard; no account creation or run dispatch."""
import datetime
import importlib.util
import json
import math
import os
from pathlib import Path
import sys
import urllib.error
import urllib.parse
import urllib.request

spec = importlib.util.spec_from_file_location('depot_transport', Path(__file__).with_name('github-macos.py'))
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
VARIABLES = ('OKILUM_GITHUB_MACOS_RUNNER', 'OKILUM_DEPOT_MACOS_SHADOW', 'OKILUM_DEPOT_MACOS_RUNNER')
WORKFLOWS = ('forgejo-macos.yml', 'forgejo-macos-depot.yml')
RATE = 0.08


def timestamp(value):
    if not isinstance(value, str):
        raise ValueError('Malformed timestamp')
    parsed = datetime.datetime.fromisoformat(value.replace('Z', '+00:00'))
    if parsed.utcoffset() is None:
        raise ValueError('Timestamp must include UTC offset')
    return parsed.timestamp()


def budget(usage, cap):
    """Conservative gross upper bound, not an invoice: ignore included credits."""
    if not math.isfinite(cap) or cap < 500:
        raise ValueError('Pilot cap must be at least USD500')
    if not isinstance(usage, dict):
        raise ValueError('Malformed Depot usage')
    if any(usage.get(k) for k in ('containerBuild', 'storage', 'agentSandbox')):
        raise ValueError('Depot non-pilot resource usage cannot be priced')
    groups = usage.get('githubActionsJobs', [])
    if not isinstance(groups, list):
        raise ValueError('Malformed Depot usage')
    minutes = 0.0
    for group in groups:
        if not isinstance(group, dict):
            raise ValueError('Malformed Depot repository usage')
        if group.get('repo') not in ('BeFeast/okilum', 'okilum'):
            raise ValueError('Depot organization contains non-pilot usage')
        jobs = group.get('jobs', [])
        if not isinstance(jobs, list):
            raise ValueError('Malformed Depot job usage')
        if not jobs and group.get('total', {}).get('jobCount', 0):
            raise ValueError('Depot per-runner usage details missing')
        for job in jobs:
            if not isinstance(job, dict):
                raise ValueError('Malformed Depot runner usage')
            if job.get('runner') != 'depot-macos-15':
                raise ValueError('Unpriced Depot runner usage')
            values = [job.get(k, 0) for k in ('minutesElapsed', 'minutesBilled')]
            if any(type(x) not in (int, float) or not math.isfinite(x) or x < 0 for x in values):
                raise ValueError('Malformed Depot minutes')
            minutes += max(values)
    # Reserve the entire monthly Developer fee, even for a partial-month pilot.
    gross = 20 + RATE * minutes
    return gross, gross >= cap * .6, gross >= cap * .9


def depot_usage(token, start, now, opener=urllib.request.urlopen):
    def iso(t):
        return datetime.datetime.fromtimestamp(t, datetime.timezone.utc).isoformat().replace('+00:00', 'Z')
    request = urllib.request.Request('https://api.depot.dev/depot.core.v1.UsageService/GetUsage',
        method='POST', data=json.dumps({'startAt': iso(start), 'endAt': iso(now)}).encode(),
        headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json',
                 'Connect-Protocol-Version': '1'})
    try:
        with opener(request, timeout=30) as response:
            value = json.load(response)
    except (OSError, ValueError):
        raise ValueError('Depot usage unavailable (response and credentials withheld)') from None
    # Proto3 may omit repeated fields for an unused organization. Period identity
    # distinguishes a valid zero-use result from an arbitrary empty response.
    if not isinstance(value, dict) or 'periodStart' not in value or 'periodEnd' not in value:
        raise ValueError('Depot usage period missing')
    if abs(timestamp(value['periodStart']) - start) > 1 or abs(timestamp(value['periodEnd']) - now) > 300:
        raise ValueError('Depot usage period mismatch')
    return value


def queued(api, now, start):
    offenders = []
    for workflow in WORKFLOWS:
        query = urllib.parse.urlencode({'status': 'queued', 'event': 'push', 'per_page': 100})
        listing = api.request(f'actions/workflows/{workflow}/runs?{query}')
        runs = listing['workflow_runs']
        if listing.get('total_count', len(runs)) != len(runs):
            raise ValueError('Active Depot queue listing incomplete')
        for run in runs:
            branch = run.get('head_branch', '')
            if (run.get('path') != '.github/workflows/' + workflow or run.get('event') != 'push'
                    or not (branch == 'main' or branch.startswith('forgejo-pr/'))):
                raise ValueError('Unexpected pilot queue identity')
            created = timestamp(run['created_at'])
            if created >= start and now - created > 600:
                # Only a queued job assigned a Depot label authorizes fallback.
                jobs = api.request(f"actions/runs/{run['id']}/jobs?per_page=100")
                if jobs.get('total_count', len(jobs['jobs'])) != len(jobs['jobs']):
                    raise ValueError('Active Depot jobs listing incomplete')
                for job in jobs['jobs']:
                    if (job.get('name') == 'macos' and job.get('status') == 'queued'
                            and 'depot-macos-15' in job.get('labels', [])):
                        offenders.append(run['id'])
    return offenders


def disable(api):
    # Inspect the list to avoid DELETE404 and never remove unrelated variables.
    listing = api.request('actions/variables?per_page=100')
    values = listing['variables']
    if listing.get('total_count', len(values)) != len(values):
        raise ValueError('GitHub variable listing incomplete')
    current = {v['name']: v.get('value', '') for v in values}
    for name in VARIABLES:
        if name in current:
            if name == 'OKILUM_GITHUB_MACOS_RUNNER' and not current[name].startswith('depot-'):
                continue
            api.request('actions/variables/' + name, 'DELETE')


def guard(api, usage, cap, now, start, end, log=print):
    reason = None
    offenders = []
    try:
        if not start <= now < end or end - start > 14 * 86400:
            raise ValueError('Pilot window expired or invalid')
        gross, alert, stop = budget(usage, cap)
        log(f'Depot conservative gross budget USD{gross:.2f}/{cap:.2f}; invoice unverified')
        if alert:
            log('::warning::Depot pilot reached 60% of the conservative spending cap')
        if stop:
            reason = 'Depot pilot reached 90% of cap (reserve for in-flight work)'
        else:
            offenders = queued(api, now, start)
            if offenders:
                reason = 'Depot queue exceeded 10 minutes'
    except (ValueError, KeyError, TypeError, transport.Unavailable):
        reason = 'Depot guard evidence unavailable/invalid; fail closed'
    if reason:
        disable(api)
        log('::warning::' + reason + '; Depot variables unset, new native jobs use macos-15')
        # Do not rerun or cancel any current native verdict. Unsetting a variable
        # affects new jobs only; pending jobs remain visibly pending/failed.
        return 'disabled'
    return 'healthy'


def main():
    if os.getenv('DEPOT_PILOT_ENABLED') != 'true':
        print('Depot pilot OFF; no API calls or jobs requested')
        return 0
    now = datetime.datetime.now(datetime.timezone.utc).timestamp()
    credentials = transport.auth.Credentials(permissions={**transport.auth.PERMISSIONS, 'variables': 'write'})
    api = transport.GitHub(credentials)
    api.log_quota()
    try:
        start, end = float(os.environ['DEPOT_PILOT_START']), float(os.environ['DEPOT_PILOT_END'])
        cap = float(os.environ['DEPOT_PILOT_CAP_USD'])
        token = os.environ['DEPOT_USAGE_TOKEN']
        if not token:
            raise ValueError('Missing Depot usage credential')
        usage = depot_usage(token, start, now)
    except (ValueError, KeyError):
        disable(api)
        print('::warning::Depot guard input/usage unavailable; Depot variables unset')
        return 1
    result = guard(api, usage, cap, now, start, end)
    print('Depot guard: ' + result)
    return 0


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except (transport.Unavailable, transport.auth.CredentialError, ValueError, KeyError, TypeError):
        print('Depot guard STOP: fallback could not be confirmed; operator action required', file=sys.stderr)
        raise SystemExit(1) from None
