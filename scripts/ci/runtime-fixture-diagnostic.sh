#!/usr/bin/env bash
# Observe the failed runtime groups after a successful original cold admission.
set -euo pipefail
tools=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
if [[ ${1:-linux} == shared ]]; then
    # The original entrypoint retains formatting, lint, every workspace target,
    # serial Log, portable reader and all Python/manifest/update gates.
    exec env OKILUM_WORKSPACE_ORDINARY_SHARDS=4 bash scripts/ci/check-shared.sh
fi
[[ ${1:-linux} == linux ]] || exit 2
packages=(-p okilum-write-authority -p okilum-core -p okilum-shell -p okilum-t3)
status=0
guarded() {
    local name=$1 target=$2 evidence
    shift 2
    evidence=$(mktemp)
    if ! bash scripts/ci/test-group.sh --isolated-writer-account "$name" 120 "$@" 2>&1 | tee "$evidence"; then
        status=1
    fi
    if ! python3 "$tools/require-target-tests.py" "$target" "$evidence"; then
        status=1
    fi
    unlink "$evidence"
}
printf 'RUNTIME_FIXTURE_SOURCE %s\n' "$(git rev-parse HEAD)"
guarded priority-source-workers okilum_core cargo test --locked "${packages[@]}" --lib \
    vault::warm::tests::priority_schedules_current_folder_links_and_recent_before_rest_without_reordering_inventory \
    -- --exact --nocapture
guarded cancellation-source-workers okilum_core cargo test --locked "${packages[@]}" --lib \
    vault::warm::tests::parallel_cancellation_drops_a_full_result_queue_before_joining \
    -- --exact --nocapture
guarded reader-editor-live-preview okilum cargo test --locked "${packages[@]}" --bin okilum \
    reader_editor::live_preview:: -- --nocapture --test-threads=4
if ! OKILUM_WORKSPACE_ORDINARY_SHARDS=4 bash scripts/ci/test-group.sh workspace-full 1320 \
        python3 scripts/ci/workspace-rust-tests.py; then
    status=1
fi
exit "$status"
