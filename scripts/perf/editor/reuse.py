#!/usr/bin/env python3
"""Reuse a successful calibration run's exact-commit trace binary without rebuilding.

The workflow verifies the producer run's head SHA and successful conclusion
before downloading. Here the build receipt and binary checksum must agree too.
"""
import argparse
import hashlib
from pathlib import Path
import re
import shutil


def reuse(root, output, commit):
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("expected a full source commit SHA")
    trace = root / "trace"
    receipt = (trace / "BUILD.txt").read_text().splitlines()
    if not receipt or receipt[0] != f"commit {commit}":
        raise ValueError("artifact belongs to another source commit")
    checksum = (trace / "SHA256SUMS").read_text().strip()
    match = re.fullmatch(r"([0-9a-f]{64})  okilum", checksum)
    if not match:
        raise ValueError("invalid binary checksum receipt")
    binary = trace / "okilum"
    with binary.open("rb") as stream:
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    if actual != match[1]:
        raise ValueError("trace binary checksum mismatch")
    output.mkdir(parents=True, exist_ok=False)
    for name in ("okilum", "BUILD.txt", "SHA256SUMS"):
        shutil.copyfile(trace / name, output / name)
    (output / "okilum").chmod(0o755)
    return actual


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("commit")
    args = parser.parse_args()
    print(f"Reused {args.commit}: sha256 {reuse(args.artifact, args.output, args.commit)}")


if __name__ == "__main__":
    main()
