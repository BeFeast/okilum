#!/usr/bin/env bash
# Cross-compile a measurement-only Windows okilum.exe with the frame trace (#1290).
#
#   scripts/perf/build-windows-trace.sh OUTDIR [--no-debug-assertions]
#
# Same profile and flags as scripts/build-windows-ci.sh (windows-diagnostic, crt-static,
# --no-default-features), but against throwaway copies of gpui-pre and gpui-pre-windows that
# scripts/perf/frame-trace/patch.py instruments. --no-debug-assertions turns off the profile's
# debug-assertions, to measure what they cost. OUTDIR gets okilum.exe and gpui-shaders/ (the
# runtime HLSL the Windows renderer compiles), ready to copy to a Windows host. Nothing here is
# published, and the repository sources and Cargo.lock are left as they were.
# Run from a checkout whose vendor/ is set up (scripts/vendor-setup.sh).
set -euo pipefail
cd "$(dirname "$0")/../.."
[ $# -ge 1 ] || { sed -n '2,11p' "$0"; exit 2; }
out=$(realpath -m "$1"); shift
extra=()
while [ $# -gt 0 ]; do
    case $1 in
        --no-debug-assertions) extra+=(--config 'profile.windows-diagnostic.debug-assertions=false') ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
    shift
done
scripts/vendor-setup.sh --verify
mkdir -p "$out"
work="$out/.work"
rm -rf "$work"
mkdir -p "$work" "$out/gpui-shaders/src"

package_dir() {
    cargo metadata --locked --format-version 1 | python3 -c '
import json, sys
from pathlib import Path
name = sys.argv[1]
packages = json.load(sys.stdin)["packages"]
print(Path(next(p for p in packages if p["name"] == name)["manifest_path"]).parent)
' "$1"
}
cp -r "$(package_dir gpui-pre)" "$work/gpui-pre"
cp -r "$(package_dir gpui-pre-windows)" "$work/gpui-pre-windows"
python3 scripts/perf/frame-trace/patch.py "$work/gpui-pre" "$work/gpui-pre-windows"

# The patch changes the dependency graph, so --locked cannot hold; the lockfile is put back.
cp Cargo.lock "$work/Cargo.lock.orig"
trap 'cp "$work/Cargo.lock.orig" Cargo.lock' EXIT

export OKILUM_SOURCE_COMMIT
OKILUM_SOURCE_COMMIT=$(git rev-parse HEAD)
export OKILUM_BUILD_VERSION="perf-${OKILUM_SOURCE_COMMIT:0:8}"
export OKILUM_RELEASE_VERSION="0.1.0-perf-${OKILUM_SOURCE_COMMIT:0:8}"
# The icon is cosmetic here but must be a valid .ico (llvm-rc refuses a truncated one); the
# production script renders the brand SVG with CairoSVG, this needs only the standard library.
export OKILUM_WINDOWS_ICON="${OKILUM_WINDOWS_ICON:-$out/okilum.ico}"
if [ ! -f "$OKILUM_WINDOWS_ICON" ]; then
    python3 scripts/perf/mini-ico.py "$OKILUM_WINDOWS_ICON"
fi
export RUSTFLAGS="-C target-feature=+crt-static"
export XWIN_ARCH=x86_64 XWIN_VARIANT=desktop XWIN_VERSION=17
export CARGO_INCREMENTAL=0
# shellcheck source=/dev/null
source scripts/ci/release-cache.sh
export OKILUM_SCCACHE="${RUSTC_WRAPPER:-}"
export RUSTC_WRAPPER="$PWD/scripts/windows-rustc.py"
export RC_x86_64_pc_windows_msvc="$PWD/scripts/windows-rc.py"

cargo xwin build --target x86_64-pc-windows-msvc \
    --profile windows-diagnostic -p okilum-shell --bin okilum --no-default-features \
    --config "patch.crates-io.gpui-pre.path='$work/gpui-pre'" \
    --config "patch.crates-io.gpui-pre-windows.path='$work/gpui-pre-windows'" \
    "${extra[@]}"

cp "${CARGO_TARGET_DIR:-target}/x86_64-pc-windows-msvc/windows-diagnostic/okilum.exe" "$out/okilum.exe"
for name in shaders.hlsl color_text_raster.hlsl alpha_correction.hlsl; do
    cp "$work/gpui-pre-windows/src/$name" "$out/gpui-shaders/src/$name"
done
(cd "$out" && sha256sum okilum.exe gpui-shaders/src/*.hlsl > SHA256SUMS)
cat > "$out/BUILD.txt" <<EOF
commit $OKILUM_SOURCE_COMMIT
profile windows-diagnostic ${extra[*]:-(debug-assertions on, as shipped)}
trace OKILUM_FRAME_TRACE=<file> writes [frame] and [win] lines; unset = no tracing
EOF
cat "$out/BUILD.txt"
