#!/bin/bash
# Sourced by build-macos-ci.sh after sign-bundle.sh (#993). Packs the stapled $APP into
# a first-install DMG (app + Applications link, branded window), signs it with the
# Developer ID, notarizes and staples it, and checks it the way Gatekeeper sees a
# download. Sets $DMG. The zip in $ARCHIVE stays the Sparkle update payload.
DMG="$OUTPUT/Okilum-$DISPLAY_VERSION.dmg"
# The M4 runner's python3 is the system 3.9: these are the last releases that support it.
python3 -m venv "$OUTPUT/dmgbuild-env"
"$OUTPUT/dmgbuild-env/bin/pip" install --quiet --disable-pip-version-check \
    dmgbuild==1.6.5 ds_store==1.3.1 mac_alias==2.2.2
rm -f "$DMG"
"$OUTPUT/dmgbuild-env/bin/dmgbuild" -s scripts/macos-dmg/settings.py \
    -D app="$APP" -D icon="$APP/Contents/Resources/Okilum.icns" \
    -D background="$PWD/scripts/macos-dmg/background.png" \
    Okilum "$DMG"
codesign --force --timestamp --sign "$OKILUM_SIGNING_IDENTITY" "$DMG"
codesign --verify --strict --verbose=2 "$DMG"
xcrun notarytool submit "$DMG" --keychain-profile "$OKILUM_NOTARY_PROFILE" --wait --output-format json | tee "$OUTPUT/notarization-dmg.json"
python3 -c 'import json,sys; s=json.load(open(sys.argv[1]))["status"]; sys.exit(s!="Accepted" and f"DMG notarization: {s}")' "$OUTPUT/notarization-dmg.json"
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"
# A quarantined copy must open without a Gatekeeper prompt, and its app must be the stapled one.
cp "$DMG" "$OUTPUT/fresh-download.dmg"
xattr -w com.apple.quarantine "0083;$(printf '%x' "$(date +%s)");OkilumRelease;$(uuidgen)" "$OUTPUT/fresh-download.dmg"
spctl --assess --type open --context context:primary-signature --verbose=2 "$OUTPUT/fresh-download.dmg"
MOUNT="$(mktemp -d "$OUTPUT/dmg-mount.XXXXXX")"
hdiutil attach -nobrowse -readonly -mountpoint "$MOUNT" "$OUTPUT/fresh-download.dmg" >/dev/null
trap 'hdiutil detach "$MOUNT" -quiet || true' EXIT
test "$(readlink "$MOUNT/Applications")" = /Applications
test -f "$MOUNT/.background.tiff"
codesign --verify --deep --strict "$MOUNT/Okilum.app"
xcrun stapler validate "$MOUNT/Okilum.app"
# Same app: the code directory hash covers the executable and every sealed resource.
# (diff -r cannot walk the framework's Versions/Current symlinks and silently skips them.)
cdhash() { codesign -d --verbose=4 "$1" 2>&1 | sed -n 's/^CDHash=//p'; }
test -n "$(cdhash "$APP")"
test "$(cdhash "$APP")" = "$(cdhash "$MOUNT/Okilum.app")"
echo "DMG app CDHash $(cdhash "$MOUNT/Okilum.app") matches the stapled app"
hdiutil detach "$MOUNT" -quiet
trap - EXIT
rm -f "$OUTPUT/fresh-download.dmg"
