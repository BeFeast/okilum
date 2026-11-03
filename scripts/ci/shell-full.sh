#!/usr/bin/env bash
# Full headless Linux shell runtime suite, outside the bounded desktop gate.
set -euo pipefail
source "$(dirname "$0")/test-nofile.sh"
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export OKILUM_SOURCE_COMMIT="${GITHUB_SHA:?}"
phase() {
    local name=$1 limit=$2 started=$SECONDS result=0
    shift 2
    echo "FAST_PHASE_START=$name unix=$(date +%s)"
    bash scripts/ci/test-group.sh "$name" "$limit" "$@" || result=$?
    echo "FAST_PHASE_END=$name elapsed=$((SECONDS - started)) status=$result"
    return "$result"
}
scripts/vendor-setup.sh
scripts/vendor-setup.sh --verify
# Resolve locked test dependencies and compile even when this job gets a different
# warm slot from check. The inventory is a positive control against an empty run.
phase shell-binary-list 360 cargo test --locked -p okilum-shell --bin okilum -- --list | tee "${RUNNER_TEMP:?}/shell-tests.txt"
grep -q ': test$' "$RUNNER_TEMP/shell-tests.txt"
# Match hosted validation, with no test-name filter or new ignored tests.
phase shell-binary-tests 900 cargo test --offline --locked -p okilum-shell --bin okilum -- --test-threads=4
