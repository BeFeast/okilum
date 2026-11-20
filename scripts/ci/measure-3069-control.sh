#!/usr/bin/env bash
# Throwaway (#3069): the bootstrap control's verdict depends only on whether a
# same-uid process named syncthing exists while it runs.
set -uo pipefail
source scripts/ci/test-nofile.sh
test_once() {  # $1 = label; prints the verdict line
    local w; w=$(mktemp -d); mkdir -m 700 "$w/state"; mkdir "$w/tmp"
    TMPDIR=$w/tmp XDG_STATE_HOME=$w/state "$MANAGED_LAUNCH" --include-ignored --nocapture --test-threads=1 \
        issue2582_linux_bootstrap_and_replaced_bindings_fail_closed --exact > "$w/log" 2>&1
    local rc=$?
    echo "CONTROL $1 rc=$rc $(grep -m1 'test result:' "$w/log")"
    grep -m1 'explicit connection selection' "$w/log" || true
    python3 scripts/ci/sync-fixture-processes.py --root "$w" --cleanup >/dev/null
    return $rc
}
f=$(mktemp -d); cp /usr/bin/sleep "$f/syncthing"; mkdir "$f/home"
STHOMEDIR=$f/home "$f/syncthing" 900 & foreign=$!
test_once foreign; foreign_rc=$?
kill "$foreign"; wait "$foreign" 2>/dev/null
test_once alone; alone_rc=$?
[[ $foreign_rc != 0 && $alone_rc == 0 ]] && echo CONTROL_PASS || { echo CONTROL_FAIL; exit 1; }
