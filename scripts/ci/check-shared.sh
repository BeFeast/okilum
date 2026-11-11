#!/usr/bin/env bash
# Execute every shared gate and preserve every failure in one final summary.
set -uo pipefail
failed=0
results=()

suite() {
    local name=$1 status
    shift
    printf '\n=== %s ===\n' "$name"
    if "$@"; then
        results+=("| $name | PASS | 0 |")
    else
        status=$?
        results+=("| $name | FAIL | $status |")
        failed=1
    fi
}

suite 'Rust formatting' cargo fmt --check
suite 'Workspace clippy' cargo clippy --workspace --all-targets --locked -- -D warnings
# Keep the complete shared workspace inventory, but do not let the host's CPU
# count run dozens of socket/GUI fixtures against each other's deadlines.
suite 'Workspace Rust tests' cargo test --workspace --locked --no-fail-fast -- --test-threads=4 --skip platform:: --skip reader_log
# Log fixtures share a process-wide memory pool. Keep their entire selection
# serial and in another process, as the complete Linux workspace census does.
suite 'Log Rust tests' cargo test --workspace --locked --no-fail-fast reader_log -- --test-threads=1 --skip platform::
suite 'Portable reader' cargo test -p okilum-core --no-default-features --test portable_reader --locked --no-fail-fast
suite 'CI Python tests' python3 -m unittest discover -s scripts/ci -p 'test_*.py'
suite 'Release Python tests' python3 -m unittest discover -s scripts/releases -p 'test_*.py'
suite 'Manifest conversion tests' python3 scripts/manifests/check.py
suite 'Updater Python tests' python3 -m unittest discover -s scripts/updater -p 'test_*.py'

summary() {
    printf '\n## Shared gate results\n\n| Suite | Result | Exit code |\n| --- | --- | --- |\n'
    printf '%s\n' "${results[@]}"
}
summary
if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
    # Failure to write the receipt must not turn a failed job green.
    summary >> "$GITHUB_STEP_SUMMARY" || failed=1
fi
exit "$failed"
