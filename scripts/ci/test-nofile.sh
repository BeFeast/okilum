#!/usr/bin/env bash
# Source in the test shell; raise only its soft limit, never the host hard limit.
okilum_test_nofile() {
    local soft hard
    soft=$(ulimit -Sn) || return
    hard=$(ulimit -Hn) || return
    printf 'nofile before: soft=%s hard=%s\n' "$soft" "$hard"
    if [[ $soft != "$hard" ]]; then
        ulimit -Sn "$hard" || { echo 'Cannot raise test process soft nofile limit' >&2; return 1; }
    fi
    printf 'nofile for tests: soft=%s hard=%s\n' "$(ulimit -Sn)" "$(ulimit -Hn)"
}
okilum_test_nofile
