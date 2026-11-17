#!/usr/bin/env python3
"""Post-job hook (#844): cancel the one GitHub run a killed bridge left running.

Called by .forgejo/actions/hosted-cancel after a Forgejo job ends. It reads the
credential-free receipt the bridge wrote and acts only when the bridge never
reached its own verdict, that is when Forgejo cancelled, superseded or killed
the job. It never dispatches, never changes a Forgejo status, never selects a
run by PR number, and never touches release or publication producers.
Every request is bounded; the outcome is printed as one line.
"""
import importlib.util
import json
import os
from pathlib import Path
import sys
import time
import urllib.parse

spec = importlib.util.spec_from_file_location('transport', Path(__file__).with_name('github-macos.py'))
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)

REQUEST_TIMEOUT = 10
DISCOVERY_READS = 5
DISCOVERY_DELAY = 3
ACTIVE = frozenset({'queued', 'in_progress', 'waiting', 'requested', 'pending'})


class InvalidReceipt(Exception):
    pass


def load(path, env):
    """The receipt's (branch, sha, workflow, run_id, finished), bound to this Forgejo attempt."""
    try:
        data = json.loads(Path(path).read_text())
    except (OSError, ValueError):
        raise InvalidReceipt('unreadable') from None
    if not isinstance(data, dict) or data.get('version') != 1:
        raise InvalidReceipt('unknown format')
    branch, sha, workflow = data.get('branch'), data.get('sha'), data.get('workflow')
    run_id, finished = data.get('run_id'), data.get('finished')
    if type(finished) is not bool:
        raise InvalidReceipt('missing completion flag')
    identity = transport.owned_identity(branch, sha, workflow)
    if identity is None:
        raise InvalidReceipt('not a cancellable bridge invocation')
    # The unique ref embeds the Forgejo run id and attempt that created it.
    if identity != (env.get('GITHUB_RUN_ID', ''), env.get('GITHUB_RUN_ATTEMPT') or '1'):
        raise InvalidReceipt('belongs to another Forgejo run or attempt')
    if run_id is not None and (type(run_id) is not int or run_id <= 0):
        raise InvalidReceipt('invalid GitHub run id')
    return branch, sha, workflow, run_id, finished


def owned(run, branch, sha, workflow):
    return (run.get('head_sha') == sha and run.get('head_branch') == branch
            and run.get('event') == 'push' and run.get('path') == workflow)


def discover(api, branch, sha, workflow, sleep):
    """The single run of this unique ref, or None; tolerates a not-yet-listed run."""
    query = urllib.parse.urlencode({'branch': branch, 'head_sha': sha, 'event': 'push', 'per_page': 100})
    for attempt in range(DISCOVERY_READS):
        runs = [run for run in api.request(f'actions/runs?{query}')['workflow_runs']
                if owned(run, branch, sha, workflow)]
        if len(runs) > 1:
            raise InvalidReceipt('ambiguous GitHub runs for one invocation ref')
        if runs and type(runs[0].get('id')) is int and runs[0]['id'] > 0:
            return runs[0]['id']
        if attempt + 1 < DISCOVERY_READS:
            sleep(DISCOVERY_DELAY)
    return None


def cancel(api, branch, sha, workflow, run_id, *, sleep=time.sleep):
    """Returns (outcome, run_id). Outcomes: accepted, already-completed, not-found, refused, failed."""
    try:
        if run_id is None:
            run_id = discover(api, branch, sha, workflow, sleep)
            if run_id is None:
                return 'not-found', None
        run = api.request(f'actions/runs/{run_id}')
        if not isinstance(run, dict) or run.get('id') != run_id or not owned(run, branch, sha, workflow):
            return 'refused', run_id
        if run.get('status') == 'completed':
            return 'already-completed', run_id
        if run.get('status') not in ACTIVE:
            return 'refused', run_id
        api.request(f'actions/runs/{run_id}/cancel', 'POST')
        return 'accepted', run_id
    except InvalidReceipt:
        return 'refused', run_id
    except transport.Unavailable as error:
        print(f'::warning::Owned GitHub run cancellation not confirmed: {error}', flush=True)
        return 'failed', run_id
    except (KeyError, TypeError, AttributeError):
        print('::warning::Owned GitHub run cancellation not confirmed: malformed GitHub response', flush=True)
        return 'failed', run_id


def delete_ref(api, branch):
    # The bridge deletes its unique ref itself; a killed bridge cannot.
    try:
        api.request('git/refs/heads/' + branch, 'DELETE')
    except transport.Unavailable:
        print('::warning::Temporary hosted ref cleanup failed', flush=True)


def main(argv, env=None, *, sleep=time.sleep, github=None):
    env = os.environ if env is None else env
    if len(argv) != 2:
        print('::warning::Usage: github-cancel.py RECEIPT', flush=True)
        return 0
    try:
        branch, sha, workflow, run_id, finished = load(argv[1], env)
    except InvalidReceipt as error:
        print(f'::warning::Hosted cancellation receipt ignored: {error}', flush=True)
        return 0
    if finished:
        print('Hosted bridge reached its verdict; no cancellation', flush=True)
        return 0
    try:
        api = (github or transport.GitHub)()
        api.timeout = REQUEST_TIMEOUT
    except transport.Unavailable as error:
        print(f'::warning::Owned GitHub run cancellation not attempted: {error}', flush=True)
        return 0
    outcome, run_id = cancel(api, branch, sha, workflow, run_id, sleep=sleep)
    if outcome != 'refused':
        delete_ref(api, branch)
    url = f'https://github.com/{transport.REPOSITORY}/actions/runs/{run_id}' if run_id else 'none'
    line = f'Hosted cancellation outcome={outcome} run={url} ref={branch}'
    print(line if outcome in ('accepted', 'already-completed') else f'::warning::{line}', flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main(sys.argv))
