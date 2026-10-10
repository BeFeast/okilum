#!/bin/bash
# Package an already cross-compiled Reader. No signing by default.
set -Eeuo pipefail
tools="${1:?vpk tools directory required}"
payload="${2:?application payload required}"
output="${3:?release directory required}"
version="${OKILUM_RELEASE_VERSION:?release version required}"
# The sync supervisor ships in the app folder (#1013); Enable stages a copy outside
# `current`. It must be present and a GUI-subsystem x64 program, or a login task would
# flash a console window.
helper="$payload/okilum-sync-supervisor.exe"
[ -f "$helper" ] || { echo "pack.sh: the sync supervisor is missing from the payload" >&2; exit 1; }
python3 "$(dirname "$0")/pe.py" gui "$helper"
cp docs/windows-delivery.md "$payload/README.md"
channel=$(python3 -c 'import json; print(json.load(open("scripts/windows/channel.json"))["default_channel"])')
args=()
# Signing (#1104): Velopack calls the template for every PE it packs, then for Setup.exe.
# On Linux it re-signs files that are already signed, so files signed earlier (okilum.exe
# and the sync supervisor, signed before the portable ZIP is built) are excluded by regex.
if [ -n "${OKILUM_WINDOWS_SIGN_TEMPLATE:-}" ]; then
    args+=(--signTemplate "$OKILUM_WINDOWS_SIGN_TEMPLATE")
    if [ -n "${OKILUM_WINDOWS_SIGN_EXCLUDE:-}" ]; then
        args+=(--signExclude "$OKILUM_WINDOWS_SIGN_EXCLUDE")
    fi
fi
"$tools/dotnet/dotnet" "$tools/vpk/tools/net8.0/any/vpk.dll" '[win]' pack \
    --packId BeFeast.Okilum --packTitle Okilum --packAuthors BeFeast \
    --packVersion "$version" --packDir "$payload" --mainExe okilum.exe \
    --runtime win-x64 --channel "$channel" --icon "$payload/okilum.ico" \
    --exclude '.*\.(pdb|zip|sha256)$|metadata\.json' \
    --outputDir "$output" --skip-updates --yes "${args[@]}"
# What Velopack actually packed, not what we meant to give it.
python3 - "$output" "$version" "$channel" <<'CHECK'
import glob, sys, zipfile
output, version, channel = sys.argv[1:4]
found = glob.glob(f"{output}/BeFeast.Okilum-{version}-{channel}-full.nupkg")
if len(found) != 1:
    sys.exit(f"pack.sh: expected one full package for {version}, found {found}")
if "lib/app/okilum-sync-supervisor.exe" not in zipfile.ZipFile(found[0]).namelist():
    sys.exit("pack.sh: okilum-sync-supervisor.exe is not in the package")
CHECK
