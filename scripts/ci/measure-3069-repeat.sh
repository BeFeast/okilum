#!/usr/bin/env bash
# Throwaway (#3069): run the admission runner N times with 3 CPU hogs.
set -uo pipefail
source scripts/ci/test-nofile.sh
variant=$1; n=$2
runner=scripts/ci/sync-managed-admission-runs.py
[[ $variant == base ]] && runner=scripts/ci/sync-managed-admission-runs-base.py
hogs=(); for _ in 1 2 3; do nice -n 10 yes > /dev/null & hogs+=($!); done
fail=0
for i in $(seq "$n"); do
    log=$(mktemp)
    if python3 "$runner" "$MANAGED_LAUNCH" > "$log" 2>&1; then v=pass; else v=FAIL; fail=$((fail+1)); fi
    boot=$(grep -c 'issue2582_linux_bootstrap_and_replaced_bindings_fail_closed ... Error: existing Sync configuration requires explicit connection selection' "$log")
    echo "RUN $variant $i $v bootstrap_refusals=$boot $(grep -m1 -o 'MANAGED_ADMISSION_PASS.*' "$log")"
    [[ $v == FAIL ]] && tail -5 "$log" | cut -c1-400
    python3 scripts/ci/sync-fixture-processes.py --root "$TMPDIR" --cleanup > /dev/null
done
kill "${hogs[@]}"
echo "SUMMARY $variant failed=$fail/$n"
