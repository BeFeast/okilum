#!/usr/bin/env bash
# Build a measurement-only macOS okilum with the frame trace (#1004), on the Mac itself.
#
#   scripts/perf/build-mac-trace.sh OUTDIR
#
# Same flags as the release build, against throwaway copies of gpui-pre and gpui-pre-apple that
# scripts/perf/frame-trace/patch.py instruments (frame phases, text shaping, and the Metal
# present split: next_drawable / encode / commit). OUTDIR gets okilum and the compiled wheel
# injector. Nothing is published; the checkout's sources and Cargo.lock are left as they were.
# Hedva is shared and is Oleg's audio rig: hold /tmp/okilum-hedva-qa.lock first (shlock) and
# the build runs at nice 10 with 3 jobs. Run from a checkout whose vendor/ is set up.
set -euo pipefail
cd "$(dirname "$0")/../.."
[ $# -eq 1 ] || { sed -n '2,10p' "$0"; exit 2; }
[ -e /tmp/okilum-hedva-qa.lock ] || { echo "the Hedva QA lock is not held" >&2; exit 1; }
# macOS has no realpath
out=$(python3 -c 'import os, sys; print(os.path.abspath(sys.argv[1]))' "$1")
case "$(uname -m)" in
    x86_64) target=x86_64-apple-darwin ;;
    arm64) target=aarch64-apple-darwin ;;
    *) echo "unknown arch $(uname -m)" >&2; exit 1 ;;
esac
scripts/vendor-setup.sh --verify
mkdir -p "$out"
work="$out/.work"
rm -rf "$work"
mkdir -p "$work"

package_dir() {
    cargo metadata --locked --format-version 1 --filter-platform "$target" | python3 -c '
import json, sys
from pathlib import Path
name = sys.argv[1]
packages = json.load(sys.stdin)["packages"]
print(Path(next(p for p in packages if p["name"] == name)["manifest_path"]).parent)
' "$1"
}
cp -R "$(package_dir gpui-pre)" "$work/gpui-pre"
cp -R "$(package_dir gpui-pre-apple)" "$work/gpui-pre-apple"
python3 scripts/perf/frame-trace/patch.py "$work/gpui-pre" - "$work/gpui-pre-apple"

# The patch changes the dependency graph, so --locked cannot hold; the lockfile is put back.
cp Cargo.lock "$work/Cargo.lock.orig"
trap 'cp "$work/Cargo.lock.orig" Cargo.lock' EXIT

export CC=/usr/bin/clang CXX=/usr/bin/clang++
SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
export SDKROOT
export MACOSX_DEPLOYMENT_TARGET=12.0
export CARGO_INCREMENTAL=0 RUSTC_WRAPPER="" RUSTC_WORKSPACE_WRAPPER="" CARGO_BUILD_JOBS=3
start=$(date +%s)
nice -n 10 cargo build --release --target "$target" -p okilum-shell --bin okilum \
    --config "patch.crates-io.gpui-pre.path='$work/gpui-pre'" \
    --config "patch.crates-io.gpui-pre-apple.path='$work/gpui-pre-apple'"
cp "${CARGO_TARGET_DIR:-target}/$target/release/okilum" "$out/okilum"
swiftc -O scripts/perf/scroll/mac/wheel.swift -o "$out/wheel"
cat > "$out/BUILD.txt" <<EOF
commit $(git rev-parse HEAD)
target $target, release, built in $(( $(date +%s) - start )) s at nice 10
trace OKILUM_FRAME_TRACE=<file> writes [frame] and [mac] lines; unset = no tracing
EOF
cat "$out/BUILD.txt"
