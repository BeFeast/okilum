#!/usr/bin/env python3
"""Run the complete workspace census with resource-sensitive cases isolated."""
import re
import hashlib
import os
from pathlib import Path
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

COMMAND = ["cargo", "test", "--locked", "-p", "okilum-core", "-p", "okilum-shell",
           "-p", "okilum-sync", "-p", "okilum-t3", "--no-fail-fast", "--"]
LOG = "reader_log"
REVEAL = "platform::reveal::tests::"
NATIVE_FRAME = "reader_editor::code_operations::tests::edit2803_unwrapped_popup_frames_reuse_shapes_and_invalidate_on_edit"
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
SUMMARY = re.compile(r"^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;")


def run(name, arguments, expected, *, command=None, limit=1320):
    print(f"WORKSPACE_CENSUS_START part={name} expected={expected}", flush=True)
    # Unrelated fixture shards must not serialize against, or repeatedly scan,
    # the same ever-growing account-wide authority registry. All descendants
    # within one shard still inherit the SAME account and real writer fences.
    supervisor = [sys.executable, str(Path(__file__).with_name('test-group.py')),
                  '--isolated-writer-account', f'workspace-{name}', str(limit)]
    process = subprocess.Popen(supervisor + (command or COMMAND) + arguments, stdout=subprocess.PIPE,
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


def main(*, shared=False):
    shard_count = os.environ.get('OKILUM_WORKSPACE_ORDINARY_SHARDS', '2')
    if shard_count not in ('2', '4'):
        print('ordinary workspace shards must be 2 (hosted) or 4 (heavy)', file=sys.stderr)
        return 2
    shard_count = int(shard_count)
    # Keep identical package selection in the census and EVERY shard. Shared
    # retains --workspace and its existing platform/Log exclusions; its caller
    # runs the complete Log selection separately afterwards.
    command = (["cargo", "test", "--workspace", "--locked", "--no-fail-fast", "--"]
               if shared else COMMAND)
    skips = ["--skip", "platform::", "--skip", LOG] if shared else []
    # Keep Cargo's target headers and libtest's names in one ordered stream.
    # AppImage fixtures fork/exec while holding installation locks, so a
    # sibling test in the same process can inherit those FDs before exec.
    inventory = subprocess.run(command + ["--list", *skips], stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, text=True)
    if inventory.returncode:
        print(inventory.stdout, end="")
        return 1
    names = []
    serial_names = set()
    appimage = False
    for raw in inventory.stdout.splitlines():
        line = ANSI.sub("", raw).strip()
        if line.startswith(("Running unittests ", "Running tests/", "Doc-tests ")):
            appimage = bool(re.search(r'/okilum_appimage-[0-9a-f]+\)', line))
        elif line.endswith(": test"):
            name = line[:-6]
            names.append(name)
            if appimage:
                serial_names.add(name)
    assert names, "workspace test inventory must not be empty"
    assert not any(LOG in name and REVEAL in name for name in names), "resource groups must be disjoint"
    logs = sum(LOG in name for name in names)
    reveals = sum(REVEAL in name for name in names)
    frames = names.count(NATIVE_FRAME)
    # A duplicate full name in another target belongs to this same exact
    # partition too; every occurrence still runs once with the same packages.
    serial = sum(name in serial_names for name in names)
    assert not any(LOG in name or REVEAL in name for name in serial_names)
    ordinary = len(names) - logs - reveals - serial - frames
    assert ordinary > 0, "ordinary inventory must fire"
    if shared:
        assert logs == reveals == 0, "shared excludes platform and separately runs Log"
        assert serial_names, "shared AppImage serial inventory must fire"
    else:
        assert logs > 0 and reveals > 0, "every measured group must fire"
    shards = [[] for _ in range(shard_count)]
    for name in names:
        if LOG not in name and REVEAL not in name and name not in serial_names and name != NATIVE_FRAME:
            # Identical names in different targets must stay in one shard;
            # libtest exact matching runs every occurrence there, once only.
            shards[hashlib.sha256(name.encode()).digest()[0] % shard_count].append(name)
    assert all(shards), "every ordinary shard must fire"
    parts = [
        (f"ordinary-{index}", ["--nocapture", "--test-threads=2", "--exact", *sorted(set(shard)), *skips], len(shard))
        for index, shard in enumerate(shards)
    ]
    if serial_names:
        parts.append(("appimage", ["--nocapture", "--test-threads=1", "--exact",
                                  *sorted(serial_names), *skips], serial))
    if not shared:
        parts += [
            ("logs", [LOG, "--nocapture", "--test-threads=1"], logs),
            ("reveal", [REVEAL, "--nocapture", "--test-threads=1"], reveals),
        ]
    print(f"WORKSPACE_CENSUS total={len(names)} ordinary={ordinary} logs={logs} reveal={reveals} serial={serial} native_frames={frames}",
          flush=True)
    # Separate processes retain separate Log pools. Hosted keeps four ordinary
    # threads; the existing heavy resource class can run eight. Overlap serial
    # Log I/O after inventory compilation. Exact filters prevent duplicate runs.
    # Reveal stays alone so its real D-Bus deadlines do not compete with either.
    concurrent = parts if shared else parts[:-1]
    execute = (lambda part: run(*part, command=command, limit=3600)) if shared else (lambda part: run(*part))
    with ThreadPoolExecutor(max_workers=len(concurrent)) as pool:
        results = list(pool.map(execute, concurrent))
    if frames:
        # Keep the actual 300 ms frame assertion, without concurrent fixture
        # work in the same census consuming its CPU scheduling interval.
        results.append(run('native-frame', ['--nocapture', '--test-threads=1', '--exact',
                                            NATIVE_FRAME, *skips], frames, command=command,
                           limit=3600 if shared else 1320))
    if not shared:
        results.append(run(*parts[-1]))
    passed = all(results)
    return 0 if passed else 1


if __name__ == "__main__":
    if sys.argv[1:] not in ([], ['--shared']):
        raise SystemExit('usage: workspace-rust-tests.py [--shared]')
    sys.exit(main(shared=bool(sys.argv[1:])))
