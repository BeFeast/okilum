#!/usr/bin/env python3
"""Fail when the okilum-shell app crate grows past its line budget (#3232).

The okilum-shell test harness is one rustc whose peak memory grows with the
crate (~16 MiB per thousand lines; epic #3241). The crate is being split into
`crates/okilum-shell-<area>` crates; every split PR lowers BUDGET to the new
size plus headroom, so the app crate cannot grow back. New code for an area
that already has its own crate belongs there, not in `crates/okilum-shell/src`.

Counted: physical lines of every `*.rs` file under `crates/okilum-shell/src`
(production and in-crate tests; both compile into the harness). Assets and
other files are not counted.
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = Path("crates/okilum-shell/src")
# 441 056 lines on 2026-11-23 (main e81e9ba60). Headroom ~4 % for the lanes
# that keep landing code until split PR1 (#3233) lowers it.
BUDGET = 460_000


def count_lines(source: Path) -> tuple[int, int]:
    """Return (lines, files) over every `*.rs` file below `source`."""
    lines = files = 0
    for path in sorted(source.rglob("*.rs")):
        if not path.is_file():
            continue
        data = path.read_bytes()
        lines += data.count(b"\n") + (1 if data and not data.endswith(b"\n") else 0)
        files += 1
    return lines, files


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=ROOT, help="repository root")
    parser.add_argument("--budget", type=int, default=BUDGET, help="line budget (default: %(default)s)")
    args = parser.parse_args(argv)
    source = args.root / SOURCE
    if not source.is_dir():
        print(f"shell-crate-budget: {SOURCE} not found under {args.root}", file=sys.stderr)
        return 2
    lines, files = count_lines(source)
    if files == 0:
        # A path or glob mistake must not pass as "small".
        print(f"shell-crate-budget: no .rs files under {SOURCE}", file=sys.stderr)
        return 2
    print(f"SHELL_CRATE_LINES={lines} files={files} budget={args.budget} headroom={args.budget - lines}")
    if lines > args.budget:
        print(
            f"shell-crate-budget: {SOURCE} has {lines} lines, over its budget of {args.budget} "
            f"by {lines - args.budget}.\n"
            "The okilum-shell crate is being split (epic #3241): put new code for an area that "
            "already has a crate into crates/okilum-shell-<area>, or move code out before adding. "
            "Raising BUDGET in scripts/ci/shell-crate-budget.py needs the split lane's agreement.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
