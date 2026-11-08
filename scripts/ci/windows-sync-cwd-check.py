#!/usr/bin/env python3
"""Prove the inherited native task CWD cannot retain the Sync transaction lock."""
from pathlib import Path
import os
import subprocess


def check():
    if os.name != "nt":
        raise RuntimeError("CWD sharing control requires native Windows")
    source = Path("crates/okilum-sync-supervisor/src/main.rs")
    original = source.read_bytes()
    fixed = (b'        std::env::set_current_dir(own.parent().context("helper directory missing")?)\n'
             b'            .context("release inherited state working directory")?;')
    if original.count(fixed) != 1:
        raise RuntimeError("Native CWD control must select the exact production boundary")
    command = ["cargo", "test", "--locked", "-p", "okilum-sync-supervisor", "--test", "serve_windows",
               "the_binary_exits_zero_when_idle_and_refuses_to_serve_without_a_signature_policy",
               "--", "--exact", "--nocapture"]
    positive = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=180)
    print(positive.stdout, flush=True)
    positive.check_returncode()
    if "test result: ok. 1 passed;" not in positive.stdout:
        raise RuntimeError("The native inherited-CWD positive did not fire")
    try:
        source.write_bytes(original.replace(fixed, b"        // CWD negative control: retain the inherited state handle."))
        baseline = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=180)
        print(baseline.stdout, flush=True)
        if baseline.returncode == 0 or "sidecar state is busy" not in baseline.stdout:
            raise RuntimeError("The unchanged native CWD did not reproduce the actual store refusal")
    finally:
        source.write_bytes(original)
    print("WINDOWS_TASK_CWD_PASS: inherited native directory sharing reproduced; fixed production startup acquires the unchanged store lock", flush=True)


if __name__ == "__main__":
    check()
