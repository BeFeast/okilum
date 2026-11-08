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
    subprocess.run([*command, "--", "--nocapture", "--test-threads=1"], check=True)
    for test in (
        "managed_launch::windows::tests::windows_listener_and_process_proofs_require_live_kernel_owners",
        "direct::lan::tests::windows_lan_selection_requires_live_physical_interfaces",
    ):
        command = ["cargo", "test", "--locked", "-p", "okilum-sync-controller", "--lib", test]
        listing = subprocess.run([*command, "--", "--exact", "--list"], text=True, stdout=subprocess.PIPE, check=True).stdout
        if f"{test}: test" not in listing or "1 test" not in listing:
            raise RuntimeError(f"Native Windows kernel control did not compile/discover: {listing}")
        subprocess.run([*command, "--", "--exact", "--nocapture"], check=True)
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
