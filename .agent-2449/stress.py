#!/usr/bin/env python3
"""Repeat the complete cache module with default libtest threads and process load."""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import sys
import time

binary, output, label = sys.argv[1:4]
runs = int(os.environ.get('STRESS_RUNS', '200'))
jobs = int(os.environ.get('STRESS_JOBS', '4'))
out = Path(output) / label
out.mkdir(parents=True, exist_ok=True)
name = 'reader_cache::tests::concurrent_checkpoints_never_lose_the_persisted_generation'

def run(n):
    started = time.monotonic()
    try:
        p = subprocess.run([binary, 'reader_cache::tests::', '--nocapture'],
                           stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                           text=True, timeout=120)
        log = p.stdout
        ok = p.returncode == 0 and f'test {name} ... ok' in log
        result = {'run': n, 'passed': ok, 'exit': p.returncode,
                  'seconds': round(time.monotonic() - started, 3)}
    except subprocess.TimeoutExpired as error:
        log = str(error)
        result = {'run': n, 'passed': False, 'exit': 'timeout'}
    (out / f'{n:04}.log').write_text(log)
    if not result['passed']:
        print(json.dumps(result), flush=True)
        print(log[-6000:], flush=True)
    return result

with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
    results = list(pool.map(run, range(1, runs + 1)))
summary = {'label': label, 'runs': runs, 'jobs': jobs, 'libtest_threads': 'default',
           'passed': sum(r['passed'] for r in results),
           'failed': sum(not r['passed'] for r in results),
           'cpus': os.cpu_count(), 'results': results}
(out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps({k: v for k, v in summary.items() if k != 'results'}), flush=True)
sys.exit(bool(summary['failed']))
