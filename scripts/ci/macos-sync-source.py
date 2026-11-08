#!/usr/bin/env python3
"""Native Sync controls with non-empty discovery; no install, signing or service."""
import re
import subprocess
import sys


def main():
    if sys.platform != "darwin":
        raise RuntimeError("native Mac Sync controls require macOS")
    for package, target, selected in (
        ("okilum-sync-supervisor", "--lib", "args::tests::"),
        ("okilum-sync-supervisor", "--lib", "serve::unix::tests::"),
        ("okilum-sync-controller", "--lib", "direct::source::tests::"),
        ("okilum-sync-controller", "--lib", "direct::inventory::tests::"),
        ("okilum-sync-controller", "--lib", "managed_launch::macos::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::macos::isolation::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::macos::bootstrap::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::authority::launch::tests::"),
        ("okilum-sync", "--lib", "tests::issue2752_"),
        ("okilum-write-authority", "--lib", "identity_macos_tests::"),
        ("okilum-sync-controller", "--lib", "direct::existing::tests::issue2752_"),
        ("okilum-sync-controller", "--lib", "direct::transfer::tests::issue2752_"),
        ("okilum-sync-controller", "--lib", "sidecar::supervisor::ipc::macos_peer::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::supervisor::ipc::macos_exit::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::macos::connection::native::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::supervisor::lease::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::store::tests::launch_authority::"),
        ("okilum-shell", "--bin=okilum", "reader_settings_sync::health::tests::"),
        ("okilum-shell", "--bin=okilum", "reader_settings_sync::tests::"),
        ("okilum-shell", "--bin=okilum", "reader_settings_sync::first_sync::tests::"),
        # #2708: compile AppKit bindings and execute normal/zoom/fullscreen controls.
        ("okilum-shell", "--bin=okilum", "window_state::"),
    ):
        command = ["cargo", "test", "--locked", "-p", package, target, selected]
        listed = subprocess.run([*command, "--", "--list"], text=True, capture_output=True, check=True)
        print(listed.stdout, flush=True)
        if not re.search(r": test$", listed.stdout, re.M):
            raise RuntimeError(f"native Mac Sync filter did not fire: {selected}")
        subprocess.run([*command, "--", "--nocapture", "--test-threads=1"], check=True)


if __name__ == "__main__":
    main()
