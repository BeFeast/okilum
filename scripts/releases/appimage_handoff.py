#!/usr/bin/env python3
"""Independent beta AppImage handoff; no Arch artifact, aggregate or feed authority."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import zipfile

import appimage_artifact as artifact
import appimage_publish as publisher
import publication


class ReuseUnavailable(ValueError):
    """Expected lack of accepted bytes; never covers identity or transport errors."""


def stage_result(api, source, directory, fetch):
    try:
        stage(api, source, directory, fetch)
    except ReuseUnavailable as error:
        print(f'APPIMAGE_REUSE_SKIP source={source} reason={error}')
        return False
    return True


def execution_state(record, label, active):
    """Validate GitHub evidence before classifying ordinary unavailability."""
    conclusions = {'success', 'failure', 'neutral', 'cancelled', 'skipped',
                   'timed_out', 'action_required', 'stale', 'startup_failure'}
    if not isinstance(record, dict):
        raise ValueError(f'Malformed {label} evidence')
    status = record.get('status')
    conclusion = record.get('conclusion')
    if (not isinstance(status, str) or status not in active | {'completed'}
            or 'conclusion' not in record
            or (status == 'completed' and
                (not isinstance(conclusion, str) or conclusion not in conclusions))
            or (status != 'completed' and conclusion is not None)):
        raise ValueError(f'Malformed {label} status/conclusion')


def accepted(run, jobs, source):
    execution_state(run, 'producer', {'queued', 'in_progress', 'waiting', 'pending', 'requested'})
    if (not isinstance(jobs, list) or any(not isinstance(job, dict) for job in jobs)
            or not isinstance(run.get('repository'), dict)
            or type(run.get('id')) is not int or run['id'] <= 0):
        raise ValueError('Malformed producer/job evidence')
    if (not re.fullmatch('[0-9a-f]{40}', source)
            or run.get('repository', {}).get('full_name') != artifact.REPOSITORY
            or run.get('path') != artifact.WORKFLOW or run.get('head_sha') != source
            or run.get('head_branch') != 'main' or run.get('event') != 'push'
            or type(run.get('run_attempt')) is not int or run['run_attempt'] != 1
            or type(run.get('run_number')) is not int or run['run_number'] <= 0):
        raise ValueError('Expected original main AppImage producer')
    if run['conclusion'] == 'cancelled':
        raise ValueError('Cancelled original AppImage producer')
    matches = [job for job in jobs if job.get('name') == 'appimage']
    if not matches:
        if run['status'] != 'completed':
            raise ReuseUnavailable('producer-not-completed')
        raise ReuseUnavailable('appimage-job-absent')
    if len(matches) != 1:
        raise ValueError('Ambiguous AppImage job')
    job = matches[0]
    if job.get('run_id') != run['id'] or job.get('head_sha') != source:
        raise ValueError('AppImage job identity mismatch')
    execution_state(job, 'AppImage job', {'queued', 'in_progress', 'waiting', 'pending'})
    steps = job.get('steps')
    if not isinstance(steps, list) or any(not isinstance(step, dict) for step in steps):
        raise ValueError('Malformed AppImage steps')
    smoke = [step for step in steps if step.get('name') == artifact.SMOKE]
    if not smoke:
        # Old immutable main artifacts executed the full two-distro smoke. Never
        # use this fallback when a new package-check step exists but is pending/failed.
        smoke = [step for step in steps if step.get('name') == 'Verify unchanged AppImage on clean Ubuntu and Fedora']
    if len(smoke) > 1:
        raise ValueError('Ambiguous AppImage smoke')
    if smoke:
        execution_state(smoke[0], 'AppImage smoke', {'queued', 'in_progress', 'pending'})
        if job['status'] == 'completed' and smoke[0]['status'] != 'completed':
            raise ValueError('Completed AppImage job has unfinished smoke')
    if job['status'] != 'completed':
        if run['status'] != 'completed':
            raise ReuseUnavailable('producer-not-completed')
        raise ValueError('Completed producer has unfinished AppImage job')
    if (job.get('status') != 'completed' or job.get('conclusion') != 'success'
            or not smoke or smoke[0].get('conclusion') != 'success'):
        raise ReuseUnavailable('appimage-smoke-not-accepted')


def unpack(directory, destination):
    receipt = json.loads((directory / 'acceptance.json').read_bytes())
    source = receipt['run']['head_sha']
    accepted(receipt['run'], receipt['jobs'], source)
    artifact.artifact_identity(receipt['artifact'], receipt['run'], source)
    data = (directory / 'artifact.zip').read_bytes()
    if 'sha256:' + hashlib.sha256(data).hexdigest() != receipt['artifact']['digest']:
        raise ValueError('Transferred archive differs from original GitHub receipt')
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        names = archive.namelist()
        if len(names) != 4 or set(names) != {artifact.IMAGE, 'provenance.json', 'SHA256SUMS', 'bundled-libraries.txt'}:
            raise ValueError('Unexpected AppImage archive members')
    publication.extract(data, destination)
    publisher.validate(destination, source, receipt)
    return source, receipt


def stage(api, source, directory, fetch):
    if not re.fullmatch('[0-9a-f]{40}', source):
        raise ValueError('Expected full source SHA')
    runs = api.request(f'actions/workflows/forgejo-linux.yml/runs?head_sha={source}&event=push&per_page=100')
    if runs.get('total_count', len(runs['workflow_runs'])) != len(runs['workflow_runs']):
        raise ValueError('Incomplete producer listing')
    matches = [run for run in runs['workflow_runs'] if run.get('head_sha') == source
               and run.get('path') == artifact.WORKFLOW and run.get('head_branch') == 'main']
    if not matches:
        raise ReuseUnavailable('no-exact-main-producer')
    if len(matches) != 1:
        raise ValueError('Ambiguous original main producer')
    run = api.request(f"actions/runs/{matches[0]['id']}")
    jobs = api.request(f"actions/runs/{run['id']}/attempts/1/jobs?per_page=100")
    items = api.request(f"actions/runs/{run['id']}/artifacts?per_page=100")
    for response, key in [(jobs, 'jobs'), (items, 'artifacts')]:
        if response.get('total_count', len(response[key])) != len(response[key]):
            raise ValueError('Incomplete GitHub evidence')
    accepted(run, jobs['jobs'], source)
    matches = [item for item in items['artifacts'] if item.get('name') == f'linux-appimage-{source}']
    if not matches:
        raise ReuseUnavailable('payload-absent')
    if len(matches) != 1:
        raise ValueError('Ambiguous AppImage artifact')
    item = matches[0]
    artifact.artifact_identity({**item, 'expired': False}, run, source)
    if item.get('expired') is True:
        raise ReuseUnavailable('payload-expired')
    artifact.artifact_identity(item, run, source)
    data = fetch(item['id'])
    if 'sha256:' + hashlib.sha256(data).hexdigest() != item['digest']:
        raise ValueError('Original GitHub artifact digest mismatch')
    current = api.request(f"actions/runs/{run['id']}")
    accepted(current, jobs['jobs'], source)
    for field in ('id', 'head_sha', 'path', 'head_branch', 'event', 'run_attempt', 'run_number'):
        if current.get(field) != run.get(field):
            raise ValueError('Producer identity changed during transfer')
    directory.mkdir(parents=True, exist_ok=False)
    (directory / 'artifact.zip').write_bytes(data)
    (directory / 'acceptance.json').write_text(json.dumps({'run': run, 'jobs': jobs['jobs'], 'artifact': item}, sort_keys=True))
    with tempfile.TemporaryDirectory() as temporary:
        unpack(directory, Path(temporary) / 'payload')


def trusted(run, run_id):
    if (run.get('status') == 'cancelled' or run.get('id') != run_id or run.get('workflow_id') != 'linux-release.yml'
            or run.get('prettyref') != 'main' or run.get('is_fork_pull_request') is not False
            or run.get('trigger_event') not in ('push', 'schedule', 'workflow_dispatch')
            or not re.fullmatch('[0-9a-f]{40}', run.get('commit_sha', ''))
            or type(run.get('index_in_repo')) is not int or run['index_in_repo'] <= 0):
        raise ValueError('Expected trusted main linux-release origin')


def publish(client, run_id):
    run = client.call('GET', f'/actions/runs/{run_id}')
    trusted(run, run_id)
    # The independent dispatch job may still be completing, or Arch may have failed.
    # The unique uploaded handoff and original accepted producer authorize these bytes.
    items = client.call('GET', f'/actions/runs/{run_id}/artifacts')
    matches = [item for item in items if item.get('name') == 'appimage-publication'
               and item.get('expired') is False and item.get('run_id') == run_id]
    if len(matches) != 1:
        raise ValueError('Missing or ambiguous appimage-publication artifact')
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        data = client.call('GET', f'/actions/artifacts/{matches[0]["id"]}/zip', raw=True)
        publication.extract(data, root / 'handoff')
        source, receipt = unpack(root / 'handoff', root / 'payload')
        subprocess.run(['git', 'merge-base', '--is-ancestor', source, 'origin/main'],
                       check=True, capture_output=True, timeout=30)
        if client.call('GET', f'/actions/runs/{run_id}') != run:
            # Completion/status changes are normal; origin identity must remain fixed.
            current = client.call('GET', f'/actions/runs/{run_id}')
            trusted(current, run_id)
            for field in ('commit_sha', 'index_in_repo', 'trigger_event'):
                if current[field] != run[field]:
                    raise ValueError('Publication origin changed during transfer')
        with publisher.arch.SigningKey() as key:
            publisher.publish(publisher.arch.R2(), key, root, root / 'payload', source,
                              receipt, 5000 + run['index_in_repo'], client=client)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['stage', 'dispatch', 'publish'])
    parser.add_argument('--source')
    parser.add_argument('--directory', type=Path)
    parser.add_argument('--run', type=int)
    parser.add_argument('--wait-seconds', type=int, default=0)
    args = parser.parse_args()
    if args.command == 'stage':
        if not args.source or args.directory is None:
            parser.error('stage requires --source and --directory')
        api = artifact.transport.GitHub()
        if not 0 <= args.wait_seconds <= 5400:
            raise ValueError('Invalid bounded artifact wait')
        deadline = time.monotonic() + args.wait_seconds
        while True:
            try:
                stage(api, args.source, args.directory, lambda aid: artifact.download(api.token, aid))
                accepted_bytes = True
                break
            except ReuseUnavailable as error:
                if time.monotonic() >= deadline:
                    if args.wait_seconds:
                        raise ValueError('Beta artifact did not become available within its build budget') from error
                    accepted_bytes = False
                    break
                # Only original producer availability is polled. Identity,
                # transport, failed smoke and malformed evidence still fail closed.
                if str(error) not in ('producer-not-completed', 'no-exact-main-producer'):
                    raise
                time.sleep(min(20, max(0, deadline - time.monotonic())))
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write(f'accepted={str(accepted_bytes).lower()}\n')
    elif args.command == 'dispatch':
        if os.environ.get('GITHUB_REF') != 'refs/heads/main':
            raise ValueError('Dispatch requires main')
        publication.Forgejo().call('POST', '/actions/workflows/release-publish.yml/dispatches',
            {'ref': 'main', 'inputs': {'platform': 'appimage', 'run_id': os.environ['GITHUB_RUN_ID']}})
    else:
        if args.run is None or args.run <= 0:
            parser.error('publish requires a positive --run')
        publish(publication.Forgejo(), args.run)


if __name__ == '__main__':
    main()
