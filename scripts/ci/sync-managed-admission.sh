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
run_group cargo test --locked -p okilum-sync-controller --lib runtime::tests::unopened_and_disabled_runtime_has_no_effects -- --exact --nocapture
# Reuse the same archive AND executable checksum verifier/atomic publisher as
# source sharing; the old version-named executable is no longer populated.
OKILUM_SYNC_DIRECT_CLIENT=$(python3 scripts/ci/sync-fixture-stage.py)
export OKILUM_SYNC_DIRECT_CLIENT
artifacts=$(mktemp)
trap 'unlink "$artifacts"' EXIT
if ! cargo test --locked -p okilum-sync-controller --features first-sync-fixture --test managed_launch --no-run --message-format=json > "$artifacts"; then
    python3 - "$artifacts" <<'DIAGNOSTICS'
import json,sys
for line in open(sys.argv[1]):
    value=json.loads(line)
    if value.get("reason")=="compiler-message":
        print(value["message"].get("rendered",value["message"]["message"]))
DIAGNOSTICS
    exit 1
fi
executable=$(python3 - "$artifacts" <<'PYTHON'
import json,sys
matches=[v['executable'] for line in open(sys.argv[1]) if (v:=json.loads(line)).get('reason')=='compiler-artifact' and v.get('executable') and v['target']['name']=='managed_launch']
assert len(matches)==1,matches
print(matches[0])
PYTHON
)
python3 scripts/ci/sync-managed-admission-runs.py "$executable"
python3 scripts/ci/sync-first-merge-negative-control.py
python3 scripts/ci/sync-managed-admission-negative-control.py
