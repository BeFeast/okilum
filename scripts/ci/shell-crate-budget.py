#!/usr/bin/env python3
"""Fail when okilum-shell's own sources grow past their line budget (#3232).

The okilum-shell test harness is one rustc process whose peak memory tracks the
crate's size (9.08 GiB at 441k lines, 23.11). The crate is being split into
okilum-shell-* crates (epic #3241); this guard keeps the app crate from growing
back. Each split PR lowers the budget in shell-crate-budget.json.

New code for an area that already has its own crate goes into
crates/okilum-shell-<area>/src, not into crates/okilum-shell/src.
"""
import argparse
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
BUDGET = Path("scripts/ci/shell-crate-budget.json")


def count_lines(directory: Path) -> tuple[int, int]:
    """Return (lines, files) over every .rs file below directory, like `wc -l`."""
    lines = files = 0
    for path in sorted(directory.rglob("*.rs")):
        if path.is_file():
            lines += path.read_bytes().count(b"\n")
            files += 1
    return lines, files


def check(root: Path) -> list[str]:
    budget_path = root / BUDGET
    try:
        budget = json.loads(budget_path.read_text())
    except (OSError, ValueError) as error:
        return [f"cannot read {BUDGET}: {error}"]
    failures = []
    entries = budget.get("crates")
    if not isinstance(entries, dict) or not entries:
        return [f"{BUDGET}: 'crates' must map source directories to budgets"]
    for directory, entry in sorted(entries.items()):
        limit = entry.get("max_lines") if isinstance(entry, dict) else None
        if not isinstance(limit, int) or limit <= 0:
            failures.append(f"{BUDGET}: {directory}: max_lines must be a positive integer")
            continue
        source = root / directory
        if not source.is_dir():
            failures.append(f"{directory}: source directory does not exist")
            continue
        lines, files = count_lines(source)
        print(f"{directory}: {lines} lines in {files} .rs files, budget {limit} "
              f"(headroom {limit - lines})")
        if lines > limit:
            failures.append(
                f"{directory}: {lines} lines exceed the budget of {limit} by {lines - limit}. "
                "okilum-shell is being split into okilum-shell-* crates (epic #3241): put new "
                "code for an area that has its own crate into crates/okilum-shell-<area>/src. "
                f"Raise the budget in {BUDGET} only with the reason in the PR.")
    return failures


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=ROOT, help="repository root")
    args = parser.parse_args(argv)
    failures = check(args.root)
    for failure in failures:
        print(f"shell-crate-budget: FAIL: {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
