#!/bin/bash
# Never mirror internal branches, PR refs or pre-publication tags.
set -Eeuo pipefail
: "${MIRROR_TOKEN:?GitHub mirror token required}"
git fetch --no-tags origin main
args=('refs/remotes/origin/main:refs/heads/main')
if [ -n "${RELEASE_TAG:-}" ]; then
    [[ "$RELEASE_TAG" == beta || "$RELEASE_TAG" =~ ^v0\.1\.[1-9][0-9]*$ ]] || exit 1
    git fetch origin "refs/tags/$RELEASE_TAG:refs/tags/$RELEASE_TAG" --force
    git merge-base --is-ancestor "refs/tags/$RELEASE_TAG" origin/main
    if [ "$RELEASE_TAG" = beta ]; then
        args+=("+refs/tags/beta:refs/tags/beta")
    else
        args+=("refs/tags/$RELEASE_TAG:refs/tags/$RELEASE_TAG")
    fi
fi
git push "https://x-access-token:${MIRROR_TOKEN}@github.com/BeFeast/tessera.git" "${args[@]}"
