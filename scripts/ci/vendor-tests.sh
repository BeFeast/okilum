#!/usr/bin/env bash
# Unit tests of the vendored gpui-kit crates our patch stack changes (gpui-base,
# gpui-component), in the vendor's own workspace: our workspace only depends on them
# by path, so its `cargo test` never builds their tests. A patch's broken test
# compiled nowhere until someone built it on CT141.
#
# Runs when the change touches the patch stack or the pinned vendor revision; skips
# otherwise (it compiles gpui a second time). When the change cannot be determined it
# runs: never skip silently. OKILUM_VENDOR_TESTS=always|never overrides.
set -euo pipefail
cd "$(dirname "$0")/../.."
mode="${OKILUM_VENDOR_TESTS:-auto}"
[ "$mode" = never ] && { echo "vendor tests: skipped (OKILUM_VENDOR_TESTS=never)"; exit 0; }

changed() {
    # The change under test: a PR head against main, or a main commit against its parent.
    # Main into its own ref: a later fetch (the deepen below) overwrites FETCH_HEAD with
    # HEAD, and a merge base equal to HEAD would compare only the last commit (#1238).
    git fetch --quiet --no-tags --depth=300 origin +refs/heads/main:refs/okilum-ci/main 2>/dev/null || return 1
    git rev-parse --is-shallow-repository | grep -q true && git fetch --quiet --no-tags --deepen=300 2>/dev/null
    local base
    base=$(git merge-base HEAD refs/okilum-ci/main 2>/dev/null) || return 1
    [ "$base" = "$(git rev-parse HEAD)" ] && base=$(git rev-parse HEAD^1 2>/dev/null || true)
    [ -n "$base" ] || return 1
    git diff --name-only "$base" HEAD
}

if [ "$mode" = auto ]; then
    if files=$(changed); then
        if ! grep -qE '^(scripts/patches/|scripts/vendor-setup\.sh$)' <<<"$files"; then
            echo "vendor tests: skipped (no patch stack or vendor revision change)"
            exit 0
        fi
        echo "vendor tests: the patch stack or vendor revision changed"
    else
        echo "vendor tests: change set unknown, running them"
    fi
fi

scripts/vendor-setup.sh --verify
# Patches may change vendor manifests, so the vendor lockfile is not --locked here.
timeout "${OKILUM_VENDOR_TESTS_TIMEOUT:-40m}" \
    cargo test --manifest-path vendor/gpui-component/Cargo.toml -p gpui-base -p gpui-component --lib
