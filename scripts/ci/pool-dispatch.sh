#!/usr/bin/env bash
# Run one Okilum Linux gate on the build-slot pool and publish its phase receipt
# for the following GitHub steps. Exit 0 when the suite produced a code verdict
# (pass or fail). Exit 1 when the pool produced no verdict (queue, timeout, ssh).
set -euo pipefail
mode=${1:?linux-full or pre-merge}
case $mode in
    linux-full) script=scripts/ci/pool-linux-gate.sh ;;
    pre-merge) script=scripts/ci/pool-pre-merge.sh ;;
    *) echo "unknown pool mode: $mode" >&2; exit 64 ;;
esac
sha=${OKILUM_SOURCE_COMMIT:-$(git rev-parse HEAD)}
[[ $sha =~ ^[0-9a-f]{40}$ ]] || { echo "source is not a full SHA: $sha" >&2; exit 75; }
export PATH="${HOME}/.local/bin:${HOME}/.cargo/bin:${PATH}"
command -v build-slot >/dev/null || { echo "build-slot is not on PATH" >&2; exit 75; }

ship=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/okilum-pool.XXXXXX")
receipt="$ship/phases.json"
label="ci-${mode}-$(printf '%s' "$sha" | cut -c1-12)"
args=(
    run --project okilum --label "$label" --owner ci-pool
    --timeout-minutes 40 --mem 7
    --ship "$ship"
    --env "OKILUM_POOL_RECEIPT=$receipt"
)
# The full gate builds the per-user AppImage runtime with Docker. Only baldr
# has Docker; pre-merge does not, and can land on any pool host.
if [[ $mode == linux-full ]]; then
    args+=(--require-host baldr)
fi
if [[ ${GITHUB_REF:-} == refs/heads/agent-checks-benchmark/* ]]; then
    args+=(--env OKILUM_POOL_BENCHMARK=1)
fi
if [[ -n ${PUBLICATION_BASELINE:-} ]]; then
    args+=(--env "OKILUM_PUBLICATION_BASELINE=${PUBLICATION_BASELINE}")
fi
if build-slot run --help 2>&1 | grep -q -- '--source-git'; then
    args+=(--source-git "$PWD" --source-rev "$sha")
else
    archive=$(mktemp "${TMPDIR:-/tmp}/okilum-src.XXXXXX.tar")
    git archive --format=tar -o "$archive" "$sha"
    args+=(--source-tar "$archive")
fi
set +e
build-slot "${args[@]}" -- bash "$script"
rc=$?
set -e
rm -f "${archive:-}"

kind=infra
if [[ -f $receipt && ( $rc -eq 0 || $rc -eq 1 ) ]]; then
    kind=verdict
fi
if [[ -n ${GITHUB_OUTPUT:-} ]]; then
    printf 'kind=%s\n' "$kind" >> "$GITHUB_OUTPUT"
fi
if [[ -n ${GITHUB_ENV:-} ]]; then
    printf 'OKILUM_POOL_RECEIPT=%s\n' "$receipt" >> "$GITHUB_ENV"
fi
printf 'pool dispatch mode=%s rc=%s kind=%s receipt=%s\n' "$mode" "$rc" "$kind" "$receipt"
if [[ $kind == verdict ]]; then
    exit 0
fi
exit 1
