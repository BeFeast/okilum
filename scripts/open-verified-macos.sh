#!/bin/bash
# Operator-run, pinned download and launch. Does not install or bypass Gatekeeper.
set -Eeuo pipefail
URL=${1:?Pinned signed ZIP URL required}
SHA256=${2:?Pinned signed ZIP SHA256 required}
SOURCE=${3:?Pinned application source SHA required}
: "${TESSERA_SIGNING_TEAM_ID:?Expected signing Team ID required}"
: "${TESSERA_SIGNING_IDENTITY:?Expected signing identity required}"
[[ $URL == https://* && $SHA256 =~ ^[0-9a-f]{64}$ && $SOURCE =~ ^[0-9a-f]{40}$ ]]
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]]
ROOT="$HOME/Downloads/Tessera-verified-${SOURCE:0:12}-$(date +%Y%m%dT%H%M%S)"
mkdir "$ROOT"
curl --fail --location --proto '=https' "$URL" -o "$ROOT/Tessera.zip"
printf '%s  %s\n' "$SHA256" "$ROOT/Tessera.zip" | shasum -a 256 -c -
ditto -x -k "$ROOT/Tessera.zip" "$ROOT/extracted"
APP="$ROOT/extracted/Tessera.app"
[[ $(/usr/libexec/PlistBuddy -c 'Print :TesseraSourceCommit' "$APP/Contents/Info.plist") == "$SOURCE" ]]
# curl is not a browser; explicitly retain normal downloaded-app assessment semantics.
xattr -w com.apple.quarantine "0083;$(printf '%x' "$(date +%s)");TesseraVerifiedDownload;$(uuidgen)" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
codesign -dv --verbose=4 "$APP" 2> "$ROOT/signature.txt"
grep -Fx "TeamIdentifier=$TESSERA_SIGNING_TEAM_ID" "$ROOT/signature.txt"
grep -Fx "Authority=$TESSERA_SIGNING_IDENTITY" "$ROOT/signature.txt"
xcrun stapler validate "$APP"
spctl --assess --type execute --verbose=2 "$APP"
if command -v syspolicy_check >/dev/null 2>&1; then syspolicy_check distribution "$APP"; fi
# User execution of this script authorizes this launch; CI never invokes it.
open "$APP"
