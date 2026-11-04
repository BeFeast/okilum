#!/bin/bash
# Native PR gate: no signing, packaging, notarization or publishing.
set -Eeuo pipefail
[[ $(uname -s) = Darwin && $(uname -m) = arm64 ]]
# Exercise optional signing arrays with macOS /bin/bash 3.2 before any Cargo work.
python3 scripts/ci/test_supervisor_bundle.py
python3 scripts/ci/test_macos_rust_runner.py
# Pure page guard units; no GUI/NSApp activation and no production browser data.
node scripts/ci/test-manager-page-keys.mjs
node scripts/ci/test-manager-webkit-compatibility.mjs
bash scripts/ci/check-manager-key-routing.sh
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
# Keep the workspace selection for filtered core/shell runs too. Selecting only
# that pair drops features from other members' dev-dependencies (e.g. Brain's
# TLS-enabled tungstenite), rebuilding the enormous shell test binary again.
SHARED=(--workspace)

macos_rust_failed=0
run_tests() {
    local package="$1" target="$2" filter="$3"
    shift 3
    local selection=(-p "$package")
    case "$package" in okilum-core|okilum-shell) selection=("${SHARED[@]}") ;; esac
    # Positive control: a renamed/removed filter must not silently run zero tests.
    if ! cargo "${cargo_args[@]}" "${selection[@]}" "$target" "$filter" -- --list "$@" \
        | tee "${RUNNER_TEMP}/macos-test-list.txt"; then
        macos_rust_failed=1
        return 0
    fi
    if ! grep -q ': test$' "${RUNNER_TEMP}/macos-test-list.txt"; then
        echo "No native Rust test matched $package $target $filter" >&2
        macos_rust_failed=1
        return 0
    fi
    cargo "${cargo_args[@]}" "${selection[@]}" "$target" "$filter" -- --test-threads=1 "$@" || macos_rust_failed=1
}

# Diagnostic-only commit: exact production source is the parent of this commit.
# Compile before interpreting negative test failures; a build error is no verdict.
cargo "${cargo_args[@]}" --workspace --no-run
run_tests okilum-shell --bins reader_project_navigation::token_tests::
run_tests okilum-t3 --lib session::tests::native_
run_tests okilum-shell --bins native_pairing_immediately_replaces_the_boards_unpaired_gateway_2520
[[ "$macos_rust_failed" == 0 ]]
python3 scripts/ci/issue-2520-negative-control.py
