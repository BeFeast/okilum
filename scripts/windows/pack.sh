#!/bin/bash
# Package an already cross-compiled Reader. No signing by default.
set -Eeuo pipefail
tools="${1:?vpk tools directory required}"
payload="${2:?application payload required}"
output="${3:?release directory required}"
version="${TESSERA_RELEASE_VERSION:?release version required}"
cp docs/windows-delivery.md "$payload/README.md"
channel=$(python3 -c 'import json; print(json.load(open("scripts/windows/channel.json"))["default_channel"])')
args=()
# Future signing service hook; no certificate or signing secret is required today.
if [ -n "${TESSERA_WINDOWS_SIGN_TEMPLATE:-}" ]; then
    args+=(--signTemplate "$TESSERA_WINDOWS_SIGN_TEMPLATE")
fi
"$tools/dotnet/dotnet" "$tools/vpk/tools/net8.0/any/vpk.dll" '[win]' pack \
    --packId BeFeast.Tessera --packTitle Tessera --packAuthors BeFeast \
    --packVersion "$version" --packDir "$payload" --mainExe tessera.exe \
    --runtime win-x64 --channel "$channel" --icon "$payload/tessera.ico" \
    --exclude '.*\.(pdb|zip|sha256)$|metadata\.json' \
    --outputDir "$output" --skip-updates --yes "${args[@]}"
