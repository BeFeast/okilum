#!/usr/bin/env bash
# Build a measurement-only Linux okilum with the frame trace (#1290): the CI guard
# (scroll/budget.py) and Linux runs of the scroll probe.
#
#   scripts/perf/build-linux-trace.sh OUTDIR
#
# Release build against a throwaway copy of gpui-pre that scripts/perf/frame-trace/patch.py
# instruments. OUTDIR gets okilum and BUILD.txt. Nothing is published; the checkout's sources and
# Cargo.lock are left as they were. On CT141 run it through the build wrapper in ~/bin (one cargo at a
# time; the VM 151 measurement windows hold that lock).
set -euo pipefail
cd "$(dirname "$0")/../.."
[ $# -eq 1 ] || { sed -n '2,10p' "$0"; exit 2; }
out=$(realpath -m "$1")
scripts/vendor-setup.sh --verify
mkdir -p "$out"
work="$out/.work"
rm -rf "$work"
mkdir -p "$work"
gpui=$(cargo metadata --locked --format-version 1 | python3 -c '
import json, sys
from pathlib import Path
packages = json.load(sys.stdin)["packages"]
print(Path(next(p for p in packages if p["name"] == "gpui-pre")["manifest_path"]).parent)
')
cp -R "$gpui" "$work/gpui-pre"
python3 scripts/perf/frame-trace/patch.py "$work/gpui-pre"

# The patch changes the dependency graph, so --locked cannot hold; the lockfile is put back.
cp Cargo.lock "$work/Cargo.lock.orig"
trap 'cp "$work/Cargo.lock.orig" Cargo.lock' EXIT
start=$(date +%s)
cargo build --release -p okilum-shell --bin okilum \
    --config "patch.crates-io.gpui-pre.path='$work/gpui-pre'"
cp "${CARGO_TARGET_DIR:-target}/release/okilum" "$out/okilum"
cat > "$out/BUILD.txt" <<EOF
commit $(git rev-parse HEAD)
release, built in $(( $(date +%s) - start )) s
trace OKILUM_FRAME_TRACE=<file> writes [frame] lines; unset = no tracing
EOF
cat "$out/BUILD.txt"
