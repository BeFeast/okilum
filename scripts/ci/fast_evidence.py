"""Exact three-platform execution receipts; no fourth hosted aggregator runner."""
import time

WORKFLOW = '.github/workflows/forgejo-fast.yml'
STEP = 'Build desktop and run fast unit tests'
PLATFORMS = {
    'build (ubuntu-24.04, linux)': 'ubuntu-24.04',
    'build (windows-2022, windows)': 'windows-2022',
    'build (macos-15, macos)': 'macos-15',
}


def executed(run, jobs, branch, sha):
    if (run.get('head_sha') != sha or run.get('head_branch') != branch
            or run.get('path') != WORKFLOW or run.get('event') != 'push'
            or run.get('run_attempt') != 1):
        raise ValueError('Fast run identity mismatch')
    if run.get('conclusion') not in (None, 'success'):
        return False
    # API run.summary can remain queued after jobs finish (#1815). The gate
    # consists of these three jobs, not a fourth aggregate job or that summary.
    if len(jobs) != 3 or {j.get('name') for j in jobs} != set(PLATFORMS):
        return False
    for job in jobs:
        steps = [s for s in job.get('steps', []) if s.get('name') == STEP]
        if (job.get('run_id') != run['id'] or job.get('head_sha') != sha
                or PLATFORMS[job['name']] not in job.get('labels', [])
                or job.get('status') != 'completed' or job.get('conclusion') != 'success'
                or len(steps) != 1 or steps[0].get('status') != 'completed'
                or steps[0].get('conclusion') != 'success'):
            return False
    return True


def wait(api, branch, sha, *, timeout=540, clock=time.monotonic, sleep=time.sleep):
    deadline = clock() + timeout
    while clock() < deadline:
        runs = api.request(f'actions/workflows/forgejo-fast.yml/runs?head_sha={sha}&event=push&per_page=100')['workflow_runs']
        matches = [r for r in runs if r.get('head_branch') == branch and r.get('head_sha') == sha
                   and r.get('path') == WORKFLOW and r.get('event') == 'push']
        if len(matches) > 1:
            raise ValueError('Ambiguous exact fast producer')
        if matches:
            run = matches[0]
            payload = api.request(f"actions/runs/{run['id']}/jobs?per_page=100")
            jobs = payload['jobs']
            if payload.get('total_count', len(jobs)) != len(jobs):
                raise ValueError('Incomplete fast job inventory')
            if executed(run, jobs, branch, sha):
                return 'success', f"All three exact-source platform builds/unit suites passed: {run['html_url']}"
            if run.get('conclusion') in ('failure', 'cancelled', 'timed_out') or any(
                    j.get('status') == 'completed' and j.get('conclusion') not in ('success', None)
                    for j in jobs):
                return 'failure', f"Fast platform failed/skipped/timed out: {run['html_url']}"
        sleep(min(10, max(0, deadline - clock())))
    return 'failure', 'Fast wall-clock deadline expired; no fallback success'
