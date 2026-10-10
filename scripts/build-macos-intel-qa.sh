#!/bin/bash
# Artifact-only Monterey QA build; independent of the arm64 release publisher.
# Runs as a cross-build on Apple Silicon or natively on an Intel Mac (Hedva, #1011).
# OKILUM_SIGNING_IDENTITY=- signs ad-hoc, for a QA Mac without a Developer ID.
set -Eeuo pipefail
OUTPUT="${1:?output directory required}"
: "${OKILUM_BUILD_VERSION:?}" "${OKILUM_SIGNING_IDENTITY:?}"
[[ $OKILUM_BUILD_VERSION =~ ^[1-9][0-9]*$ ]]
[[ $(uname -s) == Darwin ]]
[[ $(uname -m) == arm64 || $(uname -m) == x86_64 ]]
mkdir -p "$OUTPUT"
OUTPUT="$(cd "$OUTPUT" && pwd)"
if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
export CC=/usr/bin/clang CXX=/usr/bin/clang++
export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
export MACOSX_DEPLOYMENT_TARGET=12.0
export OKILUM_RELEASE_VERSION="0.1.$OKILUM_BUILD_VERSION"
export OKILUM_SOURCE_COMMIT="$(git rev-parse HEAD)"
mkdir -p "$HOME/.cache/okilum-qa/634"
CARGO_TARGET_DIR=$(mktemp -d "$HOME/.cache/okilum-qa/634/intel-no-cache.XXXXXX")
export CARGO_TARGET_DIR
# This unique target belongs only to this job; retain compiler.log, not build artifacts.
trap 'rm -rf -- "$CARGO_TARGET_DIR"' EXIT
export CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_STRIP=none
export CARGO_INCREMENTAL=0
rustc --version
rustup target add x86_64-apple-darwin
bash scripts/vendor-setup.sh
bash scripts/vendor-setup.sh --verify
# Isolate Intel QA from the shared cache daemon, including inherited wrappers.
export RUSTC_WRAPPER="" RUSTC_WORKSPACE_WRAPPER=""
printf 'Intel QA: cache disabled; target=%s; host strip=%s; deployment=%s\n' \
  "$CARGO_TARGET_DIR" "$CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_STRIP" "$MACOSX_DEPLOYMENT_TARGET"
python3 scripts/updater/sparkle.py fetch "$OUTPUT/Sparkle.tar.xz"
python3 scripts/updater/sparkle.py prepare --archive "$OUTPUT/Sparkle.tar.xz" --destination vendor/sparkle
SECONDS=0
cargo build -vv --release --locked --target x86_64-apple-darwin -p okilum-shell 2>&1 | tee "$OUTPUT/compiler.log"
BUILD_SECONDS=$SECONDS
APP="$OUTPUT/Okilum Intel QA.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/release/okilum" "$APP/Contents/MacOS/okilum"
ditto vendor/sparkle/Sparkle.framework "$APP/Contents/Frameworks/Sparkle.framework"
python3 scripts/third-party-notices.py --stage "$APP/Contents/Resources/Licenses"
# Deliberately no SUFeedURL: this QA app cannot offer arm64 feed updates.
python3 - "$APP" <<'PY'
import os, plistlib, sys
from pathlib import Path
p=Path(sys.argv[1])/'Contents/Info.plist'
p.write_bytes(plistlib.dumps(dict(CFBundleExecutable='okilum',
 CFBundleIdentifier='com.befeast.okilum.intel-qa',CFBundleName='Okilum Intel QA',
 CFBundlePackageType='APPL',CFBundleVersion=os.environ['OKILUM_BUILD_VERSION'],
 CFBundleShortVersionString=os.environ['OKILUM_RELEASE_VERSION'],
 LSMinimumSystemVersion='12.0',NSHighResolutionCapable=True,
 OkilumSourceCommit=os.environ['OKILUM_SOURCE_COMMIT'])))
PY
[[ $(lipo -archs "$APP/Contents/MacOS/okilum") == x86_64 ]]
# Reject an accidentally newer deployment target in the binary or bundled code.
python3 scripts/verify-macos-intel-qa.py "$APP"
FRAMEWORK="$APP/Contents/Frameworks/Sparkle.framework"
# Ad-hoc signatures carry neither a hardened runtime nor a secure timestamp.
SIGN_OPTIONS=(--options runtime --timestamp)
[[ $OKILUM_SIGNING_IDENTITY == - ]] && SIGN_OPTIONS=()
for COMPONENT in \
 "$FRAMEWORK/Versions/B/XPCServices/Downloader.xpc" \
 "$FRAMEWORK/Versions/B/XPCServices/Installer.xpc" \
 "$FRAMEWORK/Versions/B/Updater.app" \
 "$FRAMEWORK/Versions/B/Autoupdate" "$FRAMEWORK" "$APP"; do
 # The guarded expansion keeps an empty array legal under set -u in macOS's bash 3.2.
 codesign --force --preserve-metadata=identifier,entitlements ${SIGN_OPTIONS[@]+"${SIGN_OPTIONS[@]}"} \
  --sign "$OKILUM_SIGNING_IDENTITY" "$COMPONENT"
done
codesign --verify --deep --strict --verbose=2 "$APP"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$OUTPUT/Okilum-intel-qa-$OKILUM_BUILD_VERSION.zip"
printf 'Build seconds: %s\n' "$BUILD_SECONDS" | tee "$OUTPUT/metrics.txt"
stat -f 'Executable bytes: %z' "$APP/Contents/MacOS/okilum" | tee -a "$OUTPUT/metrics.txt"
