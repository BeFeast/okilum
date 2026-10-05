#!/bin/bash
# Native PR gate: no signing, packaging, notarization or publishing.
set -Eeuo pipefail
[[ $(uname -s) = Darwin && $(uname -m) = arm64 ]]
if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
export CC=/usr/bin/clang CXX=/usr/bin/clang++
export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
# Same toolchain/profile/cache as main releases; the single runner serializes jobs.
export CARGO_TARGET_DIR="$HOME/.cache/tessera-macos/1.96.1-arm64"
export CARGO_INCREMENTAL=0
if ! rustup run 1.96.1 rustc --version >/dev/null 2>&1; then
    rustup toolchain install 1.96.1 --profile minimal --target aarch64-apple-darwin
fi
bash scripts/vendor-setup.sh
bash scripts/vendor-setup.sh --verify
archive="${RUNNER_TEMP:?}/Sparkle-2.10.0.tar.xz"
python3 scripts/updater/sparkle.py fetch "$archive"
python3 scripts/updater/sparkle.py prepare --archive "$archive" --destination vendor/sparkle
export DYLD_FRAMEWORK_PATH="$PWD/vendor/sparkle"
cargo_args=(+1.96.1 test --release --locked --target aarch64-apple-darwin)
# Compile the production binary too: cargo test alone only builds the shell
# with cfg(test), because this crate has no integration-test targets.
cargo +1.96.1 build --release --locked --target aarch64-apple-darwin -p tessera-shell
# Compile test-only cfg branches and integration tests.
cargo "${cargo_args[@]}" -p tessera-core -p tessera-shell --no-run

run_tests() {
    local package="$1" target="$2" filter="$3"
    shift 3
    # Positive control: a renamed/removed filter must not silently run zero tests.
    cargo "${cargo_args[@]}" -p "$package" "$target" "$filter" -- --list "$@" \
        | tee "${RUNNER_TEMP}/macos-test-list.txt"
    grep -q ': test$' "${RUNNER_TEMP}/macos-test-list.txt"
    cargo "${cargo_args[@]}" -p "$package" "$target" "$filter" -- --test-threads=1 "$@"
}
# APFS rejects invalid UTF-8 filenames even though Unix OsString can represent them.
run_tests tessera-core --lib link_rewrite::tests::sidecars_and_invalid_utf8_do_not_abort_move_or_enter_search --exact
for name in \
    completed_exchange_before_journal_acknowledgement_recovers_without_conflict \
    immediate_reopen_releases_lock_despite_an_inherited_descriptor \
    pending_draft_retains_exclusion_until_it_finishes_then_reopens_immediately \
    destination_reservation_protects_orphaned_drafts_and_active_writers; do
    run_tests tessera-core --lib "file_editor::tests::$name" --exact
done
# The shell has a single binary target; avoid example/test harnesses with zero matches.
run_tests tessera-shell --bins platform::exact_macos_clipboard::native_tests
