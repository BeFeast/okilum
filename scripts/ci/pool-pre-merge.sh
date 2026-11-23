#!/usr/bin/env bash
# Shared pre-merge phases on hosted Linux or a build-slot host: formatting,
# Python suites, clippy, and the release-binary
# check. No Rust test suites and no desktop build.
set -euo pipefail
# shellcheck source=scripts/ci/pool-phase-lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/pool-phase-lib.sh"
pool_receipt_init
pool_ensure_git

export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
export RUSTFLAGS="${RUSTFLAGS:--C link-arg=-fuse-ld=mold}"

verify_toolchain() {
    command -v rustc >/dev/null && command -v cargo >/dev/null || return 75
    rustc -Vv
    cargo clippy --version
    cargo fmt --version
}

fetch_locked() {
    cargo fetch --locked
}

pool_phase "Verify pinned Rust toolchain" verify_toolchain
pool_phase "Prepare and verify pinned vendor and comrak" bash -c '
    source scripts/ci/pool-phase-lib.sh && pool_reset_stale_vendor &&
    bash scripts/vendor-setup.sh &&
    bash scripts/vendor-setup.sh --verify &&
    pool_reset_stale_vendor stamp &&
    python3 scripts/ci/check-vendor-generation.py
'
pool_phase "Fetch locked public dependencies" fetch_locked
export CARGO_NET_OFFLINE=true
pool_phase "Check formatting" cargo fmt --check
pool_phase "Prepare isolated Python test dependencies" pool_prepare_python
pool_phase "Verify agent helper controls" bash -c '
    python3 -m unittest discover -s scripts/ci -p test_agent_checks.py &&
    python3 -m unittest discover -s scripts/ci -p test_test_group.py
'
pool_phase "Run CI, releases and updater Python suites" bash -c '
    python3 scripts/appimage/heic-control.py / &&
    bash scripts/ci/test-group.sh agent-ci-python 300 env OKILUM_UNITTEST_EXCLUDE=test_pid_namespace python3 -I -m unittest discover -s scripts/ci -p "test_*.py" -v &&
    bash scripts/ci/test-group.sh agent-releases-python 90 python3 -I -m unittest discover -s scripts/releases -p "test_*.py" -v &&
    bash scripts/ci/test-group.sh agent-updater-python 90 python3 -I -m unittest discover -s scripts/updater -p "test_*.py" -v &&
    bash scripts/ci/test-group.sh agent-syncthing-python 90 python3 -I -m unittest discover -s scripts/syncthing -p "test_*.py" -v &&
    bash scripts/ci/test-group.sh agent-sprint-python 90 python3 scripts/dev/test-sprint-report.py &&
    bash scripts/ci/test-group.sh agent-qa-python 60 python3 -I -m unittest discover -s scripts/qa -p "test_*.py" -v &&
    bash scripts/ci/test-group.sh agent-inbox-restart-python 60 python3 -I -m unittest discover -s inbox/deploy -p "test_restart.py" -v
'
pool_phase "Verify bundled Sync Help and brand" bash -c '
    python3 docs/help/sync/check.py &&
    python3 docs/help/board/check.py &&
    python3 docs/help/projects/check.py &&
    python3 scripts/rebrand/check_brand.py
'
# Epic #3241: the okilum-shell app crate may not grow past its line budget.
pool_phase "Check okilum-shell size budget" python3 scripts/ci/shell-crate-budget.py
pool_phase "Lint every workspace target offline" \
    cargo clippy --workspace --all-targets --locked -- -D warnings
# Same packages, binaries and profile as scripts/appimage/build.sh, without
# dev-dependency features (#2916).
pool_phase "Check release binaries without test features" \
    cargo check --release --locked -p okilum-shell -p okilum-sync-controller --bin okilum --bin okilum-sync-launch
pool_finish
