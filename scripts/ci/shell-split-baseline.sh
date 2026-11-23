#!/usr/bin/env bash
# Baseline of the okilum-shell crate before and during its split (#3232, epic
# #3241). A heavy run: it compiles the okilum-shell test harness. Run it in the
# build queue with the shared compile cache off, so every rustc is a child of
# cargo and its peak RSS is counted:
#
#   mkdir -p "$qa/out"   # build-slot ships back only directories that exist
#   okilum-slot run --label shell-split-baseline-<sha8> --mem 9 \
#     --env BUILD_SLOT_SCCACHE=off --require-host okilum-build-nas \
#     --source-git "$PWD" --receipt "$qa/receipt.json" \
#     -- bash scripts/ci/shell-split-baseline.sh "$qa"
#
# Writes into OUT (default target/shell-split-baseline):
#   tests.txt     every test name of the okilum-shell lib and `okilum` bin
#                 harnesses (sorted, duplicates kept); Linux default features
#   actions.txt   every registered action name in the `okilum` test harness
#   summary.txt   peak RSS / wall time per measured command, toolchain, host
#   timings-*.html  cargo --timings reports; units-*.txt the okilum-shell units
# The split PRs refresh crates/okilum-shell/tests/fixtures/{tests,actions}.txt
# from tests.txt/actions.txt and compare before/after (no test lost).
set -euo pipefail
cd "$(dirname "$0")/../.."
OUT=$(realpath -m "${1:-target/shell-split-baseline}")
mkdir -p "$OUT"
export OKILUM_SOURCE_COMMIT="${OKILUM_SOURCE_COMMIT:-$(git rev-parse HEAD 2>/dev/null || echo unknown)}"
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
# shellcheck source=scripts/ci/test-nofile.sh
source scripts/ci/test-nofile.sh

summary="$OUT/summary.txt"
{
    echo "source=$OKILUM_SOURCE_COMMIT"
    echo "host=$(hostname) nproc=$(nproc) mem_total_kib=$(awk '/MemTotal/ {print $2}' /proc/meminfo)"
    echo "cgroup_memory_max=$(cat /sys/fs/cgroup/memory.max 2>/dev/null || echo n/a)"
    echo "rustc=$(rustc -V)"
    echo "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-default} RUSTC_WRAPPER=${RUSTC_WRAPPER:-none} RUSTFLAGS=${RUSTFLAGS:-none}"
    echo "CARGO_PROFILE_DEV_DEBUG=$CARGO_PROFILE_DEV_DEBUG CARGO_PROFILE_TEST_DEBUG=$CARGO_PROFILE_TEST_DEBUG CARGO_INCREMENTAL=$CARGO_INCREMENTAL"
} >"$summary"
if [ -n "${RUSTC_WRAPPER:-}" ]; then
    # A wrapper server spawns rustc outside cargo's process tree: the peak
    # below would then miss the compiler. Record it; do not pretend.
    echo "WARNING: RUSTC_WRAPPER is set; peak RSS excludes compilers it spawns" | tee -a "$summary"
fi

bash scripts/vendor-setup.sh
bash scripts/vendor-setup.sh --verify
cargo fetch --locked
export CARGO_NET_OFFLINE=true
target_dir=$(cargo metadata --format-version 1 --no-deps --locked |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')

# measure NAME CMD...: peak RSS of the largest process in CMD's tree
# and wall time, the same figure as `/usr/bin/time -f %M` (the build hosts do
# not all ship GNU time): RUSAGE_CHILDREN's ru_maxrss covers every reaped
# descendant, here each rustc cargo waits for. Keeps --timings.
peak_rss() {
    python3 -c '
import resource, subprocess, sys, time
start = time.monotonic()
rc = subprocess.call(sys.argv[2:])
peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
open(sys.argv[1], "w").write(f"{peak} {time.monotonic() - start:.1f}\n")
sys.exit(rc)
' "$@"
}
measure() {
    local name=$1 rc=0 before
    shift
    before=$(ls -t "$target_dir"/cargo-timings/cargo-timing-*.html 2>/dev/null | head -1 || true)
    echo "MEASURE_START=$name unix=$(date +%s)"
    peak_rss "$OUT/time-$name.txt" "$@" || rc=$?
    read -r kib wall <"$OUT/time-$name.txt" || { kib=0 wall=0; }
    printf '%s: status=%s peak_rss_kib=%s peak_rss_gib=%s wall_s=%s cmd=%s\n' "$name" "$rc" "$kib" \
        "$(awk -v k="$kib" 'BEGIN {printf "%.2f", k / 1048576}')" "$wall" "$*" | tee -a "$summary"
    local report
    report=$(ls -t "$target_dir"/cargo-timings/cargo-timing-*.html 2>/dev/null | head -1 || true)
    if [ -n "$report" ] && [ "$report" != "$before" ]; then
        cp "$report" "$OUT/timings-$name.html"
        python3 - "$report" >"$OUT/units-$name.txt" <<'EOF'
import json, re, sys
html = open(sys.argv[1], encoding="utf-8").read()
match = re.search(r"const UNIT_DATA = (\[.*?\]);\n", html, re.S)
units = json.loads(match.group(1)) if match else []
total = re.search(r"<td>Total time:</td><td>([^<]+)</td>", html)
print(f"total: {total.group(1) if total else 'n/a'} units={len(units)}")
for unit in sorted(units, key=lambda u: -u["duration"]):
    if unit["name"].startswith("okilum-shell") or unit["duration"] >= 30:
        print(f'{unit["duration"]:9.1f}s  {unit["name"]} {unit.get("version", "")} {unit.get("target", "")}'.rstrip())
EOF
        sed 's/^/  /' "$OUT/units-$name.txt" | head -12 | tee -a "$summary"
    fi
    return "$rc"
}

# Dependencies and dev-dependencies of the test profile, unmeasured, so the
# next command compiles only the `okilum` bin harness.
cargo test --locked -p okilum-shell --lib --no-run
cargo clean --locked -p okilum-shell
cargo test --locked -p okilum-shell --lib --no-run
measure test-harness cargo test --locked -p okilum-shell --bin okilum --no-run --timings
measure build cargo build --locked -p okilum-shell --timings

# Golden test list: every test (ignored ones too) of each harness.
cargo test --locked -p okilum-shell --lib --bins -- --list >"$OUT/list-raw.txt"
grep -E ': (test|benchmark)$' "$OUT/list-raw.txt" | sed -E 's/: (test|benchmark)$//' | LC_ALL=C sort >"$OUT/tests.txt"
echo "tests=$(wc -l <"$OUT/tests.txt") unique=$(LC_ALL=C sort -u "$OUT/tests.txt" | wc -l)" | tee -a "$summary"
test -s "$OUT/tests.txt"

# Golden action list from the linked registry; the control test runs first.
OKILUM_ACTION_NAMES_OUT="$OUT/actions.txt" cargo test --locked -p okilum-shell --bin okilum \
    action_inventory_tests:: -- --include-ignored --test-threads=1
echo "actions=$(wc -l <"$OUT/actions.txt")" | tee -a "$summary"
grep -qx 'okilum::NewNote' "$OUT/actions.txt"
cat "$summary"
