#!/usr/bin/env bash
# Linux Rust suites shared by full acceptance and hosted worker checks.
set -euo pipefail
# shellcheck source=scripts/ci/test-nofile.sh
source "$(dirname "${BASH_SOURCE[0]}")/test-nofile.sh"
# Keep every failure, while allowing independent suites to finish their evidence.
failed=0
suite() {
    printf '\nLinux suite: %s\n' "$*"
    "$@" || failed=1
}
suite cargo test -p okilum-appimage --all-features --no-fail-fast
# Preserve assertion diagnostics even if a later native task aborts the process.
suite cargo test -p okilum-core -p okilum-shell -p okilum-sync -p okilum-t3 --no-fail-fast -- --nocapture
# #1915: explicit filters make unsupported-flag coverage inspectable in CI.
suite cargo test --locked -p okilum-core --lib unix_publish:: --no-fail-fast
suite cargo test --locked -p okilum-core --lib file_editor::network_save_tests:: --no-fail-fast
suite cargo test --locked -p okilum-shell --bins reader_trash_fs::tests::unsupported_flags_trash_and_restore_files_and_folders_without_clobber --no-fail-fast -- --exact
# #588: keep shared durable pairing recovery covered on both Unix platforms.
suite cargo test --locked -p okilum-sync-controller --lib enrollment:: --no-fail-fast
suite cargo test --locked -p okilum-sync-controller --test enrollment_storage --no-fail-fast
suite cargo test --locked -p okilum-sync-controller --lib sidecar::store::tests::launch_authority:: --no-fail-fast
suite cargo test --locked -p okilum-sync-controller --lib sidecar::macos::recovery:: --no-fail-fast
suite cargo test --locked -p okilum-sync-controller --lib sidecar::macos::registration:: --no-fail-fast
suite cargo test --locked -p okilum-sync-controller --lib sidecar::macos::bundle:: --no-fail-fast
suite cargo test --locked -p okilum-sync-controller --lib sidecar::macos::connection:: --no-fail-fast
suite cargo test --locked -p okilum-sync-controller --lib sidecar::macos::payload:: --no-fail-fast
suite cargo test --locked -p okilum-sync-controller --lib daemon:: --no-fail-fast
suite cargo test --locked -p okilum-sync-controller --lib presentation:: --no-fail-fast
suite cargo test --locked -p okilum-sync-controller --test daemon_access --no-fail-fast
# A second, explicit compiled distribution must refuse updater work even when
# inherited AppImage/update-receipt variables would otherwise enable it.
OKILUM_SPRINT_BUILD=1 OKILUM_RELEASE_VERSION='Sprint CI' \
    APPIMAGE=/nonexistent/OkilumSprint.AppImage OKILUM_APPIMAGE_UPDATE_RECEIPT=/nonexistent/receipt \
    suite cargo test --locked -p okilum-shell --bin okilum --features linux-sprint sprint_distribution --no-fail-fast -- --test-threads=1
suite cargo check -p okilum-shell --no-default-features
suite cargo test -p okilum-core --no-default-features --test portable_reader --no-fail-fast
# Vendored gpui-kit tests when the patch stack changes (otherwise a skip line).
suite bash scripts/ci/vendor-tests.sh
# #1924: injected no-system-Trash fallback is shared across Unix.
suite cargo test --locked -p okilum-shell --bins reader_trash_fs::vault_fallback_tests:: --no-fail-fast
suite cargo test --locked -p okilum-shell --bins reader_trash_fs::vault_trash_inventory_tests:: --no-fail-fast

# #588: one-use fresh preparation never regenerates partial state.
suite cargo test --locked -p okilum-sync-controller --lib sidecar::macos::preparation:: --no-fail-fast
exit "$failed"
