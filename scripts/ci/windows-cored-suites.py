#!/usr/bin/env python3
"""Run the okilum-cored CLI and MCP integration suites natively on Windows (#2060).

Each suite is compiled, discovered and run on its own, with:
- a positive discovery control: the exact test count and named controls must be
  listed before anything runs, and libtest must report that many passes with
  nothing ignored or filtered out (a suite can never pass by running nothing);
- budget accounting: wall time of the build and of the run, free space on the
  target and Cargo-home drives, and the size of target/ and the Cargo home
  before and after each phase. Tree sizes are summed under a time and entry
  budget so the accounting itself cannot stall the job;
- a preflight floor of free disk and a clear ENOSPC diagnostic naming the suite,
  phase and the largest target/ directories when the build runs out of disk.
"""
import collections
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import threading
import time


ROOT = Path(__file__).resolve().parents[2]
PACKAGE = "okilum-cored"
GIB = 1024 ** 3

# Exact counts on Windows. cli.rs carries 11 portable tests plus one
# cfg(not(unix)) control that the Brain runtime refuses before writing state.
SUITES = {
    "cli": {"count": 12, "controls": (
        "strays_reports_only_entries_outside_the_vault_notes",
        "explicit_values_index_and_override_environment",
        "unsupported_brain_refuses_before_creating_state_but_reader_still_works")},
    "mcp": {"count": 7, "controls": (
        "handshake_then_tools_list",
        "a_notification_gets_no_reply_and_the_next_call_still_works")},
}

# Measured on windows-2022 (see #2060): both suites together add well under the
# floor below to target/. The floor stops a doomed build up front with a disk
# message instead of a linker error several minutes later.
MIN_FREE_GIB = 6.0
ACCOUNTING_SECONDS = 45
ACCOUNTING_ENTRIES = 1_500_000

# Linux ENOSPC, Rust io::Error on Windows (ERROR_DISK_FULL), the Windows
# message text, and the MSVC linker's insufficient-disk error.
ENOSPC = re.compile(r"No space left on device|os error (?:28|112)\b|not enough space on the disk"
                    r"|LNK1180", re.IGNORECASE)
SUMMARY = re.compile(r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; "
                     r"(\d+) measured; (\d+) filtered out")


class SuiteFailure(RuntimeError):
    pass


def log(line):
    print(line, flush=True)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write(line + "\n\n")


def tree_bytes(root, seconds=ACCOUNTING_SECONDS, entries=ACCOUNTING_ENTRIES):
    """Sum regular file sizes under root within a time/entry budget.

    Returns (bytes, complete). Symlinks and junctions are not followed.
    """
    total, seen = 0, 0
    deadline = time.monotonic() + seconds
    pending = [Path(root)]
    while pending:
        if seen >= entries or time.monotonic() > deadline:
            return total, False
        try:
            scan = os.scandir(pending.pop())
        except OSError:
            continue
        with scan:
            for entry in scan:
                seen += 1
                try:
                    if entry.is_dir(follow_symlinks=False):
                        if not getattr(entry, "is_junction", lambda: False)():
                            pending.append(Path(entry.path))
                    elif entry.is_file(follow_symlinks=False):
                        total += entry.stat(follow_symlinks=False).st_size
                except OSError:
                    continue
    return total, True


def cargo_home():
    return Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")


def gib(value):
    return f"{value / GIB:.2f}"


def snapshot(label):
    target = ROOT / "target"
    sizes = {}
    for name, path in (("target", target), ("cargo_home", cargo_home())):
        size, complete = tree_bytes(path) if path.exists() else (0, True)
        sizes[name] = (size, complete)
    free = shutil.disk_usage(target if target.exists() else ROOT).free
    home_free = shutil.disk_usage(cargo_home() if cargo_home().exists() else Path.home()).free
    state = {"label": label, "free": free, "home_free": home_free,
             "target": sizes["target"][0], "cargo_home": sizes["cargo_home"][0],
             "complete": sizes["target"][1] and sizes["cargo_home"][1]}
    partial = "" if state["complete"] else " (partial: accounting budget reached)"
    log(f"DISK {label}: free={gib(free)} GiB (cargo home drive {gib(home_free)} GiB) "
        f"target={gib(state['target'])} GiB cargo_home={gib(state['cargo_home'])} GiB{partial}")
    return state


def largest_target_dirs(limit=8):
    debug = ROOT / "target" / "debug"
    rows = []
    if debug.is_dir():
        for child in debug.iterdir():
            if child.is_dir():
                rows.append((tree_bytes(child, seconds=10)[0], child.name))
    return sorted(rows, reverse=True)[:limit]


def tee(command, *, capture_stdout):
    """Stream the command's output and keep a bounded tail for diagnostics.

    With capture_stdout, stdout is returned whole (Cargo JSON) and only stderr
    is streamed; otherwise both are streamed together.
    """
    process = subprocess.Popen(
        command, cwd=ROOT, text=True, encoding="utf-8", errors="replace",
        stdout=subprocess.PIPE, stderr=subprocess.PIPE if capture_stdout else subprocess.STDOUT)
    tail = collections.deque(maxlen=400)
    disk_full = []

    def pump(stream):
        for line in stream:
            print(line, end="", flush=True)
            tail.append(line)
            if ENOSPC.search(line):
                disk_full.append(line.strip())

    if capture_stdout:
        reader = threading.Thread(target=pump, args=(process.stderr,))
        reader.start()
        stdout = process.stdout.read()
        reader.join()
        if ENOSPC.search(stdout):
            disk_full.append("in Cargo JSON output")
    else:
        pump(process.stdout)
        stdout = "".join(tail)
    return process.wait(), stdout, disk_full


def fail_disk(suite, phase, evidence):
    state = snapshot(f"{suite} {phase} ENOSPC")
    log(f"::error::No space left on device: okilum-cored {suite} {phase} ran out of disk "
        f"(free {gib(state['free'])} GiB, target {gib(state['target'])} GiB). "
        f"First disk error: {evidence[0][:300]}")
    for size, name in largest_target_dirs():
        log(f"  target/debug/{name}: {gib(size)} GiB")
    raise SuiteFailure(f"{suite} {phase}: disk exhausted")


def build(suite):
    command = ["cargo", "test", "--locked", "-p", PACKAGE, "--test", suite, "--no-run",
               "--message-format=json-render-diagnostics"]
    code, stdout, disk_full = tee(command, capture_stdout=True)
    if disk_full:
        fail_disk(suite, "build", disk_full)
    if code:
        raise SuiteFailure(f"{suite} build failed ({code})")
    executables = []
    for line in stdout.splitlines():
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if (message.get("reason") == "compiler-artifact" and message.get("executable")
                and message.get("profile", {}).get("test") and message["target"]["name"] == suite):
            executables.append(message["executable"])
    if len(executables) != 1:
        raise SuiteFailure(f"{suite}: expected one test executable, got {executables}")
    return executables[0]


def discover(suite, executable):
    listing = subprocess.run([executable, "--list"], cwd=ROOT, text=True, encoding="utf-8",
                             errors="replace", stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             check=False)
    if listing.returncode:
        raise SuiteFailure(f"{suite}: --list failed ({listing.returncode}): {listing.stdout[-2000:]}")
    names = [line[:-len(": test")] for line in listing.stdout.splitlines() if line.endswith(": test")]
    expected = SUITES[suite]
    missing = [name for name in expected["controls"] if name not in names]
    if len(names) != expected["count"] or missing:
        raise SuiteFailure(
            f"{suite}: discovered {len(names)} tests, expected exactly {expected['count']}; "
            f"missing controls {missing}; listed {names}. Update SUITES in "
            f"scripts/ci/windows-cored-suites.py when tests are added or removed on purpose.")
    log(f"DISCOVERED {suite}: {len(names)} tests including {', '.join(expected['controls'])}")
    return names


def execute(suite, executable):
    code, output, disk_full = tee([executable, "--nocapture", "--test-threads=1"],
                                  capture_stdout=False)
    if disk_full:
        fail_disk(suite, "run", disk_full)
    verdicts = [SUMMARY.match(line.strip()) for line in output.splitlines()]
    verdicts = [match for match in verdicts if match]
    if code:
        raise SuiteFailure(f"{suite} failed ({code})")
    count = SUITES[suite]["count"]
    final = verdicts[-1] if verdicts else None
    if (final is None or final[1] != "ok" or int(final[2]) != count
            or any(int(final[index]) for index in (3, 4, 6))):
        raise SuiteFailure(f"{suite}: libtest must report {count} passed with none failed, "
                           f"ignored or filtered: {final[0] if final else 'no summary'}")


def main():
    rows = []
    baseline = snapshot("before cored suites")
    if baseline["free"] < MIN_FREE_GIB * GIB:
        log(f"::error::No space left on device: only {gib(baseline['free'])} GiB free before the "
            f"okilum-cored suites; at least {MIN_FREE_GIB} GiB is required (#2060)")
        raise SuiteFailure("insufficient free disk before the okilum-cored suites")
    before = baseline
    for suite in SUITES:
        started = time.monotonic()
        executable = build(suite)
        built = time.monotonic()
        discover(suite, executable)
        execute(suite, executable)
        finished = time.monotonic()
        after = snapshot(f"after {suite}")
        rows.append((suite, built - started, finished - built,
                     after["target"] - before["target"], after["free"] - before["free"]))
        before = after
    log("| suite | build s | run s | target delta GiB | free delta GiB |")
    log("|---|---|---|---|---|")
    for suite, build_s, run_s, target_delta, free_delta in rows:
        log(f"| {suite} | {build_s:.0f} | {run_s:.0f} | {gib(target_delta)} | {gib(free_delta)} |")
    total = sum(row[1] + row[2] for row in rows)
    log(f"PASS okilum-cored Windows CLI and MCP suites in {total / 60:.1f} min; "
        f"target grew {gib(before['target'] - baseline['target'])} GiB")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except SuiteFailure as error:
        print(f"::error::{error}", flush=True)
        sys.exit(1)
