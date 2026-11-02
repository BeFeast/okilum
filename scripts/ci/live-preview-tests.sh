#!/usr/bin/env bash
# Exact-head regression gate for the source projection / native patch boundary.
# Called inside fast-local.sh's existing source, compiler and target lock.
set -euo pipefail
scripts/vendor-setup.sh --verify
# A renamed filter must fail loudly instead of silently running zero tests.
cargo test --locked -p okilum-shell --bin okilum reader_editor::live_preview:: -- --list > "$RUNNER_TEMP/live-preview-tests.txt"
for test in large_note_native_typing_ime_undo_and_scroll long_note_native_typing_ime_undo_and_scroll; do
    grep -q "::$test: test$" "$RUNNER_TEMP/live-preview-tests.txt"
done
cargo test --locked -p okilum-core --lib source_projection::
cargo test --locked -p okilum-core --lib source_classifier::
cargo test --locked -p okilum-core --lib file_editor::
cargo test --locked -p okilum-shell --bin okilum source_presentation::
cargo test --locked -p okilum-shell --bin okilum reader_editor::live_preview:: -- --nocapture
cargo test --locked -p okilum-shell --bin okilum input_anchor_tests::
# Vendor workspace lock resolution follows vendor-tests.sh: patches add deps.
# Root Cargo.lock and gpui core remain unchanged.
cargo test --manifest-path vendor/gpui-component/Cargo.toml -p gpui-base --lib input::editor::display_map::projected_cache:: -- --list > "$RUNNER_TEMP/projected-cache-tests.txt"
grep -q '::dirty_install_uses_real_wrapping_and_matches_cold_after_edit_resize_and_undo: test$' "$RUNNER_TEMP/projected-cache-tests.txt"
cargo test --manifest-path vendor/gpui-component/Cargo.toml -p gpui-base --lib input::editor::display_map::projected_cache::
