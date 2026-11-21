#!/usr/bin/env bash
# Run one Okilum Linux gate on the build-slot pool and publish its phase receipt
# for the following GitHub steps. Exit 0 when the suite produced a code verdict
# (pass or fail). Exit 1 when the pool produced no verdict (queue, timeout, ssh,
# OOM kill, missing receipt); gh_check.py counts this step as infrastructure.
#
# linux-full runs six shards of pool-linux-gate.sh in parallel on different
# slots (Oleg 21.11: 4-6 shards, PR gate jobs first in build-slot, <= 20 min);
# pool-aggregate.py merges their receipts. pre-merge is one slot.
set -euo pipefail
mode=${1:?linux-full, linux-single or pre-merge}
sha=${OKILUM_SOURCE_COMMIT:-$(git rev-parse HEAD)}
[[ $sha =~ ^[0-9a-f]{40}$ ]] || { echo "source is not a full SHA: $sha" >&2; exit 75; }
export PATH="${HOME}/.local/bin:${HOME}/.cargo/bin:${PATH}"
command -v build-slot >/dev/null || { echo "build-slot is not on PATH" >&2; exit 75; }
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# shellcheck source=scripts/ci/pool-phase-lib.sh
source "$here/pool-phase-lib.sh"
ship=$(pool_receipt_dir)
receipt="$ship/phases.json"
short=$(printf '%s' "$sha" | cut -c1-12)

# shard  mem(GiB)  needs(comma)  preferred host  run minutes
# Memory: the unsharded full run peaked at 6.1 GiB (anon 4.0) on baldr, 20.11.
# The preferred hosts pair the shards two per heavy host within its memory
# budget (baldr 13, okilum-dev 12, okilum-build-nas 14 GiB); build-slot falls
# back to any capable free host after 30 s, so a busy host costs no wait.
case $mode in
    pre-merge) shards=("pre-merge 7 - - 30") ;;
    linux-single) shards=("all 7 docker,sudo,userbus,dbus - 70") ;;
    linux-full)
        shards=(
            "base 4 docker baldr 25"
            "rust-b 7 userbus,dbus baldr 25"
            "admission 3 tmpfs,sudo slava 25"
            "rust-a 7 userbus,dbus okilum-dev 25"
            "sync 4 - okilum-build-nas 25"
            "desktop 7 dbus okilum-build-nas 25"
        )
        ;;
    *) echo "unknown pool mode: $mode" >&2; exit 64 ;;
esac
# The benchmark branch measures warm rebuilds in one slot.
if [[ $mode == linux-full && ${GITHUB_REF:-} == refs/heads/agent-checks-benchmark/* ]]; then
    shards=("all 7 docker,sudo,userbus,dbus - 70")
fi

archive=
source_args=()
if build-slot run --help 2>&1 | grep -q -- '--source-git'; then
    source_args=(--source-git "$PWD" --source-rev "$sha")
else
    archive=$(mktemp "${TMPDIR:-/tmp}/okilum-src.XXXXXX.tar")
    git archive --format=tar -o "$archive" "$sha"
    source_args=(--source-tar "$archive")
fi

dispatch() {
    local name=$1 mem=$2 needs=$3 prefer=$4 minutes=$5
    local dir="$ship/$name" script label
    mkdir -p "$dir"
    if [[ $name == pre-merge ]]; then
        script=(bash scripts/ci/pool-pre-merge.sh)
        label="ci-pre-merge-$short"
    else
        script=(bash scripts/ci/pool-linux-gate.sh "$name")
        label="ci-linux-$name-$short"
    fi
    local args=(
        run --project okilum --label "$label" --owner ci-pool
        --wait-minutes 15 --timeout-minutes "$minutes" --mem "$mem"
        --receipt "$dir/build-slot.json"
        --ship "$dir"
        --env "OKILUM_POOL_RECEIPT=$dir/phases.json"
        --env "OKILUM_SOURCE_COMMIT=$sha"
    )
    if [[ $prefer != - ]]; then
        args+=(--host "$prefer")
    fi
    local need
    if [[ $needs != - ]]; then
        for need in ${needs//,/ }; do
            args+=(--needs "$need")
        done
    fi
    # PR gates go first in the queue (Oleg 21.11); the nightly main run does not.
    if [[ ${GITHUB_EVENT_NAME:-} != schedule ]]; then
        args+=(--priority critical)
    fi
    if [[ ${GITHUB_REF:-} == refs/heads/agent-checks-benchmark/* ]]; then
        args+=(--env OKILUM_POOL_BENCHMARK=1)
    fi
    if [[ -n ${PUBLICATION_BASELINE:-} ]]; then
        args+=(--env "OKILUM_PUBLICATION_BASELINE=${PUBLICATION_BASELINE}")
    fi
    set +e
    build-slot "${args[@]}" "${source_args[@]}" -- "${script[@]}" > "$dir/log" 2>&1
    local rc=$?
    set -e
    printf '%s\n' "$rc" > "$dir/rc"
}

started=$(date +%s)
pids=()
dirs=()
for row in "${shards[@]}"; do
    read -r name mem needs prefer minutes <<< "$row"
    dispatch "$name" "$mem" "$needs" "$prefer" "$minutes" &
    pids+=($!)
    dirs+=("$ship/$name")
done
for pid in "${pids[@]}"; do
    wait "$pid" || true
done
rm -f "$archive"
for dir in "${dirs[@]}"; do
    printf '::group::shard %s (rc %s)\n' "$(basename "$dir")" "$(cat "$dir/rc" 2>/dev/null || echo none)"
    cat "$dir/log" 2>/dev/null || true
    printf '::endgroup::\n'
done

summary=$(python3 "$here/pool-aggregate.py" --out "$receipt" --shards-out "$ship/shards.json" "${dirs[@]}")
printf '%s\n' "$summary"
kind=$(printf '%s\n' "$summary" | sed -n 's/^kind=//p')
if [[ -n ${GITHUB_OUTPUT:-} ]]; then
    printf 'kind=%s\n' "$kind" >> "$GITHUB_OUTPUT"
fi
if [[ -n ${GITHUB_ENV:-} ]]; then
    printf 'OKILUM_POOL_RECEIPT=%s\n' "$receipt" >> "$GITHUB_ENV"
    printf 'OKILUM_POOL_SHARDS=%s\n' "$ship/shards.json" >> "$GITHUB_ENV"
fi
printf 'pool dispatch mode=%s shards=%s kind=%s wall=%ss receipt=%s\n' \
    "$mode" "${#shards[@]}" "$kind" "$(( $(date +%s) - started ))" "$receipt"
if [[ $kind == verdict ]]; then
    exit 0
fi
exit 1
