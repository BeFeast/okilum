#!/usr/bin/env python3
"""Unsigned hosted PR lanes using the exact-head macOS transport."""
import importlib.util
import os
from pathlib import Path
import uuid
import time
import fast_evidence

spec = importlib.util.spec_from_file_location('transport', Path(__file__).with_name('github-macos.py'))
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
LANES = {
    'fast': ('fast', 'All fast platform builds passed', 420),
    'shared': ('shared', 'Run every shared suite', 3600),
    'linux': ('linux', 'Run full Linux gate', 5400),
    'linux-binary': ('linux-binary', 'Build and package Linux QA binary', 5400),
    'brain': ('brain', 'Run Brain tests', 2700),
    'inbox': ('inbox', 'Run Inbox tests', 1800),
    'arch': ('arch', 'Validate Arch package', 5400),
    'windows-release': ('windows-release', 'Build and validate Windows package', 5400),
    'native-core': ('native', 'Run native tests', 3600),
    'native-sync': ('native', 'Run native tests', 3600),
    'native-shell': ('native', 'Run native tests', 4500),
    # #1639: only the editor latency gate blocks; legacy scroll is informational.
    'perf-linux': ('perf-linux', 'Check editor latency and the baseline ratchet', 3600),
    # #2589: Forgejo fast.yml main certification. Exceeds the hosted job's own
    # 45 min ceiling, so GitHub's deadline fires and is observed first.
    'shell-full': ('shell-full', 'Run the complete Linux shell binary suite', 47 * 60),
    # #2589: Forgejo fast.yml `check` (Beta's gate). Exceeds the longer hosted
    # job's 47 min ceiling, so GitHub's deadline fires and is observed first.
    'check': ('check', 'Build Linux desktop and run fast unit tests', 49 * 60),
}
# Further gate steps of the same hosted run, each proven by the jobs API.
ALSO_REQUIRED = {
    'check': (('python', 'Run the full CI tooling Python suite'),
              ('python', 'Run the full releases Python suite'),
              ('python', 'Run the full updater Python suite')),
}
# Waiters inside a fixed Forgejo step ceiling. The primary `bridge` runner stops
# every job at 60 min, so queue and run share one budget; the run still gets at
# most its lane timeout. See docs/fast-runner.md (#2589).
BRIDGE_BUDGETS = {'shell-full': 53 * 60, 'check': 53 * 60}


def main():
    started = time.monotonic()
    result, branch, api = 'failure', None, None
    pushed = False
    token = ''
    try:
        lane = os.environ['CI_LANE']
        if lane not in LANES:
            raise transport.Unavailable('Unknown hosted lane')
        job, step, timeout = LANES[lane]
        sha = os.environ['PR_HEAD_SHA']
        branch = transport.ref_name(os.environ['PR_NUMBER'], sha, os.environ['GITHUB_RUN_ID'],
                                    os.environ.get('GITHUB_RUN_ATTEMPT') or '1', uuid.uuid4().hex)
        branch = branch.replace('forgejo-pr/', f'forgejo-{lane}-pr/', 1)
        if lane == 'fast':
            # A normal fast-forward ref retains PR-local compiler caches. No force push.
            branch = f"forgejo-fast-pr/{os.environ['PR_NUMBER']}"
        api = transport.GitHub()
        api.log_quota()
        token = api.token
        transport.push_head(branch, sha, token)
        pushed = True
        if lane == 'fast':
            result, message = fast_evidence.wait(api, branch, sha, timeout=max(1, 540 - (time.monotonic() - started)))
        else:
            limits = dict(queue_timeout=1800, run_timeout=timeout)
            if lane in BRIDGE_BUDGETS:
                limits.update(queue_timeout=BRIDGE_BUDGETS[lane], deadline=started + BRIDGE_BUDGETS[lane])
            if lane in ALSO_REQUIRED:
                limits.update(also_required=ALSO_REQUIRED[lane])
            result, message = transport.wait_for_run(api, branch, sha, workflow=f'.github/workflows/forgejo-{lane}.yml',
                job_name=job, build_step=step, **limits)
        print(message)
    except transport.Unavailable as error:
        print(f'Hosted CI unavailable; no fallback success: {error}')
    finally:
        if os.environ.get('CI_LANE') == 'shared':
            print(f'Shared hosted bridge queue+run seconds={time.monotonic() - started:.1f}', flush=True)
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write(f'result={result}\n')
        if pushed and branch and api and os.environ.get('CI_LANE') != 'fast':
            try:
                token = api.token
                api.request('git/refs/heads/' + branch, 'DELETE')
            except transport.Unavailable as error:
                reason = transport.safe_diagnostic(error, token)
                print(f'::warning::Temporary hosted ref cleanup failed: {reason}')
    return 0 if result == 'success' else 1


if __name__ == '__main__':
    raise SystemExit(main())
