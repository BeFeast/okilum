#!/usr/bin/env python3
"""Require nonempty native Windows Sync identity and retained-schema controls."""
import subprocess
import sys

TESTS = {
    "filesystem_identity::tests::windows_open_handle_identity_preserves_the_existing_serialized_shape_and_detects_replacement",
    "filesystem_identity::tests::windows_identity_refuses_reparse_points_from_the_open_handle",
}

def main():
    if sys.platform != "win32":
        raise RuntimeError("Windows Sync identity controls require Windows")
    command = ["cargo", "test", "--locked", "-p", "okilum-write-authority", "filesystem_identity::tests::"]
    listing = subprocess.run([*command, "--", "--list"], text=True, stdout=subprocess.PIPE, check=True).stdout
    if {line.removesuffix(": test") for line in listing.splitlines() if line.endswith(": test")} != TESTS:
        raise RuntimeError(f"Native Windows Sync identity controls did not compile/discover: {listing}")
    subprocess.run([*command, "--", "--nocapture", "--test-threads=1"], check=True)
    command = ["cargo", "test", "--locked", "-p", "okilum-sync-controller", "--lib", "private::tests::windows_private_state_"]
    expected = {
        "private::tests::windows_private_state_reopens_the_same_bytes_and_serializes_owners",
        "private::tests::windows_private_state_refuses_shared_directories_and_redirected_files",
    }
    listing = subprocess.run([*command, "--", "--list"], text=True, stdout=subprocess.PIPE, check=True).stdout
    if {line.removesuffix(": test") for line in listing.splitlines() if line.endswith(": test")} != expected:
        raise RuntimeError(f"Native Windows private Sync controls did not compile/discover: {listing}")
    return subprocess.run([*command, "--", "--nocapture", "--test-threads=1"], check=False).returncode

if __name__ == "__main__":
    raise SystemExit(main())
