#!/usr/bin/env python3
"""Run the complete workspace census with resource-sensitive cases isolated."""
import re
import subprocess
import sys

COMMAND = ["cargo", "test", "--locked", "-p", "okilum-core", "-p", "okilum-shell",
           "-p", "okilum-sync", "-p", "okilum-t3", "--no-fail-fast", "--"]
LOG = "reader_log"
REVEAL = "platform::reveal::tests::"
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
SUMMARY = re.compile(r"^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;")


def run(name, arguments, expected):
    print(f"WORKSPACE_CENSUS_START part={name} expected={expected}", flush=True)
    process = subprocess.Popen(COMMAND + arguments, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, text=True, bufsize=1)
    observed = 0
    completed = 0
    for line in process.stdout:
        print(line, end="", flush=True)
        text = ANSI.sub("", line).strip()
        match = SUMMARY.match(text)
        if match:
            passed, failed, ignored = map(int, match.groups())
            observed += passed + failed + ignored
            completed += passed + failed
    status = process.wait()
    print(f"WORKSPACE_CENSUS_END part={name} expected={expected} observed={observed} completed={completed} status={status}",
          flush=True)
    return status == 0 and observed == expected and completed > 0


def main():
    # Keep the same four packages in EVERY invocation: Cargo feature unification,
    # test/doc targets, working directories and runtime environment stay intact.
    inventory = subprocess.run(COMMAND + ["--list"], capture_output=True, text=True)
    if inventory.returncode:
        print(inventory.stdout, end="")
        print(inventory.stderr, end="", file=sys.stderr)
        return 1
    names = [line[:-6] for raw in inventory.stdout.splitlines()
             if (line := ANSI.sub("", raw).strip()).endswith(": test")]
    assert names, "workspace test inventory must not be empty"
    assert not any(LOG in name and REVEAL in name for name in names), "resource groups must be disjoint"
    logs = sum(LOG in name for name in names)
    reveals = sum(REVEAL in name for name in names)
    ordinary = len(names) - logs - reveals
    assert ordinary > 0 and logs > 0 and reveals > 0, "every measured group must fire"
    parts = [
        ("ordinary", ["--nocapture", "--test-threads=4", "--skip", LOG, "--skip", REVEAL], ordinary),
        ("logs", [LOG, "--nocapture", "--test-threads=1"], logs),
        ("reveal", [REVEAL, "--nocapture", "--test-threads=1"], reveals),
    ]
    print(f"WORKSPACE_CENSUS total={len(names)} ordinary={ordinary} logs={logs} reveal={reveals}",
          flush=True)
    passed = True
    for name, arguments, expected in parts:
        passed = run(name, arguments, expected) and passed
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
