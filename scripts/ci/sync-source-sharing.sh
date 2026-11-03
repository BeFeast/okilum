#!/usr/bin/env bash
# Real pinned managed-runtime source sharing; never a system service/user daemon.
set -euo pipefail
python3 -m unittest discover -s scripts/ci -p test_sync_fixture_processes.py
export TMPDIR="${TMPDIR:-${RUNNER_TEMP:?}}"
cache="${XDG_CACHE_HOME:-$HOME/.cache}/okilum-sync-fixture"
mkdir -p "$cache"
archive=syncthing-linux-amd64-v2.1.6.tar.gz
if [[ ! -f "$cache/$archive" ]]; then
    curl --fail --location --retry 2 "https://github.com/syncthing/syncthing/releases/download/v2.1.6/$archive" -o "$cache/$archive.part"
    mv "$cache/$archive.part" "$cache/$archive"
fi
(cd "$cache"; echo "524ef4e1df1850b719e2378c8369956f4e5f2ab9a46fdf7f45f2b925012147aa  $archive" | sha256sum --check)
tar -xzf "$cache/$archive" -C "$cache"
export OKILUM_SYNC_DIRECT_CLIENT="$cache/syncthing-linux-amd64-v2.1.6/syncthing"
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
