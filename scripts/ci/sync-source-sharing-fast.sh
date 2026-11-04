#!/usr/bin/env bash
# Dedicated warm sync target; all daemon groups close the outer exclusion lock.
set -euo pipefail
export CARGO_TARGET_DIR="$HOME/.cache/okilum-sync-source-sharing/target"
mkdir -p "$CARGO_TARGET_DIR"
exec 9>"$HOME/.cache/okilum-sync-source-sharing/gate.lock"
flock -w 600 9
bash scripts/ci/test-group.sh sync-vendor-setup 120 scripts/vendor-setup.sh 9>&-
bash scripts/ci/test-group.sh sync-vendor-verify 60 scripts/vendor-setup.sh --verify 9>&-
bash scripts/ci/test-group.sh sync-source-sharing 480 bash scripts/ci/sync-source-sharing.sh --runs 2 --parallel 2 9>&-
bash scripts/ci/test-group.sh sync-network-policy 180 bash scripts/ci/sync-network-policy.sh 9>&-
