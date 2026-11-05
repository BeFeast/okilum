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
# Compilation has a separate, generous budget: listing must never compile or
# wait for Cargo's target lock. Run the exact artifact Cargo reports, rather than
# invoking Cargo again under either execution deadline. See docs/fast-runner.md.
artifacts=$(mktemp "${RUNNER_TEMP:?}/shell-artifacts-XXXXXX")
trap 'rm -f "$artifacts"' EXIT
phase shell-binary-compile 840 bash -c \
    'exec cargo test --locked -p okilum-shell --bin okilum --no-run --message-format=json-render-diagnostics > "$1"' \
    bash "$artifacts"
executable=$(python3 - "$artifacts" <<'PY'
import json, sys
messages = [json.loads(line) for line in open(sys.argv[1])]
matches = [v['executable'] for v in messages if v.get('reason') == 'compiler-artifact'
           and v.get('executable') and v['target']['name'] == 'okilum'
           and v.get('profile', {}).get('test')]
assert len(matches) == 1, matches
print(matches[0])
PY
)
# The inventory is a positive control against an empty run.
# Match cargo test's package working directory for both direct invocations.
manifest_directory="$PWD/crates/okilum-shell"
phase shell-binary-list 30 bash -c 'cd "$1" && shift && exec "$@"' \
    bash "$manifest_directory" "$executable" --list | tee "$RUNNER_TEMP/shell-tests.txt"
grep -q ': test$' "$RUNNER_TEMP/shell-tests.txt"
# Match hosted validation, with no test-name filter or new ignored tests.
# Loaded baldr execution now needs up to ~1700 s (#2589, docs/fast-runner.md).
phase shell-binary-tests 2100 bash -c 'cd "$1" && shift && exec "$@"' \
    bash "$manifest_directory" "$executable" --test-threads=4
