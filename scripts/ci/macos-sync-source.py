#!/usr/bin/env python3
"""Native Sync controls with non-empty discovery; no install, signing or service."""
import re
import subprocess
import sys
from pathlib import Path


def recovery_negative_control():
    path = Path("crates/okilum-shell/src/reader_settings_sync/health.rs")
    original = path.read_bytes()
    guard = b"if !requested.swap(true, std::sync::atomic::Ordering::SeqCst) {"
    assert original.count(guard) == 1
    test = "reader_settings_sync::health::tests::managed_recovery_requests_once_and_waits_for_authenticated_health"
    command = ["cargo", "test", "--locked", "-p", "okilum-shell", "--bin=okilum", test,
               "--", "--exact", "--nocapture", "--test-threads=1"]
    try:
        # Restore the old no-automatic-restart behavior in compiled code.
        path.write_bytes(original.replace(guard, b"if requested.swap(true, std::sync::atomic::Ordering::SeqCst) && false {"))
        result = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        print(result.stdout, flush=True)
        assert result.returncode != 0 and "running 1 test" in result.stdout
        assert "0 passed; 1 failed" in result.stdout
        assert "a stale helper needs one bounded recovery request" in result.stdout
    finally:
        path.write_bytes(original)
    subprocess.run(command, check=True)


def main():
    if sys.platform != "darwin":
        raise RuntimeError("native Mac Sync controls require macOS")
    for package, target, selected in (
        ("okilum-sync", "--lib", "tests::issue2752_"),
        ("okilum-write-authority", "--lib", "identity_macos_tests::"),
        ("okilum-sync-controller", "--lib", "direct::source::tests::"),
        ("okilum-sync-controller", "--lib", "direct::existing::tests::issue2752_"),
        ("okilum-sync-controller", "--lib", "direct::transfer::tests::issue2752_"),
        ("okilum-sync-controller", "--lib", "sidecar::supervisor::ipc::macos_peer::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::supervisor::ipc::macos_exit::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::macos::connection::native::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::supervisor::lease::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::store::tests::launch_authority::"),
        ("okilum-sync-supervisor", "--lib", "args::tests::"),
        ("okilum-shell", "--bin=okilum", "reader_settings_sync::health::tests::"),
        ("okilum-shell", "--bin=okilum", "reader_settings_sync::tests::"),
        # #2708: compile AppKit bindings and execute normal/zoom/fullscreen controls.
        ("okilum-shell", "--bin=okilum", "window_state::"),
    ):
        command = ["cargo", "test", "--locked", "-p", package, target, selected]
        listed = subprocess.run([*command, "--", "--list"], text=True, capture_output=True, check=True)
        print(listed.stdout, flush=True)
        if not re.search(r": test$", listed.stdout, re.M):
            raise RuntimeError(f"native Mac Sync filter did not fire: {selected}")
        subprocess.run([*command, "--", "--nocapture", "--test-threads=1"], check=True)
    recovery_negative_control()


if __name__ == "__main__":
    main()
