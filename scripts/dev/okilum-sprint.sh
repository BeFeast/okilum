#!/bin/bash
# Local-only Mac sprint build. Run in Terminal, never through the release lane.
set -Eeuo pipefail
if [[ $# != 1 ]]; then echo "Usage: $0 <branch>" >&2; exit 2; fi
if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    echo 'Okilum Sprint requires an Apple Silicon Mac.' >&2; exit 1
fi
if [[ -f $HOME/.cargo/env ]]; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null; then
    echo "Install Rust, then rerun: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
    exit 1
fi
branch=$1
repo=https://git.oklabs.uk/BeFeast/okilum.git
git check-ref-format --branch "$branch" >/dev/null
root="${OKILUM_SPRINT_HOME:-$HOME/Developer/personal/okilum-sprint}"
root=${root%/}
if [[ $root != /* || $root == / || $root == "$HOME" ]]; then
    echo 'OKILUM_SPRINT_HOME must be an absolute, dedicated directory.' >&2; exit 1
fi
app="$HOME/Applications/Okilum Sprint.app"
# Refuse symlinked workspaces/destinations rather than affecting another checkout.
for path in "$root" "$root/src" "$root/target" "$app" "$HOME/Applications"; do
    if [[ -L $path ]]; then echo "Refusing symlink: $path" >&2; exit 1; fi
done
legacy="$HOME/okilum-sprint"
if [[ ! -e $root && -e $legacy && $root != "$legacy" ]]; then
    if [[ -L $legacy || -L $legacy/src || -L $legacy/target || ! -d $legacy || -e $legacy/build.lock ]]; then
        echo "Cannot migrate $legacy: expected an idle, real Sprint directory." >&2; exit 1
    fi
    mkdir -p "$(dirname "$root")"
    mv "$legacy" "$root"
    printf 'Moved Sprint checkout and warm cache: %s → %s\n' "$legacy" "$root"
fi
mkdir -p "$root" "$HOME/Applications"
if ! mkdir "$root/build.lock" 2>/dev/null; then
    echo "Sprint build is already running (lock: $root/build.lock)." >&2; exit 1
fi
trap 'rmdir "$root/build.lock"' EXIT
started=$SECONDS
if [[ ! -e $root/src ]]; then git clone "$repo" "$root/src"; fi
cd "$root/src"
[[ $(git rev-parse --show-toplevel) == "$root/src" ]] || { echo 'Not the dedicated sprint checkout.' >&2; exit 1; }
[[ $(git remote get-url origin) == "$repo" ]] || { echo 'Unexpected sprint origin.' >&2; exit 1; }
if [[ -n $(git status --porcelain --untracked-files=normal) ]]; then
    echo "Sprint checkout has local changes; preserve them before rebuilding: $root/src" >&2; exit 1
fi
git fetch origin "refs/heads/$branch"
git checkout -B sprint FETCH_HEAD
sha=$(git rev-parse HEAD)
short_sha=$(git rev-parse --short HEAD)
export CC=/usr/bin/clang CXX=/usr/bin/clang++
SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
export SDKROOT
export CARGO_TARGET_DIR="$root/target" CARGO_INCREMENTAL=1
export OKILUM_SOURCE_COMMIT="$sha" OKILUM_BUILD_VERSION=1
export OKILUM_RELEASE_VERSION="Sprint $short_sha"
bash scripts/vendor-setup.sh
bash scripts/vendor-setup.sh --verify
# The binary links Sparkle, but this app has no feed or updater controller.
# Reuse the release lane's pinned framework loader, not a new download URL.
archive="$root/Sparkle.tar.xz"
if [[ ! -f $archive ]]; then python3 scripts/updater/sparkle.py fetch "$archive"; fi
python3 scripts/updater/sparkle.py prepare --archive "$archive" --destination vendor/sparkle
log="$root/build.log"
if ! cargo build --locked --profile sprint --target aarch64-apple-darwin -p okilum-shell 2>&1 | tee "$log"; then
    echo "Sprint compile failed; first compiler error (full log: $log):" >&2
    awk '/^error/ {print; n=8} n>0 {if ($0 !~ /^error/) print; n--}' "$log" | head -25 >&2 || true
    exit 1
fi
stage="$root/Okilum Sprint.app"
rm -rf "$stage"
mkdir -p "$stage/Contents/MacOS" "$stage/Contents/Resources" "$stage/Contents/Frameworks"
cp "$CARGO_TARGET_DIR/aarch64-apple-darwin/sprint/okilum" "$stage/Contents/MacOS/okilum"
ditto vendor/sparkle/Sparkle.framework "$stage/Contents/Frameworks/Sparkle.framework"
python3 scripts/third-party-notices.py --stage "$stage/Contents/Resources/Licenses"
# Apply isolation on every launch, including Finder launches after this terminal closes.
cat > "$stage/Contents/MacOS/sprint-launch" <<'LAUNCH'
#!/bin/bash
set -euo pipefail
export OKILUM_STATE_DIR="$HOME/Library/Application Support/com.befeast.okilum.sprint"
export XDG_CONFIG_HOME="$OKILUM_STATE_DIR/config"
export XDG_CACHE_HOME="$OKILUM_STATE_DIR/cache"
export XDG_STATE_HOME="$OKILUM_STATE_DIR/state"
export XDG_DATA_HOME="$OKILUM_STATE_DIR/data"
mkdir -p "$OKILUM_STATE_DIR" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_STATE_HOME" "$XDG_DATA_HOME"
cd "$(dirname "$0")"
exec ./okilum "$@"
LAUNCH
chmod +x "$stage/Contents/MacOS/sprint-launch"
# Reuse the canonical bundle/Info.plist template from build-macos-ci.sh.
python3 - "$stage" "$sha" <<'PY'
import pathlib, plistlib, sys
stage, sha = pathlib.Path(sys.argv[1]), sys.argv[2]
source = pathlib.Path('scripts/build-macos-ci.sh').read_text()
template = source.split('cat >"$APP/Contents/Info.plist" <<PLIST\n', 1)[1].split('\nPLIST\n', 1)[0]
for key, value in {'BUNDLE_ID':'com.befeast.okilum.sprint', 'DISPLAY_VERSION':'0.1.1',
                   'OKILUM_BUILD_VERSION':'1', 'SOURCE_SHA':sha, 'SOURCE_TREE':''}.items():
    template = template.replace('$' + key, value)
info = plistlib.loads(template.encode())
for key in list(info):
    if key.startswith('SU'):
        del info[key]
# Do not register the working app's document associations or URL protocol.
info.pop('CFBundleDocumentTypes', None)
info.pop('CFBundleURLTypes', None)
info.update(CFBundleExecutable='sprint-launch', CFBundleName='Okilum Sprint',
            CFBundleDisplayName='Okilum Sprint', SUEnableAutomaticChecks=False,
            NSHumanReadableCopyright=f'Sprint source {sha}')
(stage / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
PY
plutil -lint "$stage/Contents/Info.plist"
# Reuse the approved icon rasterizer; add a bright amber sprint badge.
env DYLD_FRAMEWORK_PATH="$PWD/vendor/sparkle" cargo run --locked --profile sprint \
    --target aarch64-apple-darwin -p okilum-shell --example macos_app_icon -- "$root/icon-rasters"
python3 - "$root/icon-rasters" <<'PY'
from pathlib import Path
import sys
for path in Path(sys.argv[1]).glob('*.rgba'):
    size = int(path.stem)
    rgba = bytearray(path.read_bytes())
    for y in range(size * 2 // 3, size * 15 // 16):
        for x in range(size * 2 // 3, size * 15 // 16):
            offset = (y * size + x) * 4
            rgba[offset:offset + 4] = bytes((255, 176, 0, 255))
    path.write_bytes(rgba)
PY
python3 scripts/macos-icon.py "$root/icon-rasters" "$stage/Contents/Resources/Okilum.icns"
codesign --force --deep --sign - "$stage"
codesign --verify --deep --strict "$stage"
# Target only the Sprint bundle ID. Quit must finish before replacing its bundle.
if [[ -d $app ]]; then
    osascript -e 'if application id "com.befeast.okilum.sprint" is running then tell application id "com.befeast.okilum.sprint" to quit'
    for ((attempt=0; attempt<15; attempt++)); do
        running=$(osascript -e 'application id "com.befeast.okilum.sprint" is running')
        [[ $running == false ]] && break
        sleep 1
    done
    [[ $running == false ]] || { echo 'Quit Okilum Sprint, then rerun; app was not replaced.' >&2; exit 1; }
fi
# Only these dedicated Sprint paths can be replaced; keep the previous app on failure.
backup="$root/previous-Okilum-Sprint.app"
rm -rf "$backup"
if [[ -e $app ]]; then mv "$app" "$backup"; fi
if ! ditto "$stage" "$app"; then
    rm -rf "$app"
    if [[ -e $backup ]]; then mv "$backup" "$app"; fi
    exit 1
fi
open "$app"
printf 'Sprint ready: %s %s · %ss · %s\n' "$branch" "$short_sha" "$((SECONDS - started))" "$app"
