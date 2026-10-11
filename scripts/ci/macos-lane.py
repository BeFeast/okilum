#!/usr/bin/env python3
"""Choose this PR run's macOS lane once: local only while the M4 is idle.

    MACOS_LANE=<repository variable> macos-lane.py      prints local or hosted

In the M4 window (OKILUM_MACOS_LANE=local) the single M4 slot queued PR checks for up to
17 minutes while GitHub-hosted macOS had no queue (2026-10-11 measurement). A PR now
uses the M4 only when no job is running on it; otherwise it goes hosted at once. If
the task list cannot be read, the repository variable decides, as before.
"""
import json
import os
import sys
import urllib.request

# Jobs that run on the M4 runner (runs-on: macos), as (workflow file, job name).
M4_JOBS = {('ci.yml', 'macos-local'), ('macos-release.yml', 'release'), ('macos-intel-qa.yml', 'build')}


def m4_busy(tasks):
    """The M4 jobs running now. Tasks exist once a runner has taken the job."""
    return [t for t in tasks if t.get('status') == 'running' and (t.get('workflow_id'), t.get('name')) in M4_JOBS]


def choose(variable, tasks):
    if variable != 'local':
        return 'hosted', 'repository variable is not local'
    if tasks is None:
        return 'local', 'task list unavailable; repository variable decides'
    busy = m4_busy(tasks)
    if busy:
        names = ', '.join(f"{t['workflow_id']}:{t['name']} #{t.get('run_number')}" for t in busy)
        return 'hosted', f'M4 busy ({names})'
    return 'local', 'M4 idle'


def main():
    server = os.environ.get('FORGEJO_URL', 'https://git.oklabs.uk')
    repository = os.environ.get('REPOSITORY', 'BeFeast/okilum')
    tasks = None
    if os.environ.get('MACOS_LANE') == 'local':
        try:
            url = f'{server}/api/v1/repos/{repository}/actions/tasks?limit=20'
            with urllib.request.urlopen(url, timeout=30) as response:
                tasks = json.load(response)['workflow_runs']
        except Exception as error:  # the variable still decides
            print(f'macos-lane: task list unavailable: {error}', file=sys.stderr)
    lane, reason = choose(os.environ.get('MACOS_LANE', ''), tasks)
    print(f'macos-lane: {lane} ({reason})', file=sys.stderr)
    print(lane)


if __name__ == '__main__':
    main()
