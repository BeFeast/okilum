#!/usr/bin/env bash
# Linux Rust suites shared by full acceptance and hosted worker checks.
set -euo pipefail
# shellcheck source=scripts/ci/test-nofile.sh
source "$(dirname "${BASH_SOURCE[0]}")/test-nofile.sh"
# Retain admission's package feature union in later default-feature filters.
# Switching back to single-package roots otherwise recompiles the same GUI
# and core dependencies inside short operation groups after cold admission.
packages=(-p okilum-write-authority -p okilum-core -p okilum-shell -p okilum-t3)
# Keep every failure, while allowing independent suites to finish their evidence.
failed=0
suite() {
    local group=$1 limit=$2
    local account=()
    shift 2
    if [[ ${1:-} == cargo && ${2:-} == test ]]; then
        case "$group" in
            appimage|http-updates|sprint-distribution|portable-reader) ;;
            *) set -- "$1" "$2" "${packages[@]}" "${@:3}" ;;
        esac
    fi
    printf '\nLinux suite: %s\n' "$*"
    if [[ "$group" == reader-editor-live-preview ]]; then
        account=(--isolated-writer-account)
    fi
    bash scripts/ci/test-group.sh "${account[@]}" "$group" "$limit" "$@" || failed=1
}
suite local-writer 900 bash scripts/ci/local-writer-admission.sh
# #2710: these regression names are outside the original admission filter.
suite linux-writer-identity 120 cargo test --locked -p okilum-write-authority --lib identity_linux_tests:: -- --nocapture
suite attached-replica-identity 120 cargo test --locked -p okilum-sync-controller --lib direct::existing::tests:: -- --nocapture
# #2589: a real app pairing listener, fresh identity and repeated Add computer.
suite first-computer 180 cargo test --locked -p okilum-sync-controller --test first_computer -- --nocapture
suite direct-lan 120 cargo test --locked -p okilum-sync-controller --lib direct::lan::tests:: -- --nocapture
suite reader-settings-sync 300 cargo test --locked -p okilum-shell --bin okilum reader_settings_sync:: -- --nocapture --test-threads=1
suite feedback-fast-report 120 cargo test --locked -p okilum-shell --bin okilum reader_feedback::ui_tests::feedback2749_ -- --nocapture --test-threads=1
suite direct-tests 180 cargo test --locked -p okilum-sync-controller --lib direct::tests:: -- --nocapture
suite sync-cli 180 cargo test --locked -p okilum-shell --bin okilum issue2589_ -- --nocapture
# #2049: explicit non-empty UI and compatibility verdicts before the full shell.
suite web-sync 60 node --test web/inbox/tests/retired-sync.test.js web/inbox/tests/sync-pairing.test.js
suite issue2049 120 cargo test --locked -p okilum-shell --bin okilum issue2049_ -- --nocapture
# Process fixtures fork with live descriptors. Isolate their lock lifecycle:
# a sibling test's child can retain another fixture's flock until exec.
suite appimage 120 cargo test -p okilum-appimage --all-features --no-fail-fast -- --test-threads=1
suite http-updates 180 cargo test --locked -p okilum-http --features updates --no-fail-fast
# #2572: retain a separate non-empty verdict for unified Vaults and log sources.
suite issue2572 120 cargo test --locked -p okilum-shell --bin okilum issue2572_ -- --nocapture
# #1587: preserve explicit non-empty classifier/tree/badge click evidence.
suite issue1587 120 cargo test --locked -p okilum-shell --bin okilum issue1587_ -- --nocapture
suite issue1622 120 cargo test --locked -p okilum-shell --bin okilum issue1622_ -- --nocapture
# Keep an explicit non-empty Settings dismissal verdict ahead of the full shell.
# Keep Board filter interaction and model verdicts explicit and non-empty.
suite issue2532 120 cargo test --locked -p okilum-shell --bin okilum issue2532_ -- --nocapture
suite list-2530 120 cargo test --locked -p okilum-shell --bin okilum list_2530_ -- --nocapture
suite settings-2487 120 cargo test --locked -p okilum-shell --bin okilum settings_2487_ -- --nocapture
# #2534: an explicit native compact-card group must execute before the full shell.
suite kanban-2534 120 cargo test --locked -p okilum-shell --bin okilum kanban_2534_ -- --nocapture
suite list-2548 120 cargo test --locked -p okilum-shell --bin okilum list_2548_ -- --nocapture
suite board-order 120 cargo test --locked -p okilum-t3 board::order:: -- --nocapture
# Keep the inline agent-document regression verdict non-empty and independent.
suite issue1588 120 cargo test --locked -p okilum-shell --bin okilum issue1588_ -- --nocapture --test-threads=1
# #2533: explicit non-empty waiting verdicts before shared package tests.
suite issue2533 120 cargo test --locked -p okilum-shell --bin okilum issue2533_ -- --nocapture
suite waiting-2533 120 cargo test --locked -p okilum-shell --bin okilum waiting_2533_ -- --nocapture
suite board-waiting 120 cargo test --locked -p okilum-t3 board::waiting::tests:: -- --nocapture
# #2497: explicit non-empty performance/cache and picker verdicts.
suite board-performance 120 cargo test --locked -p okilum-shell --bin okilum project::performance::tests:: -- --nocapture
suite board-cached-live 120 cargo test --locked -p okilum-shell --bin okilum cached_live_ -- --nocapture
suite board-cached-kanban 120 cargo test --locked -p okilum-shell --bin okilum cached_kanban_membership_ -- --nocapture
suite board-filtered-list 120 cargo test --locked -p okilum-shell --bin okilum filtered_list_reuses_projection_ -- --nocapture
suite board-model-interactions 120 cargo test --locked -p okilum-shell --bin okilum project::models::interaction_tests:: -- --nocapture
# Keep issue-peek interactions visible even if a later full-shell test aborts.
suite board-peek 120 cargo test --locked -p okilum-shell --bin okilum peek_2531_ -- --nocapture
suite board-peek-model 120 cargo test --locked -p okilum-t3 peek_2531_ -- --nocapture
# Run the source/input acceptance groups independently, so an unrelated native
# task abort cannot hide their verdict behind an unfinished full shell binary.
# A non-UTC process timezone makes the conflict-copy local timestamp check
# distinguish Local from Utc without mutating global environment in Rust tests.
suite issue2486 120 env TZ=Pacific/Auckland cargo test --locked "${packages[@]}" -p okilum-shell --bin okilum issue2486_ -- --nocapture --test-threads=1
suite direct-origin-tests 120 cargo test --locked -p okilum-sync-controller --lib direct::origin::tests:: -- --nocapture
suite source-presentation 120 cargo test --locked -p okilum-shell --bin okilum source_presentation:: -- --nocapture
# Match hosted concurrency rather than starting a native fixture per heavy CPU.
suite reader-editor-live-preview 120 cargo test --locked -p okilum-shell --bin okilum reader_editor::live_preview:: -- --nocapture --test-threads=4
suite input-anchor-tests 120 cargo test --locked -p okilum-shell --bin okilum input_anchor_tests:: -- --nocapture
suite reader-editor-tables-grid 120 cargo test --locked -p okilum-shell --bin okilum reader_editor::tables::grid:: -- --nocapture
suite reader-editor-lists-key-tests 120 cargo test --locked -p okilum-shell --bin okilum reader_editor::lists::key_tests:: -- --nocapture
suite reader-outline-tests-draft-1320 120 cargo test --locked -p okilum-shell --bin okilum reader_outline::tests::draft_1320_ -- --nocapture
# #2518: explicit non-empty real-socket/paint timing regression before shell-full.
suite manager-ttfm 120 cargo test --locked -p okilum-shell --bin okilum manager_ttfm_ -- --nocapture
# #2045: require the native retention and restore regressions.
suite issue2045 120 cargo test --locked -p okilum-shell --bin okilum issue2045_ -- --nocapture
# #2044: explicit conflict controller and mounted shell acceptance.
suite conflicts 120 cargo test --locked -p okilum-sync-controller --lib conflicts:: -- --nocapture
suite issue2044 120 cargo test --locked -p okilum-shell --bin okilum issue2044_ -- --nocapture --test-threads=1
# Isolate process-wide Log budget and private reveal-bus cases. Serializing the
# entire workspace exceeds this existing deadline on some hosted runners.
# Census every unit/integration/doc case, run the remaining cases at ordinary
# concurrency, and require the full selected inventory in all three processes.
# Assertions, the production pool, internal concurrency and this deadline stay.
suite workspace-full 1320 python3 scripts/ci/workspace-rust-tests.py
# #1915: explicit filters make unsupported-flag coverage inspectable in CI.
suite unix-publish 120 cargo test --locked -p okilum-core --lib unix_publish:: --no-fail-fast
suite file-editor-network-save-tests 120 cargo test --locked -p okilum-core --lib file_editor::network_save_tests:: --no-fail-fast
suite reader-trash-fs-tests-unsupported-flags-trash-and-restore-files-and-folders-without-clobber 120 cargo test --locked -p okilum-shell --bins reader_trash_fs::tests::unsupported_flags_trash_and_restore_files_and_folders_without_clobber --no-fail-fast -- --exact
# #2442: retain process-group generation checks across live -> zombie -> reaped.
suite sidecar-supervisor-process-group 120 cargo test --locked -p okilum-sync-controller --lib sidecar::supervisor::process_group:: --no-fail-fast -- --test-threads=1
# #588: keep shared durable pairing recovery covered on both Unix platforms.
suite enrollment 120 cargo test --locked -p okilum-sync-controller --lib enrollment:: --no-fail-fast
suite enrollment-storage 120 cargo test --locked -p okilum-sync-controller --test enrollment_storage --no-fail-fast
suite sidecar-store-tests-launch-authority 120 cargo test --locked -p okilum-sync-controller --lib sidecar::store::tests::launch_authority:: --no-fail-fast
suite sidecar-macos-recovery 120 cargo test --locked -p okilum-sync-controller --lib sidecar::macos::recovery:: --no-fail-fast
suite sidecar-macos-registration 120 cargo test --locked -p okilum-sync-controller --lib sidecar::macos::registration:: --no-fail-fast
suite sidecar-macos-connect 120 cargo test --locked -p okilum-sync-controller --lib sidecar::macos::connect:: --no-fail-fast
suite sidecar-macos-bundle 120 cargo test --locked -p okilum-sync-controller --lib sidecar::macos::bundle:: --no-fail-fast
suite sidecar-macos-connection 120 cargo test --locked -p okilum-sync-controller --lib sidecar::macos::connection:: --no-fail-fast
suite sidecar-macos-payload 120 cargo test --locked -p okilum-sync-controller --lib sidecar::macos::payload:: --no-fail-fast
suite daemon 120 cargo test --locked -p okilum-sync-controller --lib daemon:: --no-fail-fast
suite presentation 120 cargo test --locked -p okilum-sync-controller --lib presentation:: --no-fail-fast
suite daemon-access 120 cargo test --locked -p okilum-sync-controller --test daemon_access --no-fail-fast
# A second, explicit compiled distribution must refuse updater work even when
# inherited AppImage/update-receipt variables would otherwise enable it.
OKILUM_SPRINT_BUILD=1 OKILUM_RELEASE_VERSION='Sprint CI' \
    APPIMAGE=/nonexistent/OkilumSprint.AppImage OKILUM_APPIMAGE_UPDATE_RECEIPT=/nonexistent/receipt \
    suite sprint-distribution 600 cargo test --locked -p okilum-shell --bin okilum --features linux-sprint sprint_distribution --no-fail-fast -- --test-threads=1
suite shell-no-default-features 600 cargo check -p okilum-shell --no-default-features
suite portable-reader 300 cargo test -p okilum-core --no-default-features --test portable_reader --no-fail-fast
# Vendored gpui-kit tests when the patch stack changes (otherwise a skip line).
suite vendor-tests 900 bash scripts/ci/vendor-tests.sh
# #1924: injected no-system-Trash fallback is shared across Unix.
suite reader-trash-fs-vault-fallback-tests 120 cargo test --locked -p okilum-shell --bins reader_trash_fs::vault_fallback_tests:: --no-fail-fast
suite reader-trash-fs-vault-trash-inventory-tests 120 cargo test --locked -p okilum-shell --bins reader_trash_fs::vault_trash_inventory_tests:: --no-fail-fast

# #588: one-use fresh preparation never regenerates partial state.
suite sidecar-macos-preparation 120 cargo test --locked -p okilum-sync-controller --lib sidecar::macos::preparation:: --no-fail-fast
exit "$failed"
