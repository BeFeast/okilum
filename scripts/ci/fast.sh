#!/usr/bin/env bash
# Real desktop build, Linux headless shell suite and parsing unit tests.
set -euo pipefail
source "$(dirname "$0")/test-nofile.sh"
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
phase() {
    local name=$1 started=$SECONDS result=0
    shift
    echo "FAST_PHASE_START=$name unix=$(date +%s)"
    "$@" || result=$?
    echo "FAST_PHASE_END=$name elapsed=$((SECONDS - started)) status=$result"
    return "$result"
}
scripts/vendor-setup.sh
scripts/vendor-setup.sh --verify
export OKILUM_SOURCE_COMMIT="${GITHUB_SHA:?}"
if [ "${RUNNER_OS:-}" = Windows ]; then
    mkdir -p target/fast
    python scripts/ci/test-icon.py target/fast/okilum.ico
    export OKILUM_WINDOWS_ICON="$(cygpath -w "$PWD/target/fast/okilum.ico")"
fi
set -- cargo build --locked -p okilum-shell -p okilum-sync-supervisor
if [ "${RUNNER_OS:-}" = Windows ]; then set -- "$@" --no-default-features; fi
if [ "${RUNNER_OS:-}" = macOS ]; then
    export CC=/usr/bin/clang CXX=/usr/bin/clang++
    export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
    python3 scripts/updater/sparkle.py fetch "$RUNNER_TEMP/Sparkle.tar.xz"
    python3 scripts/updater/sparkle.py prepare --archive "$RUNNER_TEMP/Sparkle.tar.xz" --destination vendor/sparkle
fi
if [ "${RUNNER_OS:-Linux}" = Linux ]; then
    # #2369: report the executed regressions even if a later desktop phase times out.
    phase appimage-channel-tests cargo test --locked -p okilum-appimage --lib channel -- --test-threads=1
fi
phase build "$@"
# Compile every workspace test target; fixture/API drift must block merge even
# when its suite runs only during full overnight acceptance. This also fetches
# test-only dependencies before the offline shell execution on a fresh target.
phase workspace-test-check cargo check --locked --workspace --tests
if [ "${RUNNER_OS:-Linux}" = Linux ]; then
    # #2415: checking test targets cannot detect stale runtime contracts or
    # cross-executor task teardown. Execute the whole headless shell binary.
    phase shell-binary-tests cargo test --offline --locked -p okilum-shell --bin okilum
fi
# Pure HTML/Markdown parsing tests, no UI, process trees, network or timers.
phase list-tests cargo test --locked -p okilum-paste --lib -- --list | tee "${RUNNER_TEMP:?}/fast-tests.txt"
grep -q ': test$' "$RUNNER_TEMP/fast-tests.txt"
phase paste-tests cargo test --locked -p okilum-paste --lib -- --test-threads=1

# The scoped Live Preview gate uses the same locked warm build helper. Ordinary
# fast admission keeps its existing scope; the separate gate executes GUI tests.
if [ "${OKILUM_LIVE_PREVIEW_TESTS:-0}" = 1 ]; then
    phase live-preview-regressions bash scripts/ci/live-preview-tests.sh
fi
