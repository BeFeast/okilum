#!/usr/bin/env bash
# Process v2: real desktop build + deterministic parsing unit tests, no native suites.
set -euo pipefail
source "$(dirname "$0")/test-nofile.sh"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
scripts/vendor-setup.sh
scripts/vendor-setup.sh --verify
export OKILUM_SOURCE_COMMIT="${GITHUB_SHA:?}"
if [ "${RUNNER_OS:-}" = Windows ]; then
    mkdir -p target/fast
    python scripts/ci/test-icon.py target/fast/okilum.ico
    export OKILUM_WINDOWS_ICON="$(cygpath -w "$PWD/target/fast/okilum.ico")"
fi
build_args=()
if [ "${RUNNER_OS:-}" = Windows ]; then build_args=(--no-default-features); fi
if [ "${RUNNER_OS:-}" = macOS ]; then
    export CC=/usr/bin/clang CXX=/usr/bin/clang++
    export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
    python3 scripts/updater/sparkle.py fetch "$RUNNER_TEMP/Sparkle.tar.xz"
    python3 scripts/updater/sparkle.py prepare --archive "$RUNNER_TEMP/Sparkle.tar.xz" --destination vendor/sparkle
fi
cargo build --locked -p okilum-shell -p okilum-sync-supervisor "${build_args[@]}"
# Pure HTML/Markdown parsing tests, no UI, process trees, network or timers.
cargo test --locked -p okilum-paste --lib -- --list | tee "${RUNNER_TEMP:?}/fast-tests.txt"
grep -q ': test$' "$RUNNER_TEMP/fast-tests.txt"
cargo test --locked -p okilum-paste --lib -- --test-threads=1
