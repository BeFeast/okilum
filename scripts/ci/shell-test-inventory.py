#!/usr/bin/env python3
"""Test inventory of every okilum-shell* crate, for the crate split (#3232).

`list` compiles the test targets of every workspace package named okilum-shell
or okilum-shell-* and writes one sorted line per test path (crate and target
are deliberately left out: a split moves a module to another crate but keeps
its path, so `reader_settings_sync::tests::x` stays the same name).

`compare BASE HEAD` fails when a test of BASE is missing from HEAD, counting
duplicates. A split PR runs `list` on its merge base and on its head and puts
the comparison into the PR. Intended renames (items moved out of main.rs) are
listed as `old -> new` lines in --renames.

`list` compiles the okilum-shell test harness: run it in a build slot
(`okilum-slot run`) or on GitHub, never on the orchestration host.
"""
import argparse
from collections import Counter
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]


def target_flags(package: dict) -> list[str]:
    """Unit tests of every lib and bin target plus integration tests, for the
    targets this package has (cargo refuses `--lib` without a library).
    Examples and doctests are not part of the suites the split moves."""
    kinds = {kind for target in package["targets"] for kind in target["kind"]}
    flags = []
    if kinds & {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"}:
        flags.append("--lib")
    if "bin" in kinds:
        flags.append("--bins")
    if "test" in kinds:
        flags.append("--tests")
    return flags


def shell_packages(root: Path) -> list[tuple[str, list[str]]]:
    metadata = json.loads(subprocess.run(
        ["cargo", "metadata", "--locked", "--offline", "--no-deps", "--format-version", "1"],
        cwd=root, check=True, capture_output=True, text=True).stdout)
    packages = sorted((p["name"], target_flags(p)) for p in metadata["packages"]
                      if p["name"] == "okilum-shell" or p["name"].startswith("okilum-shell-"))
    if "okilum-shell" not in {name for name, _ in packages}:
        raise SystemExit("okilum-shell is not a workspace package")
    return packages


def parse_list(text: str) -> list[str]:
    """Test paths from libtest `--list --format terse` output (benchmarks excluded)."""
    return [line[:-len(": test")] for line in text.splitlines() if line.endswith(": test")]


def list_tests(root: Path, packages: list[tuple[str, list[str]]], extra: list[str]) -> list[str]:
    tests = []
    for package, flags in packages:
        command = ["cargo", "test", "--locked", "-p", package, *flags,
                   *extra, "--", "--list", "--format", "terse"]
        result = subprocess.run(command, cwd=root, capture_output=True, text=True)
        if result.returncode != 0:
            sys.stderr.write(result.stderr[-4000:])
            raise SystemExit(f"{' '.join(command)} exited {result.returncode}")
        found = parse_list(result.stdout)
        print(f"{package}: {len(found)} tests", file=sys.stderr)
        tests.extend(found)
    return sorted(tests)


def read_renames(path: Path | None) -> dict[str, str]:
    renames = {}
    if path is None:
        return renames
    for number, line in enumerate(path.read_text().splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        old, arrow, new = line.partition(" -> ")
        if not arrow or not old or not new:
            raise SystemExit(f"{path}:{number}: expected 'old -> new'")
        renames[old.strip()] = new.strip()
    return renames


def compare(base: list[str], head: list[str], renames: dict[str, str]) -> tuple[list[str], list[str]]:
    """Return (lost, added): tests of base absent from head, and new tests of head."""
    want = Counter(renames.get(name, name) for name in base)
    have = Counter(head)
    lost = sorted((want - have).elements())
    added = sorted((have - want).elements())
    return lost, added


def read_lines(path: Path) -> list[str]:
    return [line for line in path.read_text().splitlines() if line]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    listing = sub.add_parser("list", help="write the sorted test inventory")
    listing.add_argument("--out", type=Path, required=True)
    listing.add_argument("--root", type=Path, default=ROOT)
    listing.add_argument("cargo_args", nargs="*", help="extra cargo test arguments, after --")
    comparing = sub.add_parser("compare", help="fail when a BASE test is missing from HEAD")
    comparing.add_argument("base", type=Path)
    comparing.add_argument("head", type=Path)
    comparing.add_argument("--renames", type=Path)
    args = parser.parse_args(argv)

    if args.command == "list":
        tests = list_tests(args.root, shell_packages(args.root), args.cargo_args)
        if not tests:
            print("shell-test-inventory: no tests listed", file=sys.stderr)
            return 1
        args.out.write_text("".join(f"{name}\n" for name in tests))
        print(f"{len(tests)} tests -> {args.out}")
        return 0

    base, head = read_lines(args.base), read_lines(args.head)
    lost, added = compare(base, head, read_renames(args.renames))
    print(f"base {len(base)} tests, head {len(head)} tests, lost {len(lost)}, added {len(added)}")
    for name in lost:
        print(f"- {name}")
    for name in added:
        print(f"+ {name}")
    return 1 if lost or not base else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
