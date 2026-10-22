#!/usr/bin/env bash
# Only the disposable Ubuntu 24.04 runtime: two verified official HTTPS routes.
set -euo pipefail
if [[ ${1:-} != --bounded ]]; then
    exec timeout --signal=TERM --kill-after=15s 12m bash "$0" --bounded "$@"
fi
shift
[[ $(. /etc/os-release; echo "$ID:$VERSION_ID") == ubuntu:24.04 ]]
[[ $(dpkg --print-architecture) == amd64 ]]
: "${OKILUM_APT_CA_FILE:=/ci-ca-certificates.crt}"
[[ -s $OKILUM_APT_CA_FILE ]]
helper="$(dirname "$0")/apt-bounded.sh"
sources=$(mktemp)
trap 'rm -f "$sources"' EXIT
options=(-o "Dir::Etc::sourcelist=$sources" -o Dir::Etc::sourceparts=-
    -o "Acquire::https::CaInfo=$OKILUM_APT_CA_FILE")
ready=false
for mirror in archive.ubuntu.com security.ubuntu.com; do
    echo "Ubuntu smoke provisioning: HTTPS mirror $mirror (bounded update + download)"
    cat > "$sources" <<SOURCES
deb [arch=amd64 signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://$mirror/ubuntu noble main universe
deb [arch=amd64 signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://$mirror/ubuntu noble-updates main universe
deb [arch=amd64 signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://$mirror/ubuntu noble-security main universe
SOURCES
    # apt checks Release signatures and package hashes. Completed downloads stay
    # in this container's archive cache for the single explicit fallback attempt.
    if OKILUM_APT_TIMEOUT=60s bash "$helper" "${options[@]}" update &&
       OKILUM_APT_TIMEOUT=150s bash "$helper" "${options[@]}" install --download-only -y --no-install-recommends "$@"; then
        ready=true
        break
    else
        status=$?
        echo "Ubuntu smoke provisioning: $mirror failed (status $status)" >&2
    fi
done
if [[ $ready != true ]]; then
    echo 'Ubuntu smoke provisioning: both official HTTPS routes failed' >&2
    exit 1
fi
# No runtime state is restored: install verified debs into this fresh container.
bash "$helper" "${options[@]}" install --no-download -y --no-install-recommends "$@"
