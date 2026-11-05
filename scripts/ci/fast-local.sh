#!/usr/bin/env bash
# Independent warm Cargo targets; runner capacity queues work before admission.
set -euo pipefail
# Share the existing source/target locks while keeping independent job budgets.
case ${1:-fast} in
    fast) gate_script=fast.sh; gate_limit=2040 ;;
    shell-full) gate_script=shell-full.sh; gate_limit=3030 ;;
    *) echo 'Expected fast or shell-full gate' >&2; exit 64 ;;
esac
slot_root=${OKILUM_FAST_SLOT_ROOT:-"$HOME/.cache/okilum-fast/slots"}
slots=${OKILUM_FAST_SLOTS:-2}
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
            # Stable source must not reuse the legacy ephemeral-checkout target:
            # assets build.rs metadata carries an absolute icons directory through
            # DEP_GPUI_KIT_DEFAULT_ICONS_ICONS_DIR, component rustc-env and macros.
            # Keep the old cache intact; this namespace stays warm across runs.
            export CARGO_TARGET_DIR="$slot_root/$slot/target-stable-source-v1"
            # Two fast jobs consume at most six compiler workers on baldr.
            export CARGO_BUILD_JOBS=3 RAYON_NUM_THREADS=3
            mkdir -p "$CARGO_TARGET_DIR"
            echo "FAST_LOCAL_SLOT=$slot target=$CARGO_TARGET_DIR compiler_workers=$CARGO_BUILD_JOBS"
            # Keep host contention distinguishable from a cold target or lock wait.
            if [[ -r /proc/self/cgroup ]]; then
                cgroup=$(awk -F: '$1 == "0" { print $3 }' /proc/self/cgroup)
                for metric in cpu.weight cpu.pressure memory.pressure; do
                    path="/sys/fs/cgroup$cgroup/$metric"
                    if [[ -r $path ]]; then
                        echo "FAST_LOCAL_RESOURCE=$metric $(tr '\n' ' ' < "$path")"
                    fi
                done
            fi
            # A new hostexecutor path and fresh source mtimes invalidate path
            # crates even when the target is warm. Keep source stable per slot,
            # inside the same existing exclusion lock, before vendor verification.
            source_directory="$slot_root/$slot/source"
            bash scripts/ci/fast-source.sh "$source_directory" 9>&-
            cd "$source_directory"
            # The outer shell owns the lock. Daemons/compiler subprocesses must
            # not inherit it and retain a slot after their build has completed.
            source scripts/ci/release-cache.sh 9>&-
            result=0
            bash scripts/ci/test-group.sh "$gate_script" "$gate_limit" bash "scripts/ci/$gate_script" 9>&- || result=$?
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
