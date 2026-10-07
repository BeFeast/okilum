#!/bin/bash
# Artifact-only Monterey QA build; independent of the arm64 release publisher.
set -Eeuo pipefail
OUTPUT="${1:?output directory required}"
: "${TESSERA_BUILD_VERSION:?}" "${TESSERA_SIGNING_IDENTITY:?}"
[[ $TESSERA_BUILD_VERSION =~ ^[1-9][0-9]*$ ]]
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]]
mkdir -p "$OUTPUT"
OUTPUT="$(cd "$OUTPUT" && pwd)"
if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
export CC=/usr/bin/clang CXX=/usr/bin/clang++
export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
export MACOSX_DEPLOYMENT_TARGET=12.0
export TESSERA_RELEASE_VERSION="0.1.$TESSERA_BUILD_VERSION"
export TESSERA_SOURCE_COMMIT="$(git rev-parse HEAD)"
export CARGO_TARGET_DIR="$HOME/.cache/tessera-macos/reader-intel-qa"
export CARGO_INCREMENTAL=0
rustc --version
rustup target add x86_64-apple-darwin
bash scripts/vendor-setup.sh
bash scripts/vendor-setup.sh --verify
source scripts/ci/release-cache.sh
# A stats response alone does not prove the daemon can serve rustc. Keep this
# artifact-only build usable if the shared cache daemon fails during startup.
if [[ -n ${RUSTC_WRAPPER:-} ]] && ! "$RUSTC_WRAPPER" "$(rustup which rustc)" -vV; then
  echo 'Intel QA compiler cache probe failed; compiling without the wrapper'
  unset RUSTC_WRAPPER
fi
python3 scripts/updater/sparkle.py fetch "$OUTPUT/Sparkle.tar.xz"
python3 scripts/updater/sparkle.py prepare --archive "$OUTPUT/Sparkle.tar.xz" --destination vendor/sparkle
SECONDS=0
cargo build --release --locked --target x86_64-apple-darwin -p tessera-shell
BUILD_SECONDS=$SECONDS
APP="$OUTPUT/Tessera Intel QA.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/release/tessera" "$APP/Contents/MacOS/tessera"
ditto vendor/sparkle/Sparkle.framework "$APP/Contents/Frameworks/Sparkle.framework"
python3 scripts/third-party-notices.py --stage "$APP/Contents/Resources/Licenses"
# Deliberately no SUFeedURL: this QA app cannot offer arm64 feed updates.
python3 - "$APP" <<'PY'
import os, plistlib, sys
from pathlib import Path
p=Path(sys.argv[1])/'Contents/Info.plist'
p.write_bytes(plistlib.dumps(dict(CFBundleExecutable='tessera',
 CFBundleIdentifier='uk.oklabs.tessera.intel-qa',CFBundleName='Tessera Intel QA',
 CFBundlePackageType='APPL',CFBundleVersion=os.environ['TESSERA_BUILD_VERSION'],
 CFBundleShortVersionString=os.environ['TESSERA_RELEASE_VERSION'],
 LSMinimumSystemVersion='12.0',NSHighResolutionCapable=True,
 TesseraSourceCommit=os.environ['TESSERA_SOURCE_COMMIT'])))
PY
[[ $(lipo -archs "$APP/Contents/MacOS/tessera") == x86_64 ]]
# Reject an accidentally newer deployment target in the binary or bundled code.
python3 scripts/verify-macos-intel-qa.py "$APP"
FRAMEWORK="$APP/Contents/Frameworks/Sparkle.framework"
for COMPONENT in \
 "$FRAMEWORK/Versions/B/XPCServices/Downloader.xpc" \
 "$FRAMEWORK/Versions/B/XPCServices/Installer.xpc" \
 "$FRAMEWORK/Versions/B/Updater.app" \
 "$FRAMEWORK/Versions/B/Autoupdate" "$FRAMEWORK" "$APP"; do
 codesign --force --preserve-metadata=identifier,entitlements --options runtime --timestamp --sign "$TESSERA_SIGNING_IDENTITY" "$COMPONENT"
done
codesign --verify --deep --strict --verbose=2 "$APP"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$OUTPUT/Tessera-intel-qa-$TESSERA_BUILD_VERSION.zip"
printf 'Build seconds: %s\n' "$BUILD_SECONDS" | tee "$OUTPUT/metrics.txt"
stat -f 'Executable bytes: %z' "$APP/Contents/MacOS/tessera" | tee -a "$OUTPUT/metrics.txt"
release_cache_stats
