#!/usr/bin/env bash
set -euo pipefail
qa=${1:?evidence directory}
out=${2:?results directory}
mkdir -p "$out"
source scripts/ci/test-nofile.sh
scripts/vendor-setup.sh
scripts/vendor-setup.sh --verify
python3 scripts/ci/check-vendor-generation.py
file=crates/okilum-shell/src/reader_cache.rs
cp "$file" "$out/fixed-reader_cache.rs"
trap 'cp "$out/fixed-reader_cache.rs" "$file"' EXIT
build_binary() {
    cargo test --offline --locked -p okilum-shell --bin okilum --no-run --message-format=json > "$out/build-$1.jsonl"
    python3 - "$out/build-$1.jsonl" <<'PY'
import json,sys
for line in open(sys.argv[1]):
    x=json.loads(line)
    if x.get('reason')=='compiler-artifact' and x.get('executable') and x['target']['name']=='okilum' and x['profile']['test']:
        print(x['executable'])
PY
}
cp "$qa/baseline-reader_cache.rs" "$file"
binary=$(build_binary old)
python3 "$qa/stress.py" "$binary" "$out" old || old_rc=$?
old_rc=${old_rc:-0}
if [[ "$old_rc" -ne 0 && "$old_rc" -ne 1 ]]; then exit "$old_rc"; fi
cp "$out/fixed-reader_cache.rs" "$file"
binary=$(build_binary fixed)
python3 "$qa/stress.py" "$binary" "$out" fixed
"$binary" --test-threads=4 > "$out/shell-full.log" 2>&1 || { tail -100 "$out/shell-full.log"; exit 1; }
tail -3 "$out/shell-full.log"
