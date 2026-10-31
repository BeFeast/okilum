#!/bin/bash
# Native PR gate: no signing, packaging, notarization or publishing.
set -Eeuo pipefail
[[ $(uname -s) = Darwin && $(uname -m) = arm64 ]]
# Exercise optional signing arrays with macOS /bin/bash 3.2 before any Cargo work.
python3 scripts/ci/test_supervisor_bundle.py
# Pure page guard units; no GUI/NSApp activation and no production browser data.
node scripts/ci/test-manager-page-keys.mjs
if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
export CC=/usr/bin/clang CXX=/usr/bin/clang++
export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
# #1914: exercise the actual Service provider with isolated AppKit pasteboards.
folder_test=$(mktemp -d)
xcrun clang -fobjc-arc -Wall -Werror -framework AppKit \
    crates/okilum-shell/src/folder_service/test.m -o "$folder_test/service-test"
"$folder_test/service-test"
rm "$folder_test/service-test"
rmdir "$folder_test"

# Same toolchain/profile/cache as main releases; Cargo locks the shared target.
export CARGO_TARGET_DIR="$HOME/.cache/okilum-macos/reader-arm64"
export CARGO_INCREMENTAL=0
rustc --version
rustup target add aarch64-apple-darwin
export OKILUM_SCCACHE_ISOLATED=1
source scripts/ci/release-cache.sh
bash scripts/vendor-setup.sh
bash scripts/vendor-setup.sh --verify
archive="${RUNNER_TEMP:?}/Sparkle-2.10.0.tar.xz"
python3 scripts/updater/sparkle.py fetch "$archive"
python3 scripts/updater/sparkle.py prepare --archive "$archive" --destination vendor/sparkle
export DYLD_FRAMEWORK_PATH="$PWD/vendor/sparkle"
# #1603: only the hosted lane runs the isolated signed update/relaunch fixture.
# Local/corporate runners must not execute it.
if [[ "${GITHUB_ACTIONS:-}" == true && "${GITHUB_SERVER_URL:-}" == https://github.com ]]; then
    python3 scripts/ci/sparkle-freshness/run.py "$archive"
fi
cargo_args=(test --release --locked --target aarch64-apple-darwin)
# Compile the production binary too: cargo test alone only builds the shell
# with cfg(test), because this crate has no integration-test targets.
cargo build --release --locked --target aarch64-apple-darwin -p okilum-shell
# Compile test-only cfg branches and integration tests.
# Core and shell tests below select exactly this package pair: the same selection
# unifies the same features (and dev-dependencies), so they reuse these binaries.
# Selecting one package at a time rebuilt core and shell, about 7 minutes per PR.
SHARED=(-p okilum-core -p okilum-shell)
cargo "${cargo_args[@]}" "${SHARED[@]}" --no-run

run_tests() {
    local package="$1" target="$2" filter="$3"
    shift 3
    local selection=(-p "$package")
    case "$package" in okilum-core|okilum-shell) selection=("${SHARED[@]}") ;; esac
    # Positive control: a renamed/removed filter must not silently run zero tests.
    cargo "${cargo_args[@]}" "${selection[@]}" "$target" "$filter" -- --list "$@" \
        | tee "${RUNNER_TEMP}/macos-test-list.txt"
    grep -q ': test$' "${RUNNER_TEMP}/macos-test-list.txt"
    cargo "${cargo_args[@]}" "${selection[@]}" "$target" "$filter" -- --test-threads=1 "$@"
}
# APFS rejects invalid UTF-8 filenames even though Unix OsString can represent them.
run_tests okilum-core --lib link_rewrite::tests::sidecars_stay_excluded_and_invalid_utf8_remains_searchable_without_rewriting --exact
for name in \
    completed_exchange_before_journal_acknowledgement_recovers_without_conflict \
    immediate_reopen_releases_lock_despite_an_inherited_descriptor \
    pending_draft_retains_exclusion_until_it_finishes_then_reopens_immediately \
    destination_reservation_protects_orphaned_drafts_and_active_writers; do
    run_tests okilum-core --lib "file_editor::tests::$name" --exact
done
# #1120: one note that is not valid UTF-8 must not block links between readable notes.
# The integration test is its own target; the positive control keeps a renamed test from
# passing by running nothing.
cargo "${cargo_args[@]}" "${SHARED[@]}" --test prepared_links a_non_utf8_note_leaves_links_between_readable_notes_working -- --list \
    | tee "${RUNNER_TEMP}/macos-test-list.txt"
grep -q ': test$' "${RUNNER_TEMP}/macos-test-list.txt"
cargo "${cargo_args[@]}" "${SHARED[@]}" --test prepared_links a_non_utf8_note_leaves_links_between_readable_notes_working -- --test-threads=1 --exact
run_tests okilum-shell --bins document_link_landing_tests::one_non_utf8_note_does_not_block_links_between_readable_notes --exact
run_tests okilum-shell --bins document_link_landing_tests::hover_and_navigation_agree_in_a_vault_with_a_non_utf8_note_also_on_a_warm_start --exact
# #1234: the first click on a link after a word selection (press and release in one frame).
run_tests okilum-shell --bins prepared_links::tests::the_first_click_on_a_link_opens_it_after_a_word_selection --exact
# #1248: the .stignore offer for a Syncthing vault (core detection and append, the offer, Settings).
run_tests okilum-core --lib syncthing_ignore::
run_tests okilum-shell --bins reader_stignore::
run_tests okilum-shell --bins reader_settings::tests::settings_files_shows_the_syncthing_ignore_state_and_its_button --exact
# The shell has a single binary target; avoid example/test harnesses with zero matches.
# #1510: dispatch unit tests run on hosted macOS; the real-NSApp fixture is
# ignored because hosted runners cannot establish an active/key application.
run_tests okilum-shell --bins reader_manager_focus::tests
run_tests okilum-shell --bins platform::exact_macos_clipboard::native_tests
run_tests okilum-shell --bins reader_replay::
# #1462: use the platform NewNote shortcut after an external folder deletion.
run_tests okilum-shell --bins reader_create::tests::deleted_folder_keyboard_creation_preserves_draft --exact
# #1731: real case-insensitive APFS note/folder rename, link rewriting and Undo.
run_tests okilum-core --lib note_move::case_rename_native_tests::
# #1729: exercise the native NSFileManager Trash and Undo path for a real folder.
run_tests okilum-shell --bins reader_trash_fs::tests::
run_tests okilum-shell --bins reader_trash::tests::unlinked_folder_moves_without_dialog_and_undo_restores_contents --exact
# #1110 (AGENTS 9a): first run → choose a vault → a window always exists; this
# replaces the manual first-launch check on every PR. #1137: a reopen with no
# window restores the last vault or the start screen.
for name in \
    first_run_choosing_a_vault_folder_keeps_a_window_until_it_opens \
    explicit_delivery_replaces_first_run_entry \
    reopen_without_windows_restores_the_last_vault_or_the_start_screen; do
    run_tests okilum-shell --bins "reader_open::entry_tests::$name" --exact
done
# #912: Tasks labels keep letter descenders with macOS font metrics too.
run_tests okilum-shell --bins reader_tasks::tests::task_labels_fit_descenders --exact

# #588: the managed-sidecar library is compiled into the shell, but its tests only ran on
# Linux and Windows. These cover the Unix state store (flock, atomic replace), the
# process-group owned tree (real process tree, signals, process scan via libproc) and
# the private socket transport (LOCAL_PEERCRED); each needs the real macOS kernel.
run_tests okilum-sync-controller --lib sidecar::
# #588: the durable pairing journal is shared with macOS, independently of the
# Linux systemd runtime. Exercise actual private-file/lock operations on this OS.
run_tests okilum-sync-controller --lib enrollment::
cargo "${cargo_args[@]}" -p okilum-sync-controller --test enrollment_storage -- --list \
    | tee "${RUNNER_TEMP}/macos-test-list.txt"
grep -q ': test$' "${RUNNER_TEMP}/macos-test-list.txt"
cargo "${cargo_args[@]}" -p okilum-sync-controller --test enrollment_storage -- --test-threads=1
# #588: authenticated daemon inspection and folder state are shared Unix code.
run_tests okilum-sync-controller --lib presentation::
cargo "${cargo_args[@]}" -p okilum-sync-controller --test daemon_access -- --list \
    | tee "${RUNNER_TEMP}/macos-test-list.txt"
grep -q ': test$' "${RUNNER_TEMP}/macos-test-list.txt"
cargo "${cargo_args[@]}" -p okilum-sync-controller --test daemon_access -- --test-threads=1
# #1013: the supervisor's run loop against a real process tree and real sockets (the
# shell does not depend on this crate, so nothing else would even build it here).
run_tests okilum-sync-supervisor --tests ""
# #1083: the exit path, deterministically (the integration tests above only race it).
run_tests okilum-sync-supervisor --lib serve::unix::tests::a_connection_pending_under_the_state_lock_does_not_block_clearing_the_hint --exact
# #1013: the supervisor's packaging steps, as the release runs them, on a real Mac.
bash scripts/ci/check-supervisor-bundle.sh

# #477: real Quick Look providers, bounded Retina output and cancellation.
export OKILUM_THUMBNAIL_EVIDENCE_DIR="${RUNNER_TEMP}/thumbnail-evidence"
run_tests okilum-shell --bins reader_thumbnail::native_tests
# #1307: a build that never chose a channel follows stable; a Beta choice stays.
run_tests okilum-shell --bins updater::macos::tests::channel_defaults_to_stable_and_keeps_an_explicit_choice --exact

release_cache_stats

# #1583: Settings progress/ready/restart controls and the shared update state machine.
run_tests okilum-shell --bins updater::status::
run_tests okilum-shell --bins updater::progress_ui_tests::

# Real GPUI editor protection at the same final boundary used by both Quit and Restart.
if [[ "${GITHUB_ACTIONS:-}" == true && "${GITHUB_SERVER_URL:-}" == https://github.com ]]; then
    run_tests okilum-shell --bins reader_editor::tests::update_install_post_verification_protects_latest_edits_and_refuses_failure --exact
fi

# #1691: release fixture must use canonical macOS /var -> /private/var roots.
# Exercise the same vault-switch save/conflict guard before beta publication.
run_tests okilum-shell --bins reader_editor::tests::replacing_vault_saves_dirty_source_and_refuses_conflict --exact

# #1915: unsupported rename flags must exercise the compatible path on every Mac.
run_tests okilum-core --lib unix_publish::
run_tests okilum-core --lib file_editor::network_save_tests::
# #1934: the native kernel must exercise byte-exact Repair/Restore, including
# rollback and unsupported-EXCHANGE failures, plus the actual Reader buttons.
run_tests okilum-core --lib file_editor::raw_transaction::
run_tests okilum-core --lib source_history::
run_tests okilum-core --lib vault::preimage_visibility_tests::appledouble_is_excluded_before_lossy_utf8_inventory_and_display --exact
run_tests okilum-shell --bins network_write_qa::
run_tests okilum-shell --bins reader_encoding::tests::repair_button_unlocks_edit_and_history_restores_the_original_bytes --exact
run_tests okilum-shell --bins reader_encoding::tests::appledouble_does_not_enter_tree_or_offer_encoding_repair --exact
run_tests okilum-shell --bins reader_trash_fs::tests::unsupported_flags_trash_and_restore_files_and_folders_without_clobber --exact
# Manager-authorized native mount gate only on the local M4 runner. Hosted Macs
# have no corporate mount; never substitute local APFS for this acceptance.
if [[ "${OKILUM_NETWORK_WRITE_QA:-0}" == 1 ]]; then
    [[ "${GITHUB_SERVER_URL:-}" == https://git.oklabs.uk ]]
    test -d /Volumes/Maestro/okilum-nfs-qa
    cargo run --release --locked --target aarch64-apple-darwin -p okilum-shell \
        --example network_write_acceptance -- /Volumes/Maestro/okilum-nfs-qa
fi
if [[ "${OKILUM_ENCODING_REPAIR_QA:-0}" == 1 ]]; then
    [[ "${GITHUB_SERVER_URL:-}" == https://git.oklabs.uk ]]
    test -d /Volumes/Maestro/okilum-nfs-qa
    cargo run --release --locked --target aarch64-apple-darwin -p okilum-shell \
        --example network_write_acceptance -- /Volumes/Maestro/okilum-nfs-qa --encoding-repair
fi
# #1924: no-Trash fallback, collision-safe Undo/Redo, and hidden tree/search.
run_tests okilum-shell --bins reader_trash_fs::vault_fallback_tests::
run_tests okilum-shell --bins reader_trash_fs::vault_trash_inventory_tests::

# #588: one-use fresh preparation never regenerates partial state.
run_tests okilum-sync-controller --lib sidecar::macos::preparation::
