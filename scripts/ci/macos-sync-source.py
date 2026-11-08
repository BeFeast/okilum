#!/usr/bin/env python3
"""Native Sync controls with non-empty discovery; no install, signing or service."""
import re
import subprocess
import sys
from pathlib import Path


def health_negative_controls():
    """The idle/controller regressions must fail under the former predicates."""
    health = Path("crates/okilum-shell/src/reader_settings_sync/health.rs")
    settings = Path("crates/okilum-shell/src/reader_settings_sync.rs")
    originals = {health: health.read_text(), settings: settings.read_text()}
    idle = "vault.connected || vault.needed_items == 0 || vault.shared_with.is_empty()"
    publish = ".observe(&output.overview, output.runtime_health, Instant::now());"
    if originals[health].count(idle) != 1 or originals[settings].count(publish) != 1:
        raise RuntimeError("Sync health negative-control source anchors changed")
    try:
        health.write_text(originals[health].replace(
            idle, "vault.connected || vault.shared_with.is_empty()"))
        settings.write_text(originals[settings].replace(publish,
            ".observe(&output.overview, health::Runtime { "
            "down: output.runtime_health.down || output.connection_error.is_some(), "
            "..output.runtime_health }, Instant::now());"))
        for selected, assertion in (
            ("idle_up_to_date_is_healthy_even_with_old_contact_and_every_peer_asleep",
             "no pending items means no stalled sync"),
            ("sync_health_controller_error_does_not_override_healthy_runtime_and_recovery_clears_banner",
             "healthy Sync must not become a red alarm"),
        ):
            command = ["cargo", "test", "--locked", "-p", "okilum-shell", "--bin=okilum",
                       selected, "--", "--nocapture", "--test-threads=1"]
            result = subprocess.run(command, text=True, capture_output=True)
            output = result.stdout + result.stderr
            print(output, flush=True)
            if result.returncode == 0 or assertion not in output or "1 failed" not in output:
                raise RuntimeError(f"Sync health old-behavior control did not fail at runtime: {selected}")
    finally:
        for path, source in originals.items():
            path.write_text(source)


def main():
    if sys.platform != "darwin":
        raise RuntimeError("native Mac Sync controls require macOS")
    health_negative_controls()
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
        ("okilum-sync-controller", "--lib", "direct::overview::tests::"),
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
    subprocess.run([sys.executable, "scripts/ci/macos-isolated-launch-check.py"], check=True)


if __name__ == "__main__":
    main()
