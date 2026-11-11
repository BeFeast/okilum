#!/usr/bin/env python3
"""Run the complete workspace census with resource-sensitive cases isolated."""
import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

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
    footer = None
    for line in process.stdout:
        print(line, end="", flush=True)
        text = ANSI.sub("", line).strip()
        # stderr is merged into stdout BEFORE Cargo starts, so these target
        # boundaries and harness footers share one ordered pipe. Fixtures may
        # launch nested harnesses; only the final parent footer counts.
        if text.startswith(("Running unittests ", "Running tests/", "Doc-tests ")):
            if footer is not None:
                observed += sum(footer)
                completed += sum(footer[:2])
            footer = None
        match = SUMMARY.match(text)
        if match:
            footer = tuple(map(int, match.groups()))
    if footer is not None:
        observed += sum(footer)
        completed += sum(footer[:2])
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
    # Separate processes retain separate Log pools. Overlap its serial I/O
    # fixtures with ordinary tests, after inventory has compiled every target.
    # Reveal stays alone so its real D-Bus deadlines do not compete with either.
    with ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(lambda part: run(*part), parts[:2]))
    results.append(run(*parts[2]))
    passed = all(results)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
