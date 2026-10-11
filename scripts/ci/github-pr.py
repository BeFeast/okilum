#!/usr/bin/env python3
"""Unsigned hosted PR lanes using the exact-head macOS transport."""
import importlib.util
import os
from pathlib import Path
import uuid

spec = importlib.util.spec_from_file_location('transport', Path(__file__).with_name('github-macos.py'))
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
LANES = {
    'linux': ('linux', 'Run full Linux gate', 3600),
    'linux-binary': ('linux-binary', 'Build and package Linux QA binary', 5400),
    'brain': ('brain', 'Run Brain tests', 2700),
    'inbox': ('inbox', 'Run Inbox tests', 1800),
    'arch': ('arch', 'Validate Arch package', 5400),
    'windows-release': ('windows-release', 'Build and validate Windows package', 5400),
    'native-core': ('native', 'Run native tests', 3600),
    'native-sync': ('native', 'Run native tests', 3600),
    'native-shell': ('native', 'Run native tests', 4500),
}


def main():
    result, branch, api = 'failure', None, None
    try:
        lane = os.environ['CI_LANE']
        if lane not in LANES:
            raise transport.Unavailable('Unknown hosted lane')
        job, step, timeout = LANES[lane]
        token = os.environ.get('MIRROR_TOKEN')
        if not token:
            raise transport.Unavailable('Mirror token unavailable')
        sha = os.environ['PR_HEAD_SHA']
        branch = transport.ref_name(os.environ['PR_NUMBER'], sha, os.environ['GITHUB_RUN_ID'],
                                    os.environ.get('GITHUB_RUN_ATTEMPT') or '1', uuid.uuid4().hex)
        branch = branch.replace('forgejo-pr/', f'forgejo-{lane}-pr/', 1)
        api = transport.GitHub(token)
        transport.push_head(branch, sha, token)
        result, message = transport.wait_for_run(api, branch, sha, workflow=f'.github/workflows/forgejo-{lane}.yml',
            job_name=job, build_step=step, queue_timeout=1800, run_timeout=timeout)
        print(message)
    except transport.Unavailable as error:
        print(f'Hosted CI unavailable; no fallback success: {error}')
    finally:
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write(f'result={result}\n')
        if branch and api:
            try:
                api.request('git/refs/heads/' + branch, 'DELETE')
            except transport.Unavailable:
                print('::warning::Temporary hosted ref cleanup failed')
    return 0 if result == 'success' else 1


if __name__ == '__main__':
    raise SystemExit(main())
