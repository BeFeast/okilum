#!/usr/bin/env bash
# Called only while fast-local owns this slot's lock. No compiler/host changes.
set -euo pipefail
input=$(pwd -P)
destination=${1:?slot source directory required}
expected=${GITHUB_SHA:-$(git rev-parse HEAD)}
[[ $expected =~ ^[0-9a-f]{40}$ && $destination = /* && ! -L $destination ]]
[[ $(git rev-parse HEAD) == "$expected" ]]
git diff --quiet HEAD --
if [[ ! -e $destination ]]; then
    git init --quiet "$destination"
fi
[[ ! -L $destination/.git && -d $destination/.git ]]
[[ $(git -C "$destination" rev-parse --show-toplevel) == "$destination" ]]
# Copy Git objects, not an alternates link to the ephemeral runner checkout.
# Checkout preserves unchanged tracked mtimes. Generated vendor clones remain
# at their verified patched revision instead of being freshly cloned each run.
git -C "$destination" -c core.hooksPath=/dev/null fetch --quiet --no-tags "$input" "$expected"
if git -C "$destination" rev-parse --verify HEAD >/dev/null 2>&1; then
    # Refuse tracked residue rather than rewriting it and hiding contamination.
    git -C "$destination" diff --quiet HEAD --
fi
git -C "$destination" clean -ffdx -e vendor/ >/dev/null
git -C "$destination" -c core.hooksPath=/dev/null checkout --quiet --detach "$expected"
[[ $(git -C "$destination" rev-parse HEAD) == "$expected" ]]
git -C "$destination" diff --quiet HEAD --
echo "FAST_LOCAL_SOURCE=$destination sha=$expected"
