#!/usr/bin/env bash
# Four independent warm Cargo targets; runner capacity queues work before admission.
set -euo pipefail
slot_root=${OKILUM_FAST_SLOT_ROOT:-"$HOME/.cache/okilum-fast/slots"}
slots=${OKILUM_FAST_SLOTS:-4}
wait_seconds=${OKILUM_FAST_SLOT_WAIT_SECONDS:-20}
[[ $slots =~ ^[1-6]$ && $wait_seconds =~ ^[0-9]+$ && $wait_seconds -le 240 ]]
mkdir -p "$slot_root"
run_number=${GITHUB_RUN_NUMBER:-0}
[[ $run_number =~ ^[0-9]+$ ]] || run_number=0
start=$((run_number % slots))
deadline=$((SECONDS + wait_seconds))
while true; do
    for ((offset=0; offset<slots; offset++)); do
        slot=$(((start + offset) % slots))
        exec 9>"$slot_root/$slot.lock"
        if flock -n 9; then
            export OKILUM_FAST_SLOT=$slot
            export CARGO_TARGET_DIR="$slot_root/$slot/target"
            # Four fast jobs consume at most twelve compiler workers on baldr.
            export CARGO_BUILD_JOBS=3 RAYON_NUM_THREADS=3
            mkdir -p "$CARGO_TARGET_DIR"
            echo "FAST_LOCAL_SLOT=$slot target=$CARGO_TARGET_DIR compiler_workers=$CARGO_BUILD_JOBS"
            # The outer shell owns the lock. Daemons/compiler subprocesses must
            # not inherit it and retain a slot after their build has completed.
            source scripts/ci/release-cache.sh 9>&-
            result=0
            bash scripts/ci/fast.sh 9>&- || result=$?
            release_cache_stats 9>&-
            exit "$result"
        fi
        exec 9>&-
    done
    if ((SECONDS >= deadline)); then
        echo 'All local fast targets are busy; no compile/test verdict' >&2
        exit 75
    fi
    sleep 1
done
