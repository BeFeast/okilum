#!/usr/bin/env python3
"""Merge the build-slot shard receipts of one pool gate into one phase receipt.

Each shard directory holds `phases.json` (pool-phase-lib.sh), `build-slot.json`
(build-slot --receipt) and `rc` (build-slot's exit code). A shard is

- pass:  rc 0, no OOM kill, a complete receipt whose phases all passed;
- fail:  no OOM kill, a receipt with a failing phase, rc 1 (or rc 124 when that
         phase hit its own deadline): a code verdict;
- infra: everything else (queue timeout, ssh, OOM kill, missing receipt).

The gate is a verdict when any shard failed (code) or every shard passed;
otherwise it is infrastructure and the dispatch step fails. The merged receipt
keeps every record with its shard name; pool-phase.sh passes a phase only when
every record of that name passed.
"""
import argparse
import json
import sys
from pathlib import Path

COMPLETE = "Pool shard complete"


def load(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def shard_state(directory):
    directory = Path(directory)
    phases = load(directory / "phases.json")
    slot = load(directory / "build-slot.json") or {}
    try:
        rc = int((directory / "rc").read_text().strip())
    except (OSError, ValueError):
        rc = None
    oom = int(slot.get("oom_kill") or 0) if isinstance(slot, dict) else 0
    if not isinstance(phases, list) or not all(isinstance(p, dict) for p in phases):
        return "infra", [], rc, slot
    failed = any(int(p.get("code") or 0) for p in phases if p.get("name") != COMPLETE)
    complete = any(p.get("name") == COMPLETE and int(p.get("code") or 0) == 0 for p in phases)
    if oom:
        return "infra", phases, rc, slot
    if rc == 0 and complete and not failed:
        return "pass", phases, rc, slot
    if failed and rc in (1, 124):
        return "fail", phases, rc, slot
    return "infra", phases, rc, slot


def aggregate(directories):
    shards, merged = [], []
    for directory in directories:
        name = Path(directory).name
        state, phases, rc, slot = shard_state(directory)
        shards.append({"shard": name, "state": state, "rc": rc,
                       "host": slot.get("host") if isinstance(slot, dict) else None,
                       "waited_s": slot.get("queue_waited_s") if isinstance(slot, dict) else None,
                       "ran_s": slot.get("ran_s") if isinstance(slot, dict) else None,
                       "memory_peak": slot.get("memory_peak") if isinstance(slot, dict) else None})
        merged += [{**phase, "shard": name} for phase in phases if phase.get("name") != COMPLETE]
    states = {shard["state"] for shard in shards}
    if not shards:
        kind = "infra"
    elif "fail" in states or states == {"pass"}:
        kind = "verdict"
    else:
        kind = "infra"
    return kind, merged, shards


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, help="merged phases.json")
    parser.add_argument("shards", nargs="+", help="shard directories")
    args = parser.parse_args()
    kind, merged, shards = aggregate(args.shards)
    if kind == "verdict":
        Path(args.out).write_text(json.dumps(merged) + "\n")
    for shard in shards:
        print("POOL SHARD {shard} state={state} rc={rc} host={host} waited={waited_s}s ran={ran_s}s "
              "memory_peak={memory_peak}".format(**shard), flush=True)
    print(f"kind={kind}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
