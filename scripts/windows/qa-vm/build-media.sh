#!/usr/bin/env bash
# Build the two install CDs for the Okilum Windows QA VM (#1113), outside the repo.
#   [COMPUTER_NAME=OKILUM-WIN-GPU] build-media.sh OUT_DIR
# COMPUTER_NAME is the Windows hostname: OKILUM-WIN-QA (VM 150, default) or OKILUM-WIN-GPU (VM 151, #1264).
# OUT_DIR/okilum-win-qa-answer.iso     Autounattend.xml with qa's password (Infisical
#                                      infrastructure/prod/okilum-win-qa/QA_USER_PASSWORD), mode 0600
# OUT_DIR/okilum-win-qa-provision.iso  first-logon provisioning, no secrets
# ISOs are built with xorriso in a throwaway Alpine container (maestro has no ISO tool).
set -Eeuo pipefail
out="${1:?output directory required}"
computer_name="${COMPUTER_NAME:-OKILUM-WIN-QA}"
[[ "$computer_name" =~ ^[A-Z0-9-]{1,15}$ ]] || { echo "build-media.sh: bad COMPUTER_NAME" >&2; exit 1; }
here="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
umask 077
mkdir -p "$work/answer" "$work/provision"
password="$(secret get infrastructure /okilum-win-qa QA_USER_PASSWORD)"
[ -n "$password" ] || { echo "build-media.sh: no QA password in Infisical" >&2; exit 1; }
PASSWORD="$password" COMPUTER_NAME="$computer_name" python3 - "$here/Autounattend.xml.in" "$work/answer/Autounattend.xml" <<'PY'
import os, sys
from xml.sax.saxutils import escape
text = open(sys.argv[1]).read().replace('@QA_PASSWORD@', escape(os.environ['PASSWORD']))
text = text.replace('@COMPUTER_NAME@', os.environ['COMPUTER_NAME'])
open(sys.argv[2], 'w').write(text)
PY
cp "$here/OKILUM-QA.CMD" "$here/provision.ps1" "$here/run-gui.ps1" "$here/authorized_keys" "$work/provision/"
# No chmod: the work tree stays owner-only (umask 077); the container runs as root and reads it.
docker run --rm -v "$work:/w" -v "$out:/out" alpine:latest sh -c '
    apk add --no-cache xorriso >/dev/null &&
    xorriso -as mkisofs -quiet -J -r -V OKILUM_QA_ANSWER -o /out/okilum-win-qa-answer.iso /w/answer &&
    xorriso -as mkisofs -quiet -J -r -V OKILUM_QA_PROV -o /out/okilum-win-qa-provision.iso /w/provision &&
    chown '"$(id -u):$(id -g)"' /out/okilum-win-qa-*.iso'
chmod 600 "$out/okilum-win-qa-answer.iso"
chmod 644 "$out/okilum-win-qa-provision.iso"
sha256sum "$out"/okilum-win-qa-*.iso | sed "s|$out/||"
