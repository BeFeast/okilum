#!/usr/bin/env bash
# One shard of the full Linux agent gate on a build-slot host (Oleg 21.11:
# 4-6 shards in parallel on different pool slots, <= 20 min for the verdict).
# The hosted job's apt, SDK reclaim and Actions cache steps are not repeated:
# the pool image already has the toolchain, and sccache plus the warm slot
# target replace the Actions cache. Phase names match the GitHub steps the
# merge rule still requires; pool-aggregate.py merges the shard receipts.
#
#   base       docker   AppImage runtime, Python suites, Help/brand, admission crash checkpoints
#   admission  tmpfs,pidns  PID-namespace controls, admission remaining-0 + negative controls, real disk-full transfer
#   sync       -        source sharing, admission remaining-1, local writer authority
#   desktop    -        clippy, fmt + desktop build, sync serve pairing, reveal stress, vault regression
#   rust-a     userbus  okilum-core suite, Linux Rust groups outside the workspace census
#   rust-b     userbus  Linux Rust workspace census
#   all        docker,sudo,userbus,pidns  every phase in one slot (manual runs and the benchmark branch)
set -euo pipefail
shard=${1:-all}
case $shard in
    base|admission|sync|desktop|rust-a|rust-b|all) ;;
    *) echo "unknown linux shard: $shard" >&2; exit 64 ;;
esac
export OKILUM_POOL_SHARD=$shard
# shellcheck source=scripts/ci/pool-phase-lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/pool-phase-lib.sh"
pool_receipt_init
pool_ensure_git

# Admission's host-discovery run and its controls need capability pidns,
# probed in this slot (pool_require_pidns, pool-phase-lib.sh).
case $shard in
    admission|all) pool_require_pidns ;;
esac

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
    vendor_lock=$(mktemp)
    cp vendor/gpui-component/Cargo.lock "$vendor_lock"
    set +e
    cargo fetch --manifest-path vendor/gpui-component/Cargo.toml
    code=$?
    if [[ $code -eq 0 ]]; then
        # shellcheck source=scripts/ci/vendor-gpui.sh
        (source scripts/ci/vendor-gpui.sh && cargo fetch --manifest-path vendor/gpui-component/Cargo.toml "${VENDOR_GPUI[@]}")
        code=$?
    fi
    set -e
    cp "$vendor_lock" vendor/gpui-component/Cargo.lock
    rm -f "$vendor_lock"
    return "$code"
}

prepare_user_bus() {
    local runtime="/run/user/$(id -u)"
    if [[ ! -S $runtime/bus ]]; then
        echo "systemd user bus is not available at $runtime/bus" >&2
        return 1
    fi
    export XDG_RUNTIME_DIR=$runtime
    export OKILUM_REQUIRE_SYSTEMD_USER=1
    systemctl --user --version | head -1
}

has() {
    [[ $shard == all ]] && return 0
    local name
    for name in "$@"; do
        [[ $shard == "$name" ]] && return 0
    done
    return 1
}

# Every shard checks the toolchain and prepares the same vendor, locked
# dependencies and Python environment; their receipts must all pass.
pool_phase "Verify pinned Rust toolchain" verify_toolchain
if has base; then
    pool_phase "Verify per-user AppImage runtime" \
        bash scripts/appimage/build-runtime.sh "${TMPDIR:-/tmp}/appimage-runtime" --controls
fi
pool_phase "Prepare and verify pinned vendor and comrak" bash -c '
    source scripts/ci/pool-phase-lib.sh && pool_reset_stale_vendor &&
    bash scripts/vendor-setup.sh &&
    bash scripts/vendor-setup.sh --verify &&
    pool_reset_stale_vendor stamp &&
    python3 scripts/ci/check-vendor-generation.py
'
pool_phase "Fetch locked public dependencies" fetch_locked
export CARGO_NET_OFFLINE=true
pool_phase "Prepare isolated Python test dependencies" pool_prepare_python
if has base; then
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
        python3 scripts/rebrand/check_brand.py &&
        python3 scripts/ci/shell-crate-budget.py
    '
fi
if has desktop; then
    pool_phase "Lint every workspace target offline" \
        cargo clippy --workspace --all-targets --locked -- -D warnings
fi
if has sync; then
    pool_phase "Verify pinned Sync fixture lifecycle, source sharing and remote inspection" bash -c '
        # shellcheck source=scripts/ci/test-nofile.sh
        source scripts/ci/test-nofile.sh
        python3 scripts/ci/fetch-ci-dependency.py syncthing "${XDG_CACHE_HOME:-$HOME/.cache}/okilum-sync-fixture/syncthing-linux-amd64-v2.1.6.tar.gz"
        # Compile the exact test targets first: on hosted the Actions cache made
        # this a no-op, and the 600 s deadline below is for the fixtures.
        cargo test --locked -p okilum-sync-controller --lib --test direct_bidirectional --no-run
        tini -s -- bash scripts/ci/test-group.sh agent-sync-sharing 600 bash scripts/ci/sync-source-sharing.sh --runs 2 --parallel 2
    '
fi
if has admission; then
    # Only here (Oleg 21.11 via the manager): mandatory, on a probed pidns host.
    pool_phase "Verify PID-namespace isolation controls" \
        env OKILUM_PIDNS_CONTROLS=1 python3 -I -m unittest discover -s scripts/ci -p test_pid_namespace.py -v
fi
# Managed admission is split into three processes on three shards
# (sync-managed-admission-runs.py): each keeps its own unchanged 600 s
# deadline, and the selected parent inventory is still checked in full.
admission_part=
case $shard in
    base) admission_part=crash ;;
    admission) admission_part=remaining-0 ;;
    sync) admission_part=remaining-1 ;;
    all) admission_part=all ;;
esac
if [[ -n $admission_part ]]; then
    pool_phase "Verify managed launch and configuration admission" \
        env OKILUM_ADMISSION_PART="$admission_part" bash scripts/ci/sync-managed-admission.sh
fi
if has admission; then
    # The pool slot is disposable for this mount: a private 16 MiB tmpfs under
    # the slot TMPDIR, unmounted by the script (needs host capability tmpfs).
    pool_phase "Verify real disk-full Sync transfer and first merge" bash -c '
        cargo test --locked -p okilum-sync-controller --test managed_launch --no-run &&
        OKILUM_POOL_DISK_FULL=1 bash scripts/ci/test-group.sh agent-sync-disk-full 600 bash scripts/ci/sync-disk-full.sh
    '
fi
pool_phase "Reject and reap surviving agent fixture daemons" \
    python3 scripts/ci/sync-fixture-processes.py --root "${TMPDIR:-/tmp}" --cleanup
if has sync; then
    pool_phase "Verify local writer authority and fencing" \
        bash scripts/ci/test-group.sh agent-write-authority 120 cargo test --offline --locked -p okilum-write-authority local_writer_admission -- --nocapture
fi
if has desktop; then
    pool_phase "Build Linux desktop offline" bash -c '
        cargo fmt --check &&
        cargo build --offline --locked -p okilum-shell -p okilum-sync-supervisor
    '
    pool_phase "Pair through a running okilum sync serve" bash -c '
        syncthing=$(python3 scripts/ci/sync-fixture-stage.py)
        tini -s -- python3 -I scripts/ci/sync-cli-control.py --okilum "${CARGO_TARGET_DIR:-target}/debug/okilum" --syncthing "$syncthing"
    '
    pool_phase "Stress private reveal bus (20 runs)" \
        bash scripts/ci/test-group.sh agent-reveal-stress 1800 python3 scripts/ci/reveal-stress.py --runs 20
fi
if has rust-a; then
    pool_phase "Run complete okilum-core suite offline" bash -c '
        # shellcheck source=scripts/ci/test-nofile.sh
        source scripts/ci/test-nofile.sh
        bash scripts/ci/test-group.sh agent-core 480 cargo test --offline --locked -p okilum-core
    '
fi
rust_part=
case $shard in
    rust-a) rust_part=checks ;;
    # Two ordinary census processes as on hosted: four were OOM-killed at the
    # 8 GiB slot limit (run 44837997588, rust-b on baldr).
    rust-b) rust_part=workspace ;;
    all) rust_part=all ;;
esac
if [[ -n $rust_part ]]; then
    pool_phase "Start a systemd user manager for updater unit controls" prepare_user_bus
    pool_phase "Run Linux Rust suites offline" env OKILUM_LINUX_RUST_PART="$rust_part" bash -c '
        # shellcheck source=scripts/ci/test-nofile.sh
        source scripts/ci/test-nofile.sh
        tini -s -- bash scripts/ci/test-group.sh agent-linux-rust 3000 bash scripts/ci/linux-rust-tests.sh
    '
fi
if [[ ${OKILUM_POOL_BENCHMARK:-} == 1 ]]; then
    if has all; then
        pool_phase "Warm build Linux desktop offline" \
            cargo build --offline --locked -p okilum-shell -p okilum-sync-supervisor
        pool_phase "Warm complete okilum-core suite offline" bash -c '
            # shellcheck source=scripts/ci/test-nofile.sh
            source scripts/ci/test-nofile.sh
            bash scripts/ci/test-group.sh agent-core 480 cargo test --offline --locked -p okilum-core
        '
        pool_phase "Warm Linux Rust suites offline" bash -c '
            # shellcheck source=scripts/ci/test-nofile.sh
            source scripts/ci/test-nofile.sh
            tini -s -- bash scripts/ci/test-group.sh agent-linux-rust 3000 bash scripts/ci/linux-rust-tests.sh
        '
    fi
elif has desktop; then
    pool_phase "Verify additive vault regression controls" python3 scripts/ci/at-vault-regression-control.py
fi
if [[ -n ${OKILUM_PUBLICATION_BASELINE:-} ]] && has desktop; then
    pool_phase "Compare editor publication with pinned main on this host" bash -c '
        # shellcheck source=scripts/ci/test-nofile.sh
        source scripts/ci/test-nofile.sh
        [[ "$OKILUM_PUBLICATION_BASELINE" =~ ^[0-9a-f]{40}$ ]]
        git fetch --no-tags https://github.com/BeFeast/okilum.git "$OKILUM_PUBLICATION_BASELINE"
        python3 scripts/perf/editor/publication-controls.py "$OKILUM_PUBLICATION_BASELINE"
    '
fi
pool_finish
