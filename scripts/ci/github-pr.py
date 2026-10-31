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
}


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
            result, message = transport.wait_for_run(api, branch, sha, workflow=f'.github/workflows/forgejo-{lane}.yml',
                job_name=job, build_step=step, queue_timeout=1800, run_timeout=timeout)
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
