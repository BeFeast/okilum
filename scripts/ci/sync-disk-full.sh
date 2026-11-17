#!/usr/bin/env bash
# #3080: real ENOSPC during a Sync transfer and during the first merge (#2043).
# Each test fills its own private, small tmpfs; nothing else on the host is
# filled. Only a disposable GitHub-hosted runner may run this: it mounts with
# the runner's passwordless sudo.
set -euo pipefail
if [[ ${GITHUB_ACTIONS:-} != true || ${RUNNER_ENVIRONMENT:-} != github-hosted ]]; then
    echo 'sync-disk-full.sh mounts private volumes and runs only on a disposable GitHub-hosted runner; refusing here.' >&2
    exit 64
fi
source "$(dirname "${BASH_SOURCE[0]}")/test-nofile.sh"
export TMPDIR="${TMPDIR:-${RUNNER_TEMP:?}}"
OKILUM_SYNC_DIRECT_CLIENT=$(python3 scripts/ci/sync-fixture-stage.py)
export OKILUM_SYNC_DIRECT_CLIENT
volumes=$(mktemp -d)
evidence=$(mktemp)
cleaned=
cleanup() {
    local status=$? failed=0 volume
    [[ -n $cleaned ]] && return
    cleaned=1
    # Finish cleanup even while test-group.py is terminating this group.
    trap '' INT TERM
    set +e
    # Stop this job's fixture daemons first, then anything still holding
    # one of these private mounts (scoped to the mount, never host-wide).
    python3 scripts/ci/sync-fixture-processes.py --root "$TMPDIR" --cleanup || failed=1
    for volume in merge transfer; do
        if mountpoint -q "$volumes/$volume"; then
            sudo -n fuser -k -M -m "$volumes/$volume" >/dev/null 2>&1
            if ! sudo -n umount "$volumes/$volume"; then
                echo "disk-full cleanup: umount $volumes/$volume failed; detaching it" >&2
                sudo -n umount -l "$volumes/$volume" || { echo "disk-full cleanup: could not unmount $volumes/$volume" >&2; failed=1; }
            fi
        fi
    done
    rm -rf "$volumes" || failed=1
    rm -f "$evidence"
    if (( failed )) && (( status == 0 )); then
        status=1
    fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 143' TERM
trap 'exit 130' INT
for volume in merge transfer; do
    mkdir "$volumes/$volume"
    sudo -n mount -t tmpfs -o "size=16m,mode=0700,uid=$(id -u),gid=$(id -g)" okilum-disk-full "$volumes/$volume"
    findmnt -n -o SOURCE,FSTYPE,SIZE --mountpoint "$volumes/$volume"
done
export OKILUM_SYNC_DISK_FULL_MERGE="$volumes/merge"
export OKILUM_SYNC_DISK_FULL_TRANSFER="$volumes/transfer"
cargo test --locked -p okilum-sync-controller --test managed_launch -- \
    --exact --ignored --nocapture --test-threads=1 \
    issue3080_first_merge_disk_full_keeps_every_version_and_recovers \
    issue3080_transfer_disk_full_needs_attention_keeps_files_and_resumes | tee "$evidence"
grep -q 'test result: ok\. 2 passed; 0 failed;' "$evidence"
grep -q '^ISSUE3080_FIRST_MERGE_DISK_FULL_PASS' "$evidence"
grep -q '^ISSUE3080_TRANSFER_DISK_FULL_PASS' "$evidence"
echo 'SYNC_DISK_FULL_PASS: real ENOSPC during transfer and first merge; no data loss'
