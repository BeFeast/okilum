#!/usr/bin/env python3
"""Reproduce the installed QA launch refusal with the old status adapter."""
from pathlib import Path
import subprocess
import sys


def main():
    if sys.platform != "darwin":
        raise RuntimeError("isolated launch control requires native macOS")
    source = Path("crates/okilum-sync-controller/src/sidecar/macos/isolation.rs")
    selected = "sidecar::macos::isolation::tests::dispatched_permission_passes_native_preflight_without_rearming"
    command = ["cargo", "test", "--locked", "-p", "okilum-sync-controller", "--lib",
               selected, "--", "--exact", "--nocapture"]
    positive = subprocess.run(command, text=True, capture_output=True, timeout=180)
    print(positive.stdout, flush=True)
    print(positive.stderr, flush=True)
    if positive.returncode or "1 passed; 0 failed" not in positive.stdout:
        raise RuntimeError("isolated native launch positive control did not pass")
    original = source.read_bytes()
    fixed = b"Phase::Ready | Phase::Dispatched if self.channel.is_none()"
    old = b"Phase::Ready if self.channel.is_none()"
    if original.count(fixed) != 1:
        raise RuntimeError("isolated native launch status boundary changed")
    try:
        source.write_bytes(original.replace(fixed, old))
        negative = subprocess.run(command, text=True, capture_output=True, timeout=180)
        print(negative.stdout, flush=True)
        print(negative.stderr, flush=True)
        if (negative.returncode == 0 or "0 passed; 1 failed" not in negative.stdout
                or "dispatch permission was mistaken for native registration"
                not in negative.stderr + negative.stdout):
            raise RuntimeError("old isolated status did not reproduce the actual preflight refusal")
    finally:
        source.write_bytes(original)
    print("ISOLATED_NATIVE_LAUNCH_PASS: real spawn/channel and one-use claim passed; "
          "old status failed the post-dispatch preflight", flush=True)


if __name__ == "__main__":
    main()
