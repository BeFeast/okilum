#!/usr/bin/env python3
"""Run pinned managed fixtures in independent, concurrent processes; no retries."""
import argparse
import json
import os
import runpy
import signal
from pathlib import Path
import subprocess
import tempfile
import time


lifecycle = runpy.run_path(str(Path(__file__).with_name("sync-fixture-lifecycle.py")))
check = lifecycle["check"]

TESTS = {
    "managed_real_conflicts_resolve_each_choice_with_hashes",
    "managed_source_share_empty_receive_and_bidirectional_hashes",
    "managed_source_skipped_share_negative_control",
    "managed_source_nonempty_destination_refused_without_writes",
    "managed_peer_addresses_without_discovery_negative_then_hashes_and_restart",
    "managed_receiver_catalog_name_and_7504_label_reconcile",
    "managed_receiver_meshes_through_its_share_peer_introducer",
    "remote_inventory::managed_remote_inventory_exact_plan_binary_chunks_staleness_and_zero_writes",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=Path)
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--parallel", type=int, default=1)
    args = parser.parse_args()
    if not 1 <= args.parallel <= args.runs <= 100:
        parser.error("require 1 <= parallel <= runs <= 100")
    evidence = Path(tempfile.mkdtemp(prefix="sync-source-sharing-", dir=os.environ["TMPDIR"]))
    print(f"Fixture evidence: {evidence}", flush=True)
    lifecycle["subreaper"]()
    def interrupted(signum, _frame):
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, interrupted)
    processes = []
    try:
        return run_trials(args, evidence, processes)
    finally:
        # Cancellation must not leave sibling trials or their fixture daemons.
        for process in processes:
            if process.poll() is None:
                process.kill()
        for process in processes:
            process.wait()
        check(evidence, cleanup=True)
        lifecycle["reap"]()


def run_trials(args, evidence, processes):
    trials = []
    failed = False
    for start in range(0, args.runs, args.parallel):
        batch = []
        for number in range(start + 1, min(start + args.parallel, args.runs) + 1):
            work = evidence / f"run-{number}"
            work.mkdir()
            log = evidence / f"run-{number}.log"
            env = dict(os.environ, RUST_BACKTRACE="1", TMPDIR=str(work))
            with log.open("w") as output:
                process = subprocess.Popen(
                    [str(args.executable.resolve()), "source_sharing::", "--ignored",
                     "--nocapture", "--test-threads=1",
                     "--skip", "source_sharing::fixture_lifecycle_control"],
                    env=env, stdout=output, stderr=subprocess.STDOUT,
                    preexec_fn=lifecycle["parent_death_signal"](),
                )
            processes.append(process)
            trial = {"run": number, "pid": process.pid, "started": time.time(), "log": str(log)}
            batch.append((process, trial))
        # Launch the entire batch before waiting: no shared Cargo build lock can
        # turn the requested concurrency into a sequence of isolated successes.
        for process, trial in batch:
            trial["exit"] = process.wait()
            trial["finished"] = time.time()
            output = Path(trial["log"]).read_text()
            passed = {name for name in TESTS if f"test source_sharing::{name} ... ok" in output.splitlines()}
            trial["passed"] = len(passed)
            trial["ok"] = trial["exit"] == 0 and passed == TESTS and f"test result: ok. {len(TESTS)} passed;" in output
            failed |= not trial["ok"]
            trials.append(trial)
            print(f"Run {trial['run']}: exit={trial['exit']} passed={len(passed)}/{len(TESTS)}", flush=True)
            if not trial["ok"]:
                print(output, flush=True)
    (evidence / "results.json").write_text(json.dumps(trials, indent=2) + "\n")
    failed |= bool(check(evidence))
    lifecycle["reap"]()
    print(f"Completed {args.runs} runs, parallel={args.parallel}: {'FAIL' if failed else 'PASS'}", flush=True)
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
