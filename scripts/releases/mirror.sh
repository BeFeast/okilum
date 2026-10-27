#!/bin/bash
# Never mirror internal branches, PR refs or pre-publication tags.
set -Eeuo pipefail
if [ "${OKILUM_GITHUB_AUTH_RESOLVED:-}" != 1 ]; then
    exec python3 "$(dirname "$0")/../ci/github_auth.py" exec -- bash "$0"
fi
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
auth_dir=$(mktemp -d)
trap 'rm -rf "$auth_dir"' EXIT
cat > "$auth_dir/askpass" <<'ASKPASS'
#!/bin/sh
case "$1" in
    *Username*) printf '%s\n' x-access-token ;;
    *) printf '%s\n' "$MIRROR_TOKEN" ;;
esac
ASKPASS
chmod 700 "$auth_dir/askpass"
GIT_ASKPASS="$auth_dir/askpass" GIT_TERMINAL_PROMPT=0 \
    git -c credential.helper= push https://github.com/BeFeast/okilum.git "${args[@]}"
