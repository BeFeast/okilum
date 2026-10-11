#!/usr/bin/env python3
"""Read Forgejo CI job logs by what people see, and keep failed ones where agents look.

    ci-logs.py get <run URL | run number> [job]   print one job's log (default: failed jobs)
    ci-logs.py collect DIR                        copy logs of new failed main/PR jobs to DIR

The number in a run's web URL (/actions/runs/6123) is not the API run id; asking the
API for /actions/runs/6123/jobs reads another run or none. This resolves the number
first. Logs are public for this repository: no token is needed (FORGEJO_TOKEN is used
when set). `collect` writes DIR/<run>-<job>.log and appends DIR/index.tsv:
  time  run  job  workflow  event  branch  sha  summary  url
and keeps 30 days. docs/hosted-ci.md, "Logs of failed jobs".
"""
import json
import os
import re
import signal
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

SERVER = os.environ.get('FORGEJO_URL', 'https://git.oklabs.uk')
REPO = os.environ.get('REPOSITORY', 'BeFeast/okilum')
API = f'{SERVER}/api/v1/repos/{REPO}'
KEEP_DAYS = 30
FAILED_TEST = re.compile(r'^---- (\S+) stdout ----$', re.M)
PREFIX = re.compile(r'^\S+Z ', re.M)
GITHUB_RUN = re.compile(r'https://github\.com/BeFeast/okilum/actions/runs/\d+')


def fetch(path, raw=False):
    headers = {'Authorization': f"token {os.environ['FORGEJO_TOKEN']}"} if os.environ.get('FORGEJO_TOKEN') else {}
    with urllib.request.urlopen(urllib.request.Request(API + path, headers=headers), timeout=60) as response:
        data = response.read()
    return data.decode('utf-8', 'replace') if raw else json.loads(data)


RUN_IDS = {}


def run_id(number):
    """API id of the run the web UI calls `number` (index_in_repo); one listing per process."""
    if number not in RUN_IDS:
        for page in range(1, 11):
            runs = fetch(f'/actions/runs?limit=50&page={page}')
            runs = runs.get('workflow_runs', runs) if isinstance(runs, dict) else runs
            for run in runs:
                RUN_IDS.setdefault(run.get('index_in_repo'), run['id'])
            if number in RUN_IDS or len(runs) < 50:
                break
    if number not in RUN_IDS:
        raise LookupError(f'ci-logs: run {number} not found in the last 500 runs')
    return RUN_IDS[number], None


def jobs(rid):
    result = fetch(f'/actions/runs/{rid}/jobs')
    return result if isinstance(result, list) else result.get('jobs', [])


# Jobs that only combine other jobs' results (ci.yml): their failure is elsewhere.
AGGREGATES = {('ci.yml', 'linux'), ('ci.yml', 'macos'), ('ci.yml', 'check')}


def summary(log, workflow='', job=''):
    if (workflow, job) in AGGREGATES:
        return 'aggregate: a job it needs failed (see that job)'
    text = PREFIX.sub('', log)
    github = GITHUB_RUN.search(text)
    if github:
        return 'hosted lane: the failure is in the GitHub run (log saved as .github.log)'
    if 'Missing, failed or ambiguous' in text or re.search(r'^\+?\s*test "\$\w+_RESULT" = success', text, re.M):
        return 'aggregate: a job it needs failed (see that job)'
    tests = FAILED_TEST.findall(text)
    if tests:
        return 'tests: ' + ' '.join(tests)
    error = re.search(r'^(error(\[E\d+\])?: .*)$', text, re.M)
    if error:
        return error.group(1)[:200]
    if not re.search(r'^\s*(Compiling|Checking|Finished|Running) ', text, re.M):
        return 'no build or test output (infrastructure?)'
    return '-'


def get(target, job_name=None):
    number = int(re.search(r'(\d+)(?:/jobs/\d+)?/?$', target).group(1))
    rid, _ = run_id(number)
    wanted = [j for j in jobs(rid) if (j['name'] == job_name if job_name else j['status'] == 'failure')]
    if not wanted:
        raise SystemExit(f'ci-logs: run {number} has no ' + (f'job {job_name}' if job_name else 'failed job'))
    for job in wanted:
        print(f'===== run {number} job {job["name"]} ({job["status"]})')
        sys.stdout.write(fetch(f'/actions/jobs/{job["id"]}/logs', raw=True))


def collect(directory):
    directory.mkdir(parents=True, exist_ok=True)
    index = directory / 'index.tsv'
    if not index.exists():
        index.write_text('time\trun\tjob\tworkflow\tevent\tbranch\tsha\tsummary\turl\n')
    tasks = fetch('/actions/tasks?limit=100')['workflow_runs']
    for task in tasks:
        if task['status'] != 'failure':
            continue
        if task.get('event') not in ('push', 'pull_request', 'pull_request_target', 'schedule', 'workflow_dispatch'):
            continue
        name = re.sub(r'[^A-Za-z0-9._-]+', '-', task['name'])
        target = directory / f"{task['run_number']}-{name}.log"
        if target.exists():
            continue
        try:
            rid, _ = run_id(task['run_number'])
        except LookupError as error:
            print(error)
            continue
        job = next((j for j in jobs(rid) if j['name'] == task['name']), None)
        if job is None:
            continue
        log = fetch(f'/actions/jobs/{job["id"]}/logs', raw=True)
        target.write_text(log)
        github = GITHUB_RUN.search(log)
        if github:
            # The hosted lane ran on the public GitHub mirror: keep its failed steps too.
            failed = subprocess.run(['gh', 'run', 'view', github.group(0).rsplit('/', 1)[1], '-R', 'BeFeast/okilum',
                                     '--log-failed'], capture_output=True, text=True, timeout=120)
            if failed.returncode == 0 and failed.stdout:
                target.with_suffix('.github.log').write_text(failed.stdout)
        row = [time.strftime('%Y-%m-%dT%H:%MZ', time.gmtime()), str(task['run_number']), task['name'],
               task['workflow_id'], task.get('event', ''), task.get('head_branch', ''), task['head_sha'][:12],
               summary(log, task['workflow_id'], task['name']), f"{SERVER}/{REPO}/actions/runs/{task['run_number']}" + (f' {github.group(0)}' if github else '')]
        with index.open('a') as stream:
            stream.write('\t'.join(cell.replace('\t', ' ').replace('\n', ' ') for cell in row) + '\n')
        print(f'saved {target.name}: {row[7]}')
    cutoff = time.time() - KEEP_DAYS * 86400
    for path in directory.glob('*.log*'):
        if path.stat().st_mtime < cutoff:
            path.unlink()


def main():
    signal.signal(signal.SIGPIPE, signal.SIG_DFL)  # `| head` must not raise BrokenPipeError
    if len(sys.argv) >= 3 and sys.argv[1] == 'get':
        get(sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else None)
    elif len(sys.argv) == 3 and sys.argv[1] == 'collect':
        collect(Path(sys.argv[2]))
    else:
        raise SystemExit(__doc__)


if __name__ == '__main__':
    main()
