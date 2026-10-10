#!/bin/bash
# Sourced by build-macos-ci.sh. Signs $APP inside-out with the Developer ID,
# notarizes and staples it, then sets $ARCHIVE to the distributable ZIP in $OUTPUT.
FRAMEWORK="$APP/Contents/Frameworks/Sparkle.framework"
security find-identity -v -p codesigning | grep -F "$OKILUM_SIGNING_IDENTITY" >/dev/null
for COMPONENT in \
    "$FRAMEWORK/Versions/B/XPCServices/Downloader.xpc" \
    "$FRAMEWORK/Versions/B/XPCServices/Installer.xpc" \
    "$FRAMEWORK/Versions/B/Updater.app" \
    "$FRAMEWORK/Versions/B/Autoupdate" \
    "$FRAMEWORK"; do
    codesign --force --preserve-metadata=identifier,entitlements --options runtime --timestamp --sign "$OKILUM_SIGNING_IDENTITY" "$COMPONENT"
done
[[ $(lipo -archs "$APP/Contents/MacOS/okilum") == arm64 ]]
# The sync helper first (inside-out), with the explicit identifier its policy requires;
# the app's own policy then trusts exactly this team and identifier.
[[ $(lipo -archs "$APP/$SUPERVISOR_RELATIVE") == arm64 ]]
supervisor_sign "$APP" "$OKILUM_SIGNING_IDENTITY" "$BUNDLE_ID" --timestamp
codesign -dv --verbose=4 "$APP/$SUPERVISOR_RELATIVE" 2>&1 | grep -Fx "TeamIdentifier=$TEAM_ID" >/dev/null
codesign --force --options runtime --timestamp --sign "$OKILUM_SIGNING_IDENTITY" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$OUTPUT/notarization-app.zip"
xcrun notarytool submit "$OUTPUT/notarization-app.zip" --keychain-profile "$OKILUM_NOTARY_PROFILE" --wait --output-format json | tee "$OUTPUT/notarization.json"
python3 -c 'import json,sys; s=json.load(open(sys.argv[1]))["status"]; sys.exit(s!="Accepted" and f"Notarization: {s}")' "$OUTPUT/notarization.json"
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
# Only this archive, made after stapling, is distributable. The name is what
# builds 4791/4792 require in an update URL.
ARCHIVE="$OUTPUT/okilum-macos-arm64-${SOURCE_SHA}-notarized.zip"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ARCHIVE"
# Check the archive the way a downloaded copy is checked: quarantined, by Gatekeeper.
mkdir "$OUTPUT/fresh-download"
ditto -x -k "$ARCHIVE" "$OUTPUT/fresh-download"
xattr -w com.apple.quarantine "0083;$(printf '%x' "$(date +%s)");OkilumRelease;$(uuidgen)" "$OUTPUT/fresh-download/Okilum.app"
spctl --assess --type execute --verbose=2 "$OUTPUT/fresh-download/Okilum.app"
