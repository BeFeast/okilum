#!/usr/bin/env bash
# vendor-setup.sh — make vendor/gpui-component ready to build against.
#
# Clones the pinned upstream revision if it is not there, then applies our patch
# set idempotently. Safe to re-run; run it after every fresh clone.
#
# The vendored tree is NOT committed. What is committed is this script, the
# pinned revision, and the diffs in scripts/patches/ — so the build is
# reproducible without carrying 20 MB of someone else's repository.
#
# A fresh clone arrives UNPATCHED. That has already cost a full session of
# misleading evidence: a reader was built, measured and screenshotted against a
# tree everyone assumed was patched and was not. This script prints what it did,
# and --verify exits non-zero if the tree is not in the expected state.
set -euo pipefail

REPO_URL="https://github.com/longbridge/gpui-kit"
# Pinned. Do not float this: every measurement in docs/ was taken against it.
# The directory keeps its pre-rename name; upstream is longbridge/gpui-kit.
REPO_REV="928c3eb776a3d733d9b771f7dea27a6a79242ced"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR="$ROOT/vendor/gpui-component"
PATCHES="$ROOT/scripts/patches"

verify_only=0
[ "${1:-}" = "--verify" ] && verify_only=1

if [ ! -d "$VENDOR/.git" ]; then
    if [ "$verify_only" = 1 ]; then
        echo "vendor-setup --verify: $VENDOR is missing; run scripts/vendor-setup.sh" >&2
        exit 1
    fi
    echo "cloning $REPO_URL at $REPO_REV"
    mkdir -p "$(dirname "$VENDOR")"
    git clone --quiet "$REPO_URL" "$VENDOR"
fi

have_rev="$(git -C "$VENDOR" rev-parse HEAD)"
if [ "$have_rev" != "$REPO_REV" ]; then
    if [ "$verify_only" = 1 ]; then
        echo "vendor-setup --verify: vendor is at $have_rev, expected $REPO_REV" >&2
        exit 1
    fi
    echo "checking out $REPO_REV (was $have_rev)"
    git -C "$VENDOR" fetch --quiet origin "$REPO_REV" 2>/dev/null || git -C "$VENDOR" fetch --quiet origin
    git -C "$VENDOR" checkout --quiet "$REPO_REV"
fi

# Verify the cumulative patch result in a disposable index. Later patches may
# intentionally refine lines introduced by earlier ones, so independently
# reverse-checking each diff is not sufficient. The working tree is never changed.
expected_index="$(mktemp)"
rm "$expected_index"
trap 'rm -f "$expected_index"' EXIT
GIT_INDEX_FILE="$expected_index" git -C "$VENDOR" read-tree HEAD
for patch in "$PATCHES"/*.diff; do
    [ -e "$patch" ] || continue
    GIT_INDEX_FILE="$expected_index" git -C "$VENDOR" apply --cached "$patch"
done
if GIT_INDEX_FILE="$expected_index" git -C "$VENDOR" diff --quiet; then
    echo "vendor ready: $VENDOR @ $REPO_REV with the complete patch stack verified"
    exit 0
fi
if [ "$verify_only" = 1 ]; then
    echo "vendor-setup --verify: working tree differs from the complete patch stack" >&2
    exit 1
fi

# Apply each patch unless it already reverse-applies cleanly, which is the only
# honest test for "already applied".
missing=0
for patch in "$PATCHES"/*.diff; do
    [ -e "$patch" ] || continue
    name="$(basename "$patch")"
    if git -C "$VENDOR" apply --reverse --check "$patch" 2>/dev/null; then
        echo "already applied: $name"
        continue
    fi
    if [ "$verify_only" = 1 ]; then
        echo "vendor-setup --verify: NOT applied: $name" >&2
        missing=1
        continue
    fi
    git -C "$VENDOR" apply "$patch"
    echo "applied: $name"
done

if [ "$verify_only" = 1 ]; then
    [ "$missing" = 0 ] || exit 1
    echo "vendor-setup --verify: vendor is at $REPO_REV with every patch applied"
    exit 0
fi

echo
echo "vendor ready: $VENDOR @ $REPO_REV"
echo "verify at any time with: scripts/vendor-setup.sh --verify"
