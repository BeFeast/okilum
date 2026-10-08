#!/bin/bash
# Build Tessera.app on the macOS runner, then sign, notarize, staple and zip it.
# Usage: build-macos-ci.sh OUTPUT_DIR. Runs from the repository root.
#   TESSERA_BUILD_VERSION   monotonic CFBundleVersion (CI: 5000 + run number)
#   TESSERA_SIGNING_IDENTITY, TESSERA_NOTARY_PROFILE   existing runner keychain items
#   SPARKLE_PUBLIC_ED_KEY   SUPublicEDKey value
set -Eeuo pipefail
OUTPUT="${1:?output directory required}"
: "${TESSERA_BUILD_VERSION:?}" "${TESSERA_SIGNING_IDENTITY:?}" "${TESSERA_NOTARY_PROFILE:?}" "${SPARKLE_PUBLIC_ED_KEY:?}"
[[ $TESSERA_BUILD_VERSION =~ ^[1-9][0-9]*$ ]]
DISPLAY_VERSION="0.1.$TESSERA_BUILD_VERSION"
SOURCE_SHA="$(git rev-parse HEAD)"
export TESSERA_RELEASE_VERSION="$DISPLAY_VERSION" TESSERA_SOURCE_COMMIT="$SOURCE_SHA"
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
export CARGO_TARGET_DIR="$HOME/.cache/tessera-macos/reader-arm64"
export CARGO_INCREMENTAL=0
source scripts/ci/release-cache.sh
SPARKLE_ARCHIVE="$OUTPUT/Sparkle-2.10.0.tar.xz"
python3 scripts/updater/sparkle.py fetch "$SPARKLE_ARCHIVE"
python3 scripts/updater/sparkle.py prepare --archive "$SPARKLE_ARCHIVE" --destination vendor/sparkle
mkdir -p "$OUTPUT/sparkle-bin"
tar -xJf "$SPARKLE_ARCHIVE" -C "$OUTPUT/sparkle-bin" --include='*bin/sign_update'
cargo build --release --locked --target aarch64-apple-darwin -p tessera-shell

APP="$OUTPUT/Tessera.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
python3 scripts/third-party-notices.py --stage "$APP/Contents/Resources/Licenses"
ditto vendor/sparkle/Sparkle.framework "$APP/Contents/Frameworks/Sparkle.framework"
python3 scripts/updater/sparkle.py verify "$APP/Contents/Frameworks"
cp "$CARGO_TARGET_DIR/aarch64-apple-darwin/release/tessera" "$APP/Contents/MacOS/tessera"
python3 scripts/brand-assets.py verify
# The example binary runs outside the bundle; point it at the source framework.
env DYLD_FRAMEWORK_PATH="$PWD/vendor/sparkle" cargo run --release --locked --target aarch64-apple-darwin -p tessera-shell --example macos_app_icon -- "$OUTPUT/icon-rasters"
python3 scripts/macos-icon.py "$OUTPUT/icon-rasters" "$APP/Contents/Resources/Tessera.icns"
cat >"$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>tessera</string>
<key>CFBundleIconFile</key><string>Tessera.icns</string>
<key>CFBundleIdentifier</key><string>uk.oklabs.tessera</string>
<key>CFBundleName</key><string>Tessera</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleDocumentTypes</key>
<array><dict>
  <key>CFBundleTypeName</key><string>Markdown document</string>
  <key>CFBundleTypeRole</key><string>Viewer</string>
  <key>LSHandlerRank</key><string>Alternate</string>
  <key>LSItemContentTypes</key><array><string>public.markdown</string><string>net.daringfireball.markdown</string></array>
</dict><dict>
  <key>CFBundleTypeName</key><string>Log file</string>
  <key>CFBundleTypeRole</key><string>Viewer</string>
  <key>LSHandlerRank</key><string>Alternate</string>
  <key>LSItemContentTypes</key><array><string>public.log</string><string>com.apple.log</string><string>uk.oklabs.tessera.json-lines</string><string>uk.oklabs.tessera.logfmt</string></array>
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
</dict><dict>
  <key>UTTypeIdentifier</key><string>uk.oklabs.tessera.json-lines</string>
  <key>UTTypeDescription</key><string>JSON lines log</string>
  <key>UTTypeConformsTo</key><array><string>public.plain-text</string></array>
  <key>UTTypeTagSpecification</key><dict>
    <key>public.filename-extension</key><array><string>jsonl</string><string>ndjson</string></array>
  </dict>
</dict><dict>
  <key>UTTypeIdentifier</key><string>uk.oklabs.tessera.logfmt</string>
  <key>UTTypeDescription</key><string>logfmt log</string>
  <key>UTTypeConformsTo</key><array><string>public.plain-text</string></array>
  <key>UTTypeTagSpecification</key><dict>
    <key>public.filename-extension</key><array><string>logfmt</string></array>
  </dict>
</dict></array>
<key>CFBundleShortVersionString</key><string>$DISPLAY_VERSION</string>
<key>CFBundleVersion</key><string>$TESSERA_BUILD_VERSION</string>
<key>NSHumanReadableCopyright</key><string>Source $SOURCE_SHA</string>
<key>TesseraSourceCommit</key><string>$SOURCE_SHA</string>
<key>TesseraSourceTree</key><string>$SOURCE_TREE</string>
<key>SUFeedURL</key><string>https://updates.befeast.com/tessera/appcast.xml</string>
<key>SUPublicEDKey</key><string>$SPARKLE_PUBLIC_ED_KEY</string>
<key>SUEnableAutomaticChecks</key><true/>
<key>SUScheduledCheckInterval</key><integer>3600</integer>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
plutil -lint "$APP/Contents/Info.plist"

source scripts/updater/sign-bundle.sh
cat >"$OUTPUT/release.env" <<EOF
BUILD=$TESSERA_BUILD_VERSION
DISPLAY_VERSION=$DISPLAY_VERSION
SOURCE_SHA=$SOURCE_SHA
SOURCE_TREE=$SOURCE_TREE
ARCHIVE=$ARCHIVE
SIGN_UPDATE=$(find "$OUTPUT/sparkle-bin" -name sign_update -type f | head -1)
EOF
cat "$OUTPUT/release.env"

release_cache_stats
