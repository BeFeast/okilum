#!/usr/bin/env python3
"""Build exact main and PR release binaries on this job, with no measurement patch."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess


def run(*args, cwd=None):
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def build(checkout, output, commit, role):
    if run("git", "rev-parse", "HEAD", cwd=checkout) != commit:
        raise ValueError("checkout does not match pinned source")
    if run("git", "status", "--porcelain", "--untracked-files=no", cwd=checkout):
        raise ValueError("tracked source is dirty")
    subprocess.run(["bash", "scripts/vendor-setup.sh"], cwd=checkout, check=True)
    subprocess.run(["bash", "scripts/vendor-setup.sh", "--verify"], cwd=checkout, check=True)
    subprocess.run(["cargo", "build", "--release", "--locked", "-p", "okilum-shell", "--bin", "okilum"], cwd=checkout, check=True)
    target = Path(os.environ.get("CARGO_TARGET_DIR", checkout / "target"))
    if not target.is_absolute():
        target = checkout / target
    output.mkdir(parents=True, exist_ok=False)
    shutil.copy2(target / "release/okilum", output / "okilum")
    with (output / "okilum").open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    if run("git", "status", "--porcelain", "--untracked-files=no", cwd=checkout):
        raise ValueError("build changed tracked source")
    receipt = (f"commit {commit}\nshipped-unpatched release, role {role}\n"
               f"{run('rustc', '-Vv', cwd=checkout)}\nRUSTFLAGS={os.environ.get('RUSTFLAGS', '')}\n")
    (output / "BUILD.txt").write_text(receipt)
    (output / "SHA256SUMS").write_text(f"{digest}  okilum\n")
    return {"source_commit": commit, "sha256": digest, "kind": "shipped-unpatched"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("main_commit")
    parser.add_argument("out", type=Path)
    args = parser.parse_args()
    if not re.fullmatch("[0-9a-f]{40}", args.main_commit):
        raise ValueError("main commit must be a pinned full SHA")
    checkout = Path(run("git", "rev-parse", "--show-toplevel"))
    pr = run("git", "rev-parse", "HEAD", cwd=checkout)
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    baseline = out / "main-source"
    run("git", "worktree", "add", "--detach", str(baseline), args.main_commit, cwd=checkout)
    target = os.environ.setdefault("CARGO_TARGET_DIR", str(out / "target"))
    if not Path(target).is_absolute():
        raise ValueError("CARGO_TARGET_DIR must be absolute for both checkouts")
    receipts = {}
    try:
        receipts["main"] = build(baseline, out / "main", args.main_commit, "main")
        receipts["pr"] = build(checkout, out / "pr", pr, "pr")
        (out / "pair.json").write_text(json.dumps(receipts, indent=2) + "\n")
    finally:
        # Our detached source only; no reset/clean of a caller-owned checkout.
        run("git", "worktree", "remove", str(baseline), cwd=checkout)
    print(json.dumps(receipts))


if __name__ == "__main__":
    main()
