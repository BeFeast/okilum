#!/usr/bin/env bash
# Production regression groups must actually run, not pass with empty filters.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/test-nofile.sh"
run_group() {
    local evidence
    evidence=$(mktemp)
    "$@" | tee "$evidence"
    if ! rg -q 'test result: ok\. [1-9][0-9]* passed;' "$evidence"; then
        echo "No non-zero passing test count for local-writer group: $*" >&2
        exit 1
    fi
    unlink "$evidence"
}
run_group cargo test --locked -p okilum-write-authority local_writer_admission -- --nocapture
run_group cargo test --locked -p okilum-core --lib file_editor::local_writer_admission_tests -- --nocapture
run_group cargo test --locked -p okilum-core --test local_writer_admission -- --nocapture
run_group cargo test --locked -p okilum-shell --bin okilum local_writer_admission -- --nocapture
run_group cargo test --locked -p okilum-t3 local_writer_admission -- --nocapture
