#!/usr/bin/env python3
"""Unsigned permanent Linux PR lane using the proven exact-head macOS transport."""
import importlib.util
import os
from pathlib import Path
import uuid

spec = importlib.util.spec_from_file_location('transport', Path(__file__).with_name('github-macos.py'))
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
WORKFLOW = '.github/workflows/forgejo-linux.yml'
BUILD_STEP = 'Run full Linux gate'
# The daily full set dispatches Linux, perf, shell and macOS at 04:00 UTC while agent
# checks also queue on the same GitHub account: a 15-minute queue budget failed main
# with no build output (#3045, run 28988). ci.yml's linux-github timeout must exceed
# QUEUE_TIMEOUT + RUN_TIMEOUT.
QUEUE_TIMEOUT = 3600
RUN_TIMEOUT = 5400


def main():
    result, branch, api = 'failure', None, None
    pushed = False
    receipt = None
    try:
        sha = os.environ['PR_HEAD_SHA']
        branch = transport.ref_name(os.environ['PR_NUMBER'], sha, os.environ['GITHUB_RUN_ID'],
                                    os.environ.get('GITHUB_RUN_ATTEMPT') or '1', uuid.uuid4().hex)
        branch = branch.replace('forgejo-pr/', 'forgejo-linux-pr/', 1)
        api = transport.GitHub()
        api.log_quota()
        receipt = transport.Receipt.open(branch, sha, WORKFLOW)  # #844
        transport.push_head(branch, sha, api.token)
        pushed = True
        result, message = transport.wait_for_run(api, branch, sha, workflow=WORKFLOW,
            job_name='linux', build_step=BUILD_STEP, queue_timeout=QUEUE_TIMEOUT, run_timeout=RUN_TIMEOUT,
            **(dict(on_run=receipt.observed) if receipt else {}))
        if receipt:
            receipt.finish()
        print(message)
    except transport.Unavailable as error:
        if receipt:
            receipt.finish()
        print(f'Hosted Linux unavailable; no fallback success: {error}')
    finally:
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write(f'result={result}\n')
        if pushed and branch and api:
            try:
                api.request('git/refs/heads/' + branch, 'DELETE')
            except transport.Unavailable:
                print('::warning::Temporary Linux ref cleanup failed')
    return 0 if result == 'success' else 1


if __name__ == '__main__':
    raise SystemExit(main())
