#!/usr/bin/env bash
# Real pinned managed-runtime source sharing; never a system service/user daemon.
set -euo pipefail
python3 -m unittest discover -s scripts/ci -p 'test_sync_fixture_*.py'
export TMPDIR="${TMPDIR:-${RUNNER_TEMP:?}}"
OKILUM_SYNC_DIRECT_CLIENT=$(python3 scripts/ci/sync-fixture-stage.py)
export OKILUM_SYNC_DIRECT_CLIENT
# Compile once; concurrent trials execute independent fixture processes rather
# than serializing on Cargo's target lock. Every trial checks every expected name.
cargo test --locked -p okilum-sync-controller --lib direct:: -- --nocapture
artifacts=$(mktemp)
trap 'rm -f "$artifacts"' EXIT
cargo test --locked -p okilum-sync-controller --test direct_bidirectional --no-run --message-format=json > "$artifacts"
executable=$(python3 - "$artifacts" <<'PYTHON'
import json, sys
matches = [v['executable'] for line in open(sys.argv[1]) if (v := json.loads(line)).get('reason') == 'compiler-artifact' and v.get('executable') and v['target']['name'] == 'direct_bidirectional']
assert len(matches) == 1, matches
print(matches[0])
PYTHON
)
python3 scripts/ci/sync-fixture-lifecycle.py "$executable"
python3 scripts/ci/sync-source-sharing-runs.py "$executable" "$@"
