#!/usr/bin/env python3
"""Require nonempty native Windows Sync identity and retained-schema controls."""
import subprocess
import sys
import os
import tempfile
from pathlib import Path

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
        "sidecar::windows::signature::tests::windows_authenticode_policy_requires_trust_exact_signer_and_unchanged_bytes",
        "managed_launch::windows::tests::windows_listener_and_process_proofs_require_live_kernel_owners",
        "direct::lan::tests::windows_lan_selection_requires_live_physical_interfaces",
        "direct::inventory::tree::tests::windows_inventory_reads_binary_bytes_and_refuses_redirects_and_hardlinks",
        "direct::inventory::merge::storage::windows::tests::windows_native_publication_recovers_capture_gap_and_preserves_every_original",
    ):
        command = ["cargo", "test", "--locked", "-p", "okilum-sync-controller", "--lib", test]
        listing = subprocess.run([*command, "--", "--exact", "--include-ignored", "--list"], text=True, stdout=subprocess.PIPE, check=True).stdout
        if f"{test}: test" not in listing or "1 test" not in listing:
            raise RuntimeError(f"Native Windows kernel control did not compile/discover: {listing}")
        with tempfile.TemporaryDirectory(prefix="okilum-first-sync-") as root:
            selected = str(Path(root).resolve())
            env = dict(os.environ, LOCALAPPDATA=selected, OKILUM_WINDOWS_FIRST_SYNC_FIXTURE=selected)
            subprocess.run([*command, "--", "--exact", "--include-ignored", "--nocapture"], env=env, check=True)
    command = ["cargo", "test", "--locked", "-p", "okilum-sync-controller", "--lib", "sidecar::supervisor::windows::tests::"]
    listing = subprocess.run([*command, "--", "--list"], text=True, stdout=subprocess.PIPE, check=True).stdout
    native_jobs = {line.removesuffix(": test") for line in listing.splitlines() if line.endswith(": test")}
    expected_jobs = {"sidecar::supervisor::windows::tests::" + name for name in (
        "native_job_stop_and_drop_terminate_confirmed_descendant",
        "native_job_reports_live_descendant_after_root_exit",
        "native_job_expired_deadline_refuses_termination_with_stop_positive_control",
        "native_job_tree_reports_stopped_only_for_the_whole_tree_and_stopping_on_timeout",
        "native_job_uncaptured_exit_cannot_authorize_stopped",
    )}
    if native_jobs != expected_jobs:
        raise RuntimeError(f"Complete Windows Job Object controls did not compile/discover: {listing}")
    subprocess.run([*command, "--", "--nocapture", "--test-threads=1"], check=True)
    subprocess.run(["cargo", "test", "--locked", "-p", "okilum-sync-supervisor"], check=True)
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
