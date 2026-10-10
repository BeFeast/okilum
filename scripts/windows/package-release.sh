#!/usr/bin/env bash
# Sign and package a Windows release on the signer runner (#1104).
#
#   package-release.sh TOOLS PAYLOAD OUTPUT
#
# PAYLOAD is the unsigned cross-build (scripts/build-windows-ci.sh output). With a working
# signing key: sign okilum.exe and the sync supervisor, rebuild the portable ZIP from the
# signed okilum.exe, `vpk pack` with the sign template (Update.exe, the execution stub,
# Setup.exe), then verify every shipped program. OUTPUT/signing.json records the result
# for publication; stable promotion can require it.
#
# Without a working key, OKILUM_WINDOWS_UNSIGNED_POLICY decides:
#   publish-unsigned-beta  package unsigned, signing.json says so (stable then refuses it)
#   fail (default)         stop; nothing is packaged
set -Eeuo pipefail
tools="${1:?vpk tools directory required}"
payload="${2:?payload directory required}"
output="${3:?release directory required}"
version="${OKILUM_RELEASE_VERSION:?release version required}"
here="$(cd "$(dirname "$0")" && pwd)"
policy="${OKILUM_WINDOWS_UNSIGNED_POLICY:-fail}"
mkdir -p "$output"

record() {  # signed reason
    python3 - "$output/signing.json" "$1" "$2" <<'PY'
import json, os, sys
path, signed, reason = sys.argv[1], sys.argv[2] == 'true', sys.argv[3]
json.dump({'signed': signed, 'reason': reason,
           'backend': os.environ.get('OKILUM_WINDOWS_SIGN_BACKEND', ''),
           'certificate_sha256': os.environ.get('OKILUM_WINDOWS_SIGN_CERT_SHA256', '').replace(':', '').lower() if signed else ''},
          open(path, 'w'))
PY
}

signed_release() {
    bash "$here/sign.sh" "$payload/okilum.exe" "$payload/okilum-sync-supervisor.exe" &&
    python3 "$here/portable-zip.py" "$payload" "$version" &&
    OKILUM_WINDOWS_SIGN_TEMPLATE="$here/sign.sh {{file...}}" \
    OKILUM_WINDOWS_SIGN_EXCLUDE='[\\/](okilum|okilum-sync-supervisor)\.exe$' \
        bash "$here/pack.sh" "$tools" "$payload" "$output" &&
    python3 "$here/verify-signatures.py" "$output" "$payload"/okilum-*-x86_64.zip
}

if probe=$(bash "$here/sign.sh" --probe 2>&1); then
    echo "$probe"
    # Keep the unsigned payload: if signing fails halfway (session ends, timestamp server
    # down), the policy can still ship this build unsigned rather than not at all.
    pristine=$(mktemp -d)
    cp -a "$payload" "$pristine/payload"
    cp -a "$output" "$pristine/output"   # holds the delta base from publish.py prepare
    if signed_release; then
        rm -rf "$pristine"
        record true "signed and verified"
        exit 0
    fi
    probe="signing failed after a successful probe"
    rm -rf "$payload" "$output"
    mv "$pristine/payload" "$payload"
    mv "$pristine/output" "$output"
    rm -rf "$pristine"
fi

echo "$probe" >&2
case "$policy" in
    publish-unsigned-beta)
        echo "package-release.sh: no signing key; OKILUM_WINDOWS_UNSIGNED_POLICY=publish-unsigned-beta packages unsigned" >&2
        env -u OKILUM_WINDOWS_SIGN_TEMPLATE -u OKILUM_WINDOWS_SIGN_EXCLUDE bash "$here/pack.sh" "$tools" "$payload" "$output"
        record false "no signing key: ${probe##*: }"
        ;;
    *)
        echo "package-release.sh: no signing key and OKILUM_WINDOWS_UNSIGNED_POLICY=$policy: nothing is packaged" >&2
        exit 1
        ;;
esac
