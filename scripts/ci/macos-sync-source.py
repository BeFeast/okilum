#!/usr/bin/env python3
"""Native Sync controls with non-empty discovery; no install, signing or service."""
import re
import subprocess
import sys


def main():
    if sys.platform != "darwin":
        raise RuntimeError("native Mac Sync controls require macOS")
    for package, target, selected in (
        ("okilum-sync-controller", "--lib", "direct::source::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::supervisor::ipc::macos_peer::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::supervisor::ipc::macos_exit::tests::"),
        ("okilum-sync-controller", "--lib", "sidecar::macos::connection::native::tests::"),
        ("okilum-shell", "--bin=okilum", "reader_settings_sync::tests::"),
    ):
        command = ["cargo", "test", "--locked", "-p", package, target, selected]
        listed = subprocess.run([*command, "--", "--list"], text=True, capture_output=True, check=True)
        print(listed.stdout, flush=True)
        if not re.search(r": test$", listed.stdout, re.M):
            raise RuntimeError(f"native Mac Sync filter did not fire: {selected}")
        subprocess.run([*command, "--", "--nocapture", "--test-threads=1"], check=True)


if __name__ == "__main__":
    main()
