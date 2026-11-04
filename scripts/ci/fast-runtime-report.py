#!/usr/bin/env python3
"""Read-only 24-hour fast.yml follow-up for #2503, using a merged PR's time."""
import argparse
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import urllib.request

BASE = 'https://git.oklabs.uk/api/v1/repos/BeFeast/okilum/'


def get(path):
    request = urllib.request.Request(BASE + path, headers={
        'Authorization': 'token ' + os.environ['FORGEJO_TOKEN'],
    })
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def stamp(value):
    if not value or value.startswith('0001-'):
        return None
    return datetime.fromisoformat(value.replace('Z', '+00:00'))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('pr', type=int)
    parser.add_argument('--json', type=Path, help='save raw evidence outside the repository')
    args = parser.parse_args()
    pr = get(f'pulls/{args.pr}')
    if not pr.get('merged') or not stamp(pr.get('merged_at')):
        parser.error('PR has not merged; the observation window has not started')
    since = stamp(pr['merged_at'])
    now = datetime.now(timezone.utc)
    until = min(since + timedelta(hours=24), now)
    runs = []
    page = 1
    while True:
        batch = get(f'actions/runs?workflow_id=fast.yml&limit=50&page={page}')['workflow_runs']
        if any(run['workflow_id'] != 'fast.yml' for run in batch):
            raise RuntimeError('API did not honor workflow_id filter')
        runs.extend(run for run in batch if since <= stamp(run['created']) <= until)
        if len(batch) < 50 or any(stamp(run['created']) < since for run in batch):
            break
        page += 1
    evidence = dict(pr=args.pr, merged_sha=pr['merge_commit_sha'], since=since.isoformat(),
                    observed_until=until.isoformat(), captured_at=now.isoformat(), workflow_runs=runs)
    if args.json:
        args.json.write_text(json.dumps(evidence, indent=2) + '\n')
    print(f"Window: {since.isoformat()} .. {until.isoformat()}; "
          f"{'full 24 h' if until >= since + timedelta(hours=24) else 'partial observation'}")
    observed = []
    for run in runs:
        started, stopped = stamp(run.get('started')), stamp(run.get('stopped'))
        if started and started <= until:
            elapsed = (min(stopped or until, until) - started).total_seconds()
            observed.append((elapsed, run, bool(stopped and stopped <= until)))
    if not observed:
        print('No started fast.yml run in this window; no runtime verdict.')
        return
    elapsed, longest, finished = max(observed, key=lambda row: row[0])
    queued = (min(stamp(longest.get('stopped')) or until, until) - stamp(longest['created'])).total_seconds()
    print(f"Longest execution span: {elapsed:.0f} s ({elapsed/60:.2f} min), "
          f"{'finished' if finished else 'lower bound, unfinished at window end'}; "
          f"incident baseline 2700 s (45 min); status={longest['status']}")
    print(f"Creation-to-stop/observation: {queued:.0f} s; runs={len(runs)}; "
          f"unfinished={sum(not stamp(run.get('stopped')) for run in runs)}")
    print(longest['html_url'])


if __name__ == '__main__':
    main()
