#!/usr/bin/env bash
# Production regression groups must actually run, not pass with empty filters.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/test-nofile.sh"
# One package selection keeps the dependency feature closure stable throughout
# the compile-inclusive 900s admission. Do not repeatedly rebuild shell/core
# dependencies with each single-package selection.
packages=(-p okilum-write-authority -p okilum-core -p okilum-shell -p okilum-t3)
run_group() {
    local evidence target=$1
    shift
    evidence=$(mktemp)
    "$@" 2>&1 | tee "$evidence"
    python3 "$(dirname "${BASH_SOURCE[0]}")/require-target-tests.py" "$target" "$evidence"
    unlink "$evidence"
}
run_group okilum_write_authority cargo test --locked "${packages[@]}" local_writer_admission -- --nocapture
run_group okilum_core cargo test --locked "${packages[@]}" --lib file_editor::local_writer_admission_tests -- --nocapture
run_group local_writer_admission cargo test --locked "${packages[@]}" --test local_writer_admission -- --nocapture
run_group okilum cargo test --locked "${packages[@]}" --bin okilum local_writer_admission -- --nocapture
run_group okilum_t3 cargo test --locked "${packages[@]}" local_writer_admission -- --nocapture

# #2507 data-safety follow-up: every reviewed regression group is non-empty.
run_group okilum_core cargo test --locked "${packages[@]}" --lib local_writer_review -- --nocapture
run_group okilum cargo test --locked "${packages[@]}" --bin okilum local_writer_review -- --nocapture
