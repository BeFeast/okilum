#!/usr/bin/env bash
# Native Linux external-drop regression controls; run under Xvfb on CI.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
python3 scripts/gpui-pre-linux-setup.py --verify
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cp -R vendor/gpui-pre-linux-0.3.3 "$work/crate"
cp Cargo.lock "$work/crate/Cargo.lock"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
cargo test --manifest-path "$work/crate/Cargo.toml" \
  --config "patch.crates-io.gpui-pre.path='$ROOT/vendor/gpui-pre-0.3.3'" \
  --features test-support --lib external_drop -- --test-threads=1 2>&1 | tee "$work/tests.log"
for test in external_drop_aggregate_budget_counts_failed_reads external_drop_selection_and_bounds external_drop_pipe_rejects_oversize_and_timeout external_drop_uri_lists_keep_file_and_web_routes external_drop_x11_real_selection_and_incr; do
  grep -q "$test ... ok" "$work/tests.log"
done
cargo test --locked -p okilum-shell --bin okilum linux_dropped_mime_data_routes_through_the_paste_router 2>&1 | tee "$work/router.log"
grep -q 'linux_dropped_mime_data_routes_through_the_paste_router ... ok' "$work/router.log"
python3 scripts/gpui-pre-linux-setup.py --verify
