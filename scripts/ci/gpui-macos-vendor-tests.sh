#!/usr/bin/env bash
# Run the pinned gpui-pre-macos crate's own unit tests on macOS (#2825).
#
# The crate is a [patch.crates-io] path dependency outside the workspace, so
# `cargo test -p` cannot enable its `test-support` feature (its cfg(test) code
# needs gpui_apple's). Test a byte-identical copy through its own manifest,
# with the patched gpui-pre-apple and the workspace lock as the starting
# versions. The generated vendor tree is verified before and after and never
# receives a lockfile or target directory.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
[ "$(uname -s)" = Darwin ] || { echo "gpui-macos-vendor-tests: macOS only" >&2; exit 2; }

python3 scripts/gpui-pre-apple-setup.py --verify
work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/gpui-macos-vendor-tests.XXXXXX")"
cp -R vendor/gpui-pre-macos-0.3.3 "$work/crate"
cp Cargo.lock "$work/crate/Cargo.lock"
log="$work/test.log"

# Run from the repository root so rust-toolchain.toml selects the pinned compiler.
CARGO_TARGET_DIR="$ROOT/target/gpui-macos-vendor-tests" cargo test \
    --manifest-path "$work/crate/Cargo.toml" \
    --config "patch.crates-io.gpui-pre-apple.path='$ROOT/vendor/gpui-pre-apple-0.3.3'" \
    --features test-support --lib -- --test-threads=1 2>&1 | tee "$log"

# A green run must have executed the patch's own test and the whole suite.
grep -q 'window::tests::tab_group_is_queried_once_tabbing_may_be_active_and_stays_queried \.\.\. ok' "$log" \
    || { echo "gpui-macos-vendor-tests: tabbing-flag test did not run" >&2; exit 1; }
passed="$(sed -nE 's/^test result: ok\. ([0-9]+) passed.*/\1/p' "$log" | tail -1)"
[ "${passed:-0}" -ge 8 ] \
    || { echo "gpui-macos-vendor-tests: expected at least 8 passing tests, got ${passed:-0}" >&2; exit 1; }

python3 scripts/gpui-pre-apple-setup.py --verify
echo "GPUI_MACOS_VENDOR_TESTS_PASS passed=$passed"
