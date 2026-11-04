#!/usr/bin/env python3
"""Repeat the real core live fixtures with parallel processes and CPU contention.

Run an already-built okilum-core libtest binary on a build host, never maestro.
Each successful process must execute the complete live fixture set. The watchdog
bounds stuck tests; fixture readiness must come from completion notifications.
"""

import argparse
import concurrent.futures
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--iterations", type=int, default=50)
    parser.add_argument("--load-workers", type=int, default=2)
    parser.add_argument("--cpus", type=int, default=2)
    parser.add_argument("--watchdog", type=int, default=300)
    args = parser.parse_args()
    if min(args.workers, args.iterations, args.cpus, args.watchdog) < 1:
        parser.error("workers, iterations, cpus and watchdog must be positive")
    if args.load_workers < 0:
        parser.error("load-workers must be nonnegative")
    args.output.mkdir(parents=True, exist_ok=False)
    binary = str(args.binary.resolve(strict=True))
    cpus = sorted(os.sched_getaffinity(0))[: args.cpus]
    os.sched_setaffinity(0, cpus)
    command = [binary, "remote_log::live::tests::", "--test-threads=4", "--nocapture"]
    listing = subprocess.check_output([binary, "--list"], text=True)
    expected = re.findall(r"^(remote_log::live::tests::\S+): test$", listing, re.M)
    if not expected:
        raise RuntimeError("positive control: no live fixtures in this binary")
    started = time.monotonic()
    load = []
    results = []

    def run_worker(worker):
        runs = []
        for iteration in range(args.iterations):
            began = time.monotonic()
            try:
                process = subprocess.run(
                    command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                    text=True, timeout=args.watchdog, check=False,
                )
                output, code = process.stdout, process.returncode
            except subprocess.TimeoutExpired as error:
                output = (error.stdout or b"").decode(errors="replace")
                output += "\nPROCESS WATCHDOG EXPIRED\n"
                code = 124
            log = f"worker-{worker:02}-{iteration:03}.log"
            (args.output / log).write_text(output)
            passed = re.findall(r"^test (remote_log::live::tests::\S+) \.\.\. ok$", output, re.M)
            ok = code == 0 and sorted(passed) == sorted(expected)
            record = dict(worker=worker, iteration=iteration, exit_code=code,
                          ok=ok, passed=passed, log=log,
                          seconds=round(time.monotonic() - began, 3))
            runs.append(record)
            print(json.dumps({k: v for k, v in record.items() if k != "passed"}), flush=True)
            if not ok:
                break
        return runs

    try:
        for _ in range(args.load_workers):
            load.append(subprocess.Popen([sys.executable, "-c", "while True: pass"]))
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
            for runs in pool.map(run_worker, range(args.workers)):
                results.extend(runs)
    finally:
        for process in load:
            process.terminate()
        for process in load:
            process.wait()
    counts = {name: sum(name in run["passed"] for run in results) for name in expected}
    ok = (len(results) == args.workers * args.iterations
          and all(run["ok"] for run in results))
    summary = dict(ok=ok, command=command, affinity=cpus,
                   workers=args.workers, iterations=args.iterations,
                   load_workers=args.load_workers, watchdog_seconds=args.watchdog,
                   seconds=round(time.monotonic() - started, 3),
                   counts=counts, runs=results)
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({k: v for k, v in summary.items() if k != "runs"}), flush=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
