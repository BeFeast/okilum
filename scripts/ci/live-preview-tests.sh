#!/usr/bin/env bash
# Exact-head regression gate for the source projection / native patch boundary.
# Called by fast.sh (OKILUM_LIVE_PREVIEW_TESTS=1) inside its source, compiler and target.
set -euo pipefail
scripts/vendor-setup.sh --verify
# A renamed filter must fail loudly instead of silently running zero tests.
cargo test --locked -p okilum-shell --bin okilum reader_editor::live_preview:: -- --list > "$RUNNER_TEMP/live-preview-tests.txt"
for test in large_note_native_typing_ime_undo_and_scroll long_note_native_typing_ime_undo_and_scroll; do
    grep -q "::$test: test$" "$RUNNER_TEMP/live-preview-tests.txt"
done
# Execute independent suites even after an assertion fails, then fail the gate.
# One cold test-binary build should report all regressions, not just the first.
failures=0
run_suite() {
    "$@" || failures=$((failures + 1))
}
run_suite cargo test --locked -p okilum-core --lib source_projection::
run_suite cargo test --locked -p okilum-core --lib source_classifier::
run_suite cargo test --locked -p okilum-core --lib file_editor::
run_suite cargo test --locked -p okilum-shell --bin okilum source_presentation::
run_suite cargo test --locked -p okilum-shell --bin okilum reader_editor::live_preview:: -- --nocapture
run_suite cargo test --locked -p okilum-shell --bin okilum input_anchor_tests::
run_suite cargo test --locked -p okilum-shell --bin okilum reader_editor::tables::grid::
# Vendor workspace lock resolution follows vendor-tests.sh: patches add deps.
# Root Cargo.lock and gpui core remain unchanged.
if cargo test --manifest-path vendor/gpui-component/Cargo.toml -p gpui-base --lib input::display_map::projected_cache:: -- --list > "$RUNNER_TEMP/projected-cache-tests.txt"; then
    grep -q '::dirty_install_uses_real_wrapping_and_matches_cold_after_edit_resize_and_undo: test$' "$RUNNER_TEMP/projected-cache-tests.txt"
    run_suite cargo test --manifest-path vendor/gpui-component/Cargo.toml -p gpui-base --lib input::display_map::projected_cache::
    run_suite cargo test --manifest-path vendor/gpui-component/Cargo.toml -p gpui-base --lib input::projection_layout::
else
    failures=$((failures + 1))
fi
printf 'LIVE_PREVIEW_REGRESSION_FAILURES=%s\n' "$failures"
test "$failures" = 0
