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
    "managed_remove_2042_plain_introducer_handoff_refused_without_host_authority",
    "managed_remove_2042_remote_request_preserves_unrequested_association",
    "managed_remove_2042_reaches_plain_introducer_peer_without_touching_other_folders",
    "managed_remove_2042_from_receiver_propagates_and_keeps_files",
    "managed_remove_2042_introducer_replays_after_lost_reply",
    "managed_shared_2046_include_propagation_local_review_and_initial_seed",
    "managed_create_2569_artifacts_and_guarded_existing_policy_migration",
    "managed_history_2045_broken_folder_warns_while_other_vault_receives",
    "managed_history_2045_restore_retention_and_migration",
    "managed_history_2568_remote_rename_recovery_explains_duplicate",
    "managed_real_conflicts_resolve_each_choice_with_hashes",
    "managed_source_share_empty_receive_and_bidirectional_hashes",
    "managed_source_skipped_share_negative_control",
    "managed_source_nonempty_destination_refused_without_writes",
    "managed_peer_addresses_without_discovery_negative_then_hashes_and_restart",
    "managed_receiver_catalog_name_and_7504_label_reconcile",
    "managed_receiver_meshes_through_its_share_peer_introducer",
    "remote_inventory::managed_remote_inventory_exact_plan_binary_chunks_staleness_and_zero_writes",
}
# #2567: archive stamps are local wall times. The suite runs in UTC; the
# retention/history scenario runs again with both daemons and the reader in a
# DST zone, against the same pinned runtime.
ZONE_TEST = "managed_history_2045_restore_retention_and_migration"
ZONE = "Asia/Jerusalem"


def zone_passed(exit_code, output):
    """The DST-zone trial ran exactly the retention test, in that zone."""
    lines = output.splitlines()
    return (exit_code == 0
            and f"test source_sharing::{ZONE_TEST} ... ok" in lines
            and "test result: ok. 1 passed;" in output
            and ("#2567 retention control timezone: IST +0200" in output
                 or "#2567 retention control timezone: IDT +0300" in output))


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
            def launch(name, zone, selection):
                work = evidence / name
                work.mkdir()
                log = evidence / f"{name}.log"
                env = dict(os.environ, RUST_BACKTRACE="1", TMPDIR=str(work), TZ=zone)
                with log.open("w") as output:
                    process = subprocess.Popen(
                        [str(args.executable.resolve()), *selection, "--ignored",
                         "--nocapture", "--test-threads=1"],
                        env=env, stdout=output, stderr=subprocess.STDOUT,
                        preexec_fn=lifecycle["parent_death_signal"](),
                    )
                processes.append(process)
                return process, log
            process, log = launch(f"run-{number}", "UTC", [
                "source_sharing::", "--skip", "source_sharing::fixture_lifecycle_control"])
            zone, zone_log = launch(f"run-{number}-zone", ZONE, [
                f"source_sharing::{ZONE_TEST}", "--exact"])
            trial = {"run": number, "pid": process.pid, "started": time.time(), "log": str(log),
                     "zone_pid": zone.pid, "zone_log": str(zone_log)}
            batch.append((process, zone, trial))
        # Launch the entire batch before waiting: no shared Cargo build lock can
        # turn the requested concurrency into a sequence of isolated successes.
        for process, zone, trial in batch:
            trial["exit"] = process.wait()
            trial["zone_exit"] = zone.wait()
            trial["finished"] = time.time()
            output = Path(trial["log"]).read_text()
            zone_output = Path(trial["zone_log"]).read_text()
            passed = {name for name in TESTS if f"test source_sharing::{name} ... ok" in output.splitlines()}
            trial["passed"] = len(passed)
            trial["zone_ok"] = zone_passed(trial["zone_exit"], zone_output)
            trial["ok"] = (trial["exit"] == 0 and passed == TESTS
                           and f"test result: ok. {len(TESTS)} passed;" in output and trial["zone_ok"])
            failed |= not trial["ok"]
            trials.append(trial)
            print(f"Run {trial['run']}: exit={trial['exit']} passed={len(passed)}/{len(TESTS)}; "
                  f"{ZONE} exit={trial['zone_exit']} ok={trial['zone_ok']}", flush=True)
            if trial["ok"]:
                print("#2045/#2568 passed: pinned two-runtime delete/restore and rename-recovery hashes, overwrite/stale guards, managed migrations, external preservation and time-shifted retention controls", flush=True)
                print(f"#2567 passed: retention order/age and DST ambiguity controls in UTC and {ZONE}", flush=True)
            if not trial["ok"]:
                print(output, flush=True)
                print(zone_output, flush=True)
    (evidence / "results.json").write_text(json.dumps(trials, indent=2) + "\n")
    failed |= bool(check(evidence))
    lifecycle["reap"]()
    print(f"Completed {args.runs} runs, parallel={args.parallel}: {'FAIL' if failed else 'PASS'}", flush=True)
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
