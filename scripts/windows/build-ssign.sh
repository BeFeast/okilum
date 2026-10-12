#!/usr/bin/env bash
# Build the pinned ssign for the Windows signer (#1104) and install it on CT 142.
# Run on CT 141 (tessera-dev), where builds belong:  bash build-ssign.sh
# ssign (github.com/Le-Syl21/ssign, MIT) signs PE files over Certum's SimplySign HTTPS API
# with the same OAuth login as SimplySign Desktop; reviewed at this commit (v0.1.7, see #1104).
# The installed binary must match ssign.sha256, which sign.sh checks before every use.
# Signer setup once (root on CT 142):
#   install -m 755 ssign /usr/local/bin/ssign
#   echo 'd /run/okilum-ssign 0700 okilum-signer okilum-signer -' > /etc/tmpfiles.d/okilum-ssign.conf
#   systemd-tmpfiles --create /etc/tmpfiles.d/okilum-ssign.conf
#   install -m 644 hello.exe /usr/local/share/okilum-signer/test.exe   # ssign's own test PE
set -Eeuo pipefail
COMMIT=5fd4daf22155b19b645aef3dd467f3c4c9e440b3
dir="${1:-$HOME/work/ssign}"
[ -d "$dir/.git" ] || git clone -q https://github.com/Le-Syl21/ssign "$dir"
cd "$dir"
git fetch -q --tags
git checkout -q "$COMMIT"
[ "$(git rev-parse HEAD)" = "$COMMIT" ] || { echo "not at the pinned commit" >&2; exit 1; }
CC="${CC:-/usr/bin/cc}" cargo build --release --locked --bin ssign
sha256sum target/release/ssign
echo "compare with scripts/windows/ssign.sha256; update it only with a reviewed commit"
