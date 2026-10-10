#!/bin/bash
# PR gate for the supervisor packaging (#1013): runs the same functions as the release
# build (scripts/ci/supervisor-bundle.sh) on a real Mac with synthetic signing values and
# an ad-hoc signature, then checks what a release would ship. Run by check-macos.sh as a
# child process after its cargo environment is exported; it needs CARGO_TARGET_DIR and
# nothing signed.
set -Eeuo pipefail
source scripts/ci/supervisor-bundle.sh

TRIPLE=aarch64-apple-darwin
TEAM=ABCDE12345            # synthetic: never a real team
BUNDLE=org.example.app     # synthetic: never the real bundle identifier
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
BIN="$CARGO_TARGET_DIR/$TRIPLE/release/$SUPERVISOR_NAME"

# Negative control: a build without signing configuration carries neither value, so the
# presence check below cannot be satisfied by accident.
cargo build --release --locked --target "$TRIPLE" -p okilum-sync-supervisor
cp "$BIN" "$WORK/unconfigured"
# (`! cmd` never trips `set -e`, so the failure is explicit.)
if strings "$WORK/unconfigured" | grep -F -e "$TEAM" -e "$BUNDLE" >/dev/null; then
    echo "an unconfigured build contains signing values" >&2
    exit 1
fi

# The same build step as the release, with synthetic values.
supervisor_build "$TEAM" "$BUNDLE" "$TRIPLE"
strings "$BIN" | grep -F "$TEAM" >/dev/null
strings "$BIN" | grep -F "$BUNDLE" >/dev/null

# A minimal bundle with the same layout the release assembles.
APP="$WORK/Test.app"
mkdir -p "$APP/Contents/MacOS"
cp /usr/bin/true "$APP/Contents/MacOS/okilum"
supervisor_stage "$APP" "$CARGO_TARGET_DIR" "$TRIPLE"
test -x "$APP/$SUPERVISOR_RELATIVE"
[[ $(lipo -archs "$APP/$SUPERVISOR_RELATIVE") == arm64 ]]

# The static LaunchAgent plist registers the helper by its bundle-relative path.
PLIST_FILE="$(echo "$APP"/Contents/Library/LaunchAgents/*.plist)"
[[ $(plutil -extract Label raw "$PLIST_FILE") == com.befeast.okilum.sync ]]
[[ $(plutil -extract BundleProgram raw "$PLIST_FILE") == "$SUPERVISOR_RELATIVE" ]]

# Ad-hoc signature, as the release does with the Developer ID (without the timestamp,
# which needs the network): the explicit identifier is what the app's policy requires.
supervisor_sign "$APP" - "$BUNDLE"
codesign -dv "$APP/$SUPERVISOR_RELATIVE" 2>&1 | grep -Fx "Identifier=$BUNDLE.sync" >/dev/null
codesign -dv --verbose=4 "$APP/$SUPERVISOR_RELATIVE" 2>&1 | grep -E 'flags=.*runtime' >/dev/null

# The Team ID helper: a configured team is used when the keychain cannot say (positive
# control), and with neither source it refuses instead of guessing.
[[ $(OKILUM_SIGNING_TEAM_ID=$TEAM supervisor_team_id "no such identity") == "$TEAM" ]]
if (unset OKILUM_SIGNING_TEAM_ID; supervisor_team_id "no such identity") 2>/dev/null; then
    echo "a team was derived for an identity that does not exist" >&2
    exit 1
fi
echo "supervisor bundle: build config compiled in (and absent without it), layout, plist and signature identifier verified"
