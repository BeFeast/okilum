#!/usr/bin/env bash
# Linux Rust suites shared by full acceptance and hosted worker checks.
set -euo pipefail
# shellcheck source=scripts/ci/test-nofile.sh
source "$(dirname "${BASH_SOURCE[0]}")/test-nofile.sh"
cargo test -p okilum-appimage --all-features
cargo test -p okilum-core -p okilum-shell -p okilum-sync -p okilum-t3
# #1915: explicit filters make unsupported-flag coverage inspectable in CI.
cargo test --locked -p okilum-core --lib unix_publish::
cargo test --locked -p okilum-core --lib file_editor::network_save_tests::
cargo test --locked -p okilum-shell --bins reader_trash_fs::tests::unsupported_flags_trash_and_restore_files_and_folders_without_clobber -- --exact
# #588: keep shared durable pairing recovery covered on both Unix platforms.
cargo test --locked -p okilum-sync-controller --lib enrollment::
cargo test --locked -p okilum-sync-controller --test enrollment_storage
cargo test --locked -p okilum-sync-controller --lib sidecar::store::tests::launch_authority::
cargo test --locked -p okilum-sync-controller --lib sidecar::macos::recovery::
cargo test --locked -p okilum-sync-controller --lib sidecar::macos::registration::
cargo test --locked -p okilum-sync-controller --lib sidecar::macos::bundle::
cargo test --locked -p okilum-sync-controller --lib sidecar::macos::connection::
cargo test --locked -p okilum-sync-controller --lib sidecar::macos::payload::
cargo test --locked -p okilum-sync-controller --lib daemon::
cargo test --locked -p okilum-sync-controller --lib presentation::
cargo test --locked -p okilum-sync-controller --test daemon_access
# A second, explicit compiled distribution must refuse updater work even when
# inherited AppImage/update-receipt variables would otherwise enable it.
OKILUM_SPRINT_BUILD=1 OKILUM_RELEASE_VERSION='Sprint CI' \
    APPIMAGE=/nonexistent/OkilumSprint.AppImage OKILUM_APPIMAGE_UPDATE_RECEIPT=/nonexistent/receipt \
    cargo test --locked -p okilum-shell --bin okilum --features linux-sprint sprint_distribution -- --test-threads=1
cargo check -p okilum-shell --no-default-features
cargo test -p okilum-core --no-default-features --test portable_reader
# Vendored gpui-kit tests when the patch stack changes (otherwise a skip line).
bash scripts/ci/vendor-tests.sh
# #1924: injected no-system-Trash fallback is shared across Unix.
cargo test --locked -p okilum-shell --bins reader_trash_fs::vault_fallback_tests::
cargo test --locked -p okilum-shell --bins reader_trash_fs::vault_trash_inventory_tests::

# #588: one-use fresh preparation never regenerates partial state.
cargo test --locked -p okilum-sync-controller --lib sidecar::macos::preparation::
