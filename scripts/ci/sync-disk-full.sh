#!/usr/bin/env bash
# #3080: real ENOSPC during a Sync transfer and during the first merge (#2043).
# Each test fills its own private, small tmpfs; nothing else on the host is
# filled. GitHub-hosted runners provide the passwordless sudo the mounts need.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/test-nofile.sh"
export TMPDIR="${TMPDIR:-${RUNNER_TEMP:?}}"
OKILUM_SYNC_DIRECT_CLIENT=$(python3 scripts/ci/sync-fixture-stage.py)
export OKILUM_SYNC_DIRECT_CLIENT
volumes=$(mktemp -d)
evidence=$(mktemp)
cleanup() {
    for volume in merge transfer; do
        if mountpoint -q "$volumes/$volume"; then
            sudo -n umount "$volumes/$volume"
        fi
    done
    rm -rf "$volumes"
    unlink "$evidence"
}
trap cleanup EXIT
for volume in merge transfer; do
    mkdir "$volumes/$volume"
    sudo -n mount -t tmpfs -o "size=16m,mode=0700,uid=$(id -u),gid=$(id -g)" okilum-disk-full "$volumes/$volume"
    findmnt -n -o FSTYPE,SIZE --target "$volumes/$volume"
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
