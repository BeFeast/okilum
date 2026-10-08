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


def main():
    result, branch, api = 'failure', None, None
    try:
        token = os.environ.get('MIRROR_TOKEN')
        if not token:
            raise transport.Unavailable('Mirror token unavailable')
        sha = os.environ['PR_HEAD_SHA']
        branch = transport.ref_name(os.environ['PR_NUMBER'], sha, os.environ['GITHUB_RUN_ID'],
                                    os.environ.get('GITHUB_RUN_ATTEMPT') or '1', uuid.uuid4().hex)
        branch = branch.replace('forgejo-pr/', 'forgejo-linux-pr/', 1)
        api = transport.GitHub(token)
        with transport.cancellation_scope(api, branch, sha, WORKFLOW) as cancellation:
            transport.push_head(branch, sha, token)
            result, message = transport.wait_for_run(api, branch, sha, workflow=WORKFLOW,
                job_name='linux', build_step=BUILD_STEP, queue_timeout=900, run_timeout=3600, cancellation=cancellation)
        print(message)
    except transport.BridgeCancelled as error:
        print(str(error), flush=True)
        raise SystemExit(130)
    except transport.Unavailable as error:
        print(f'Hosted Linux unavailable; no fallback success: {error}')
    finally:
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write(f'result={result}\n')
        if branch and api:
            try:
                api.request('git/refs/heads/' + branch, 'DELETE')
            except transport.Unavailable:
                print('::warning::Temporary Linux ref cleanup failed')
    return 0 if result == 'success' else 1


if __name__ == '__main__':
    raise SystemExit(main())
