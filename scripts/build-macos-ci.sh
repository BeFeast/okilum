#!/bin/bash
# Build Okilum.app on the macOS runner, then sign, notarize, staple and zip it,
# and pack the same app into a signed, notarized first-install DMG (#993).
# Usage: build-macos-ci.sh OUTPUT_DIR. Runs from the repository root.
#   OKILUM_BUILD_VERSION   monotonic CFBundleVersion (CI: 5000 + run number)
#   OKILUM_SIGNING_IDENTITY, OKILUM_NOTARY_PROFILE   existing runner keychain items
#   SPARKLE_PUBLIC_ED_KEY   SUPublicEDKey value
set -Eeuo pipefail
OUTPUT="${1:?output directory required}"
: "${OKILUM_BUILD_VERSION:?}" "${OKILUM_SIGNING_IDENTITY:?}" "${OKILUM_NOTARY_PROFILE:?}" "${SPARKLE_PUBLIC_ED_KEY:?}"
[[ $OKILUM_BUILD_VERSION =~ ^[1-9][0-9]*$ ]]
DISPLAY_VERSION="0.1.$OKILUM_BUILD_VERSION"
SOURCE_SHA="$(git rev-parse HEAD)"
export OKILUM_RELEASE_VERSION="$DISPLAY_VERSION" OKILUM_SOURCE_COMMIT="$SOURCE_SHA"
SOURCE_TREE="$(git rev-parse 'HEAD^{tree}')"
test "$(uname -s)" = Darwin
test "$(uname -m)" = arm64
mkdir -p "$OUTPUT"
OUTPUT="$(cd "$OUTPUT" && pwd)"
if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
export CC=/usr/bin/clang CXX=/usr/bin/clang++
SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
export SDKROOT
rustc --version
rustup target add aarch64-apple-darwin
bash scripts/vendor-setup.sh
bash scripts/vendor-setup.sh --verify
# Build cache lives outside the checkout.
export CARGO_TARGET_DIR="$HOME/.cache/okilum-macos/reader-arm64"
export CARGO_INCREMENTAL=0
source scripts/ci/release-cache.sh
SPARKLE_ARCHIVE="$OUTPUT/Sparkle-2.10.0.tar.xz"
python3 scripts/updater/sparkle.py fetch "$SPARKLE_ARCHIVE"
python3 scripts/updater/sparkle.py prepare --archive "$SPARKLE_ARCHIVE" --destination vendor/sparkle
mkdir -p "$OUTPUT/sparkle-bin"
tar -xJf "$SPARKLE_ARCHIVE" -C "$OUTPUT/sparkle-bin" --include='*bin/sign_update'
cargo build --release --locked --target aarch64-apple-darwin -p okilum-shell
# The sync helper (#1013): its signature policy is compiled in from the build configuration.
source scripts/ci/supervisor-bundle.sh
BUNDLE_ID="${OKILUM_BUNDLE_ID:-com.befeast.okilum}"
TEAM_ID="$(supervisor_team_id "$OKILUM_SIGNING_IDENTITY")"
supervisor_build "$TEAM_ID" "$BUNDLE_ID" aarch64-apple-darwin

APP="$OUTPUT/Okilum.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
python3 scripts/third-party-notices.py --stage "$APP/Contents/Resources/Licenses"
ditto vendor/sparkle/Sparkle.framework "$APP/Contents/Frameworks/Sparkle.framework"
python3 scripts/updater/sparkle.py verify "$APP/Contents/Frameworks"
cp "$CARGO_TARGET_DIR/aarch64-apple-darwin/release/okilum" "$APP/Contents/MacOS/okilum"
supervisor_stage "$APP" "$CARGO_TARGET_DIR" aarch64-apple-darwin
python3 scripts/brand-assets.py verify
# The example binary runs outside the bundle; point it at the source framework.
env DYLD_FRAMEWORK_PATH="$PWD/vendor/sparkle" cargo run --release --locked --target aarch64-apple-darwin -p okilum-shell --example macos_app_icon -- "$OUTPUT/icon-rasters"
python3 scripts/macos-icon.py "$OUTPUT/icon-rasters" "$APP/Contents/Resources/Okilum.icns"
cat >"$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>okilum</string>
<key>CFBundleIconFile</key><string>Okilum.icns</string>
<key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
<key>CFBundleName</key><string>Okilum</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleDocumentTypes</key>
<array><dict>
  <key>CFBundleTypeName</key><string>Markdown document</string>
  <key>CFBundleTypeRole</key><string>Viewer</string>
  <key>LSHandlerRank</key><string>Alternate</string>
  <key>LSItemContentTypes</key><array><string>public.markdown</string><string>net.daringfireball.markdown</string></array>
</dict></array>
<key>CFBundleURLTypes</key>
<array><dict>
  <key>CFBundleURLName</key><string>$BUNDLE_ID.link</string>
  <key>CFBundleURLSchemes</key><array><string>okilum</string></array>
</dict></array>
<key>UTImportedTypeDeclarations</key>
<array><dict>
  <key>UTTypeIdentifier</key><string>public.markdown</string>
  <key>UTTypeDescription</key><string>Markdown document</string>
  <key>UTTypeConformsTo</key><array><string>public.plain-text</string></array>
  <key>UTTypeTagSpecification</key><dict>
    <key>public.filename-extension</key><array><string>md</string></array>
    <key>public.mime-type</key><string>text/markdown</string>
  </dict>
</dict><dict>
  <key>UTTypeIdentifier</key><string>net.daringfireball.markdown</string>
  <key>UTTypeDescription</key><string>Markdown document</string>
  <key>UTTypeConformsTo</key><array><string>public.plain-text</string></array>
  <key>UTTypeTagSpecification</key><dict>
    <key>public.filename-extension</key><array><string>md</string></array>
    <key>public.mime-type</key><string>text/markdown</string>
  </dict>
</dict></array>
<key>CFBundleShortVersionString</key><string>$DISPLAY_VERSION</string>
<key>CFBundleVersion</key><string>$OKILUM_BUILD_VERSION</string>
<key>NSHumanReadableCopyright</key><string>Source $SOURCE_SHA</string>
<key>OkilumSourceCommit</key><string>$SOURCE_SHA</string>
<key>OkilumSourceTree</key><string>$SOURCE_TREE</string>
<key>SUFeedURL</key><string>https://updates.befeast.com/okilum/appcast.xml</string>
<key>SUPublicEDKey</key><string>$SPARKLE_PUBLIC_ED_KEY</string>
<key>SUEnableAutomaticChecks</key><true/>
<key>SUScheduledCheckInterval</key><integer>3600</integer>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
plutil -lint "$APP/Contents/Info.plist"
[[ $(plutil -extract CFBundleURLTypes.0.CFBundleURLSchemes.0 raw "$APP/Contents/Info.plist") == okilum ]]

source scripts/updater/sign-bundle.sh
source scripts/updater/make-dmg.sh
cat >"$OUTPUT/release.env" <<EOF
BUILD=$OKILUM_BUILD_VERSION
DISPLAY_VERSION=$DISPLAY_VERSION
SOURCE_SHA=$SOURCE_SHA
SOURCE_TREE=$SOURCE_TREE
ARCHIVE=$ARCHIVE
DMG=$DMG
SIGN_UPDATE=$(find "$OUTPUT/sparkle-bin" -name sign_update -type f | head -1)
EOF
cat "$OUTPUT/release.env"

release_cache_stats
