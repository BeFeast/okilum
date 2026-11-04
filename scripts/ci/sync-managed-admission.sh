#!/usr/bin/env bash
# #2582 exact launch command + separate-process admission, disposable fixtures.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/test-nofile.sh"
run_group() {
    local evidence
    evidence=$(mktemp)
    "$@" | tee "$evidence"
    grep -Eq 'test result: ok\. [1-9][0-9]* passed;' "$evidence" || {
        echo "No non-zero managed admission verdict: $*" >&2; exit 1;
    }
    unlink "$evidence"
}
run_group cargo test --locked -p okilum-sync-controller --lib lifecycle::tests:: -- --nocapture
run_group cargo test --locked -p okilum-sync-controller --lib linux_bootstrap::tests:: -- --nocapture
run_group cargo test --locked -p okilum-sync-controller --lib runtime::tests:: -- --nocapture
cache="${XDG_CACHE_HOME:-$HOME/.cache}/okilum-sync-fixture"
archive=syncthing-linux-amd64-v2.1.6.tar.gz
# The source-sharing step fetches this pinned public fixture first.
(cd "$cache"; echo "524ef4e1df1850b719e2378c8369956f4e5f2ab9a46fdf7f45f2b925012147aa  $archive" | sha256sum --check)
export OKILUM_SYNC_DIRECT_CLIENT="$cache/syncthing-linux-amd64-v2.1.6/syncthing"
artifacts=$(mktemp)
trap 'unlink "$artifacts"' EXIT
cargo test --locked -p okilum-sync-controller --test managed_launch --no-run --message-format=json > "$artifacts"
executable=$(python3 - "$artifacts" <<'PYTHON'
import json,sys
matches=[v['executable'] for line in open(sys.argv[1]) if (v:=json.loads(line)).get('reason')=='compiler-artifact' and v.get('executable') and v['target']['name']=='managed_launch']
assert len(matches)==1,matches
print(matches[0])
PYTHON
)
python3 scripts/ci/sync-managed-admission-runs.py "$executable"
python3 scripts/ci/sync-managed-admission-negative-control.py
