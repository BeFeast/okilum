#!/usr/bin/env python3
"""Same-runner red/green control; temporary evidence branch only."""
import concurrent.futures
import json
import os
import re
from pathlib import Path
import shutil
import subprocess

BASE = 'dd0260c3ac910794dc0199eb7d6de80e8103dcde'
TESTS = Path('crates/okilum-shell/src/reader_log_remote/investigation_tests.rs')
PRODUCT = Path('crates/okilum-shell/src/reader_log.rs')
NAME = 'reader_log::remote::investigation_tests::request_details_back_live_and_saved_view_use_fake_server_and_keep_investigation'
REGRESSION = 'reader_log::remote::investigation_tests::remote_replacement_restarts_an_inflight_investigation_filter'
OUT = Path('investigation-stress-evidence')
OUT.mkdir(exist_ok=True)

def build(name):
    r = subprocess.run(['cargo', 'test', '--offline', '--locked', '-p', 'okilum-shell', '--bin', 'okilum', '--no-run', '--message-format=json'], text=True, stdout=subprocess.PIPE, check=True)
    binaries = [x['executable'] for line in r.stdout.splitlines() if (x := json.loads(line)).get('reason') == 'compiler-artifact' and x.get('executable') and x.get('profile', {}).get('test')]
    assert len(binaries) == 1, binaries
    dest = OUT / name
    shutil.copy2(binaries[0], dest)
    return str(dest.resolve())

def run(binary, test, name, seed=0, iterations=1):
    env = dict(os.environ, ITERATIONS=str(iterations), SEED=str(seed))
    env.pop('SCHEDULER_NONINTERACTIVE', None)
    log = OUT / (name + '.log')
    with log.open('w') as f:
        r = subprocess.run([binary, test, '--exact', '--nocapture'], stdout=f, stderr=subprocess.STDOUT, env=env, timeout=600)
    data = log.read_text(errors='replace')
    result = dict(name=name, code=r.returncode, seed=seed, requested=iterations, started=len(re.findall(r'^seed = [0-9]+$', data, re.MULTILINE)) or int('running 1 test' in data))
    print(json.dumps(result), flush=True)
    if r.returncode:
        print(data[-7000:], flush=True)
    return result

def stress(binary, name):
    # Pinned GPUI 0.3.3 runs the explicit SEED once, then SEED..SEED+ITERATIONS.
    # Thus each worker proves 51 executions (200 distinct seeds overall).
    # Four independent test processes compete for the hosted runner's four CPUs.
    burners = [subprocess.Popen(['python3', '-c', 'while True: pass']) for _ in range(2)]
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            return list(pool.map(lambda i: run(binary, NAME, f'{name}-{i}', i * 50, 50), range(4)))
    finally:
        for process in burners:
            process.terminate()
        for process in burners:
            process.wait()


fixed = {p: p.read_bytes() for p in (TESTS, PRODUCT)}
# Preserve the new deterministic regression, but restore the old end-to-end
# harness and product. Both binaries run on this exact runner, under one harness.
regression = fixed[TESTS].decode().split('// Force the worker/table race')[1].split('#[gpui::test]\nfn request_labels')[0]
subprocess.run(['git', 'fetch', 'https://github.com/BeFeast/okilum.git', BASE, '--depth=1'], check=True)
try:
    for p in fixed:
        p.write_bytes(subprocess.check_output(['git', 'show', f'{BASE}:{p}']))
    with TESTS.open('a') as f:
        f.write('\n// Force the worker/table race' + regression)
    old = build('old-product-and-harness')
finally:
    for p, data in fixed.items():
        p.write_bytes(data)
red = run(old, REGRESSION, 'regression-red')
assert red['code'] != 0
assert 'replacement must own a runnable filter' in (OUT / 'regression-red.log').read_text()
old_results = stress(old, 'old')
new = build('fixed')
green = run(new, REGRESSION, 'regression-green', iterations=200)
new_results = stress(new, 'fixed')
summary = dict(cpu_load_workers=2, parallel_test_workers=4, base=BASE, head=subprocess.check_output(['git','rev-parse','HEAD'], text=True).strip(), regression_red=red, regression_green=green, old=old_results, fixed=new_results)
(OUT / 'summary.json').write_text(json.dumps(summary, indent=2))
print(json.dumps(summary, indent=2), flush=True)
assert green['code'] == 0 and green['started'] == 201
assert all(r['code'] == 0 and r['started'] == 51 for r in new_results)
# Keep logs/summary as the artifact; avoid uploading multi-hundred-MB binaries.
(OUT / 'old-product-and-harness').unlink()
(OUT / 'fixed').unlink()
