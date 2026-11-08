#!/usr/bin/env python3
"""Prove native readiness polling leaves startup directory readers unblocked."""
from pathlib import Path
import os
import subprocess


def check():
    if os.name != "nt":
        raise RuntimeError("Readiness sharing control requires native Windows")
    source = Path("crates/okilum-sync-controller/src/sidecar/windows/bootstrap.rs")
    original = source.read_bytes()
    boundary = b"        if running()? {"
    if original.count(boundary) != 1:
        raise RuntimeError("Readiness control must select the production observation")
    command = ["cargo", "test", "--locked", "-p", "okilum-sync-controller", "--lib",
               "sidecar::windows::bootstrap::tests::windows_native_readiness_does_not_lock_startup_reads_and_rechecks_cancelled_intent",
               "--", "--exact", "--nocapture"]
    positive = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=180)
    print(positive.stdout, flush=True)
    positive.check_returncode()
    if "test result: ok. 1 passed;" not in positive.stdout:
        raise RuntimeError("The native readiness positive did not fire")
    try:
        # Restore the former pre-readiness transaction on the measured path.
        source.write_bytes(original.replace(boundary,
            b"        drop(store.begin(deadline)?);\n" + boundary))
        negative = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=180)
        print(negative.stdout, flush=True)
        if negative.returncode == 0 or "sidecar state is busy" not in negative.stdout:
            raise RuntimeError("Pre-readiness transactions did not reproduce native sharing contention")
    finally:
        source.write_bytes(original)
    print("WINDOWS_TASK_READINESS_PASS: native directory contention reproduced; authenticated readiness precedes intent locking; concurrent Disable remains refused", flush=True)


if __name__ == "__main__":
    check()
