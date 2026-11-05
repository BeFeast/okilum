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
suite bash scripts/ci/local-writer-admission.sh
# #2615: keep the complete Sync modal/approval suite visible before unrelated native tests.
suite cargo test --locked -p okilum-shell --bin okilum reader_settings_sync:: -- --nocapture --test-threads=1
# #2589: a real app pairing listener, fresh identity and repeated Add computer.
suite cargo test --locked -p okilum-sync-controller --test first_computer -- --nocapture
suite cargo test --locked -p okilum-sync-controller --lib direct::lan::tests:: -- --nocapture
# #2049: explicit non-empty UI and compatibility verdicts before the full shell.
suite node --test web/inbox/tests/retired-sync.test.js web/inbox/tests/sync-pairing.test.js
suite cargo test --locked -p okilum-shell --bin okilum issue2049_ -- --nocapture
# Process fixtures fork with live descriptors. Isolate their lock lifecycle:
# a sibling test's child can retain another fixture's flock until exec.
suite cargo test -p okilum-appimage --all-features --no-fail-fast -- --test-threads=1
suite cargo test --locked -p okilum-http --features updates --no-fail-fast
# #2572: retain a separate non-empty verdict for unified Vaults and log sources.
suite cargo test --locked -p okilum-shell --bin okilum issue2572_ -- --nocapture
# #1587: preserve explicit non-empty classifier/tree/badge click evidence.
suite cargo test --locked -p okilum-shell --bin okilum issue1587_ -- --nocapture
# Keep an explicit non-empty Settings dismissal verdict ahead of the full shell.
# Keep Board filter interaction and model verdicts explicit and non-empty.
suite cargo test --locked -p okilum-shell --bin okilum issue2532_ -- --nocapture
suite cargo test --locked -p okilum-shell --bin okilum list_2530_ -- --nocapture
suite cargo test --locked -p okilum-shell --bin okilum settings_2487_ -- --nocapture
# #2534: an explicit native compact-card group must execute before the full shell.
suite cargo test --locked -p okilum-shell --bin okilum kanban_2534_ -- --nocapture
suite cargo test --locked -p okilum-shell --bin okilum list_2548_ -- --nocapture
suite cargo test --locked -p okilum-t3 board::order:: -- --nocapture
# Keep the inline agent-document regression verdict non-empty and independent.
suite cargo test --locked -p okilum-shell --bin okilum issue1588_ -- --nocapture --test-threads=1
# #2533: explicit non-empty waiting verdicts before shared package tests.
suite cargo test --locked -p okilum-shell --bin okilum issue2533_ -- --nocapture
suite cargo test --locked -p okilum-shell --bin okilum waiting_2533_ -- --nocapture
# Run the source/input acceptance groups independently, so an unrelated native
# task abort cannot hide their verdict behind an unfinished full shell binary.
# A non-UTC process timezone makes the conflict-copy local timestamp check
# distinguish Local from Utc without mutating global environment in Rust tests.
suite env TZ=Pacific/Auckland cargo test --locked -p okilum-shell --bin okilum issue2486_ -- --nocapture --test-threads=1
suite cargo test --locked -p okilum-sync-controller --lib direct::origin::tests:: -- --nocapture
suite cargo test --locked -p okilum-shell --bin okilum source_presentation:: -- --nocapture
suite cargo test --locked -p okilum-shell --bin okilum reader_editor::live_preview:: -- --nocapture
suite cargo test --locked -p okilum-shell --bin okilum input_anchor_tests:: -- --nocapture
suite cargo test --locked -p okilum-shell --bin okilum reader_editor::tables::grid:: -- --nocapture
suite cargo test --locked -p okilum-shell --bin okilum reader_editor::lists::key_tests:: -- --nocapture
suite cargo test --locked -p okilum-shell --bin okilum reader_outline::tests::draft_1320_ -- --nocapture
# #2044: explicit conflict controller and mounted shell acceptance.
suite cargo test --locked -p okilum-sync-controller --lib conflicts:: -- --nocapture
suite cargo test --locked -p okilum-shell --bin okilum issue2044_ -- --nocapture --test-threads=1
# Preserve assertion diagnostics even if a later native task aborts the process.
suite cargo test -p okilum-core -p okilum-shell -p okilum-sync -p okilum-t3 --no-fail-fast -- --nocapture
# #1915: explicit filters make unsupported-flag coverage inspectable in CI.
suite cargo test --locked -p okilum-core --lib unix_publish:: --no-fail-fast
suite cargo test --locked -p okilum-core --lib file_editor::network_save_tests:: --no-fail-fast
suite cargo test --locked -p okilum-shell --bins reader_trash_fs::tests::unsupported_flags_trash_and_restore_files_and_folders_without_clobber --no-fail-fast -- --exact
# #2442: retain process-group generation checks across live -> zombie -> reaped.
suite cargo test --locked -p okilum-sync-controller --lib sidecar::supervisor::process_group:: --no-fail-fast -- --test-threads=1
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
