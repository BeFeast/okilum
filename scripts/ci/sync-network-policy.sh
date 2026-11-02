#!/usr/bin/env bash
# Compile normally; run real transport policy tests with loopback as the only NIC.
set -euo pipefail
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
binary="$cache/syncthing-linux-amd64-v2.1.6/syncthing"
artifacts=$(mktemp)
trap 'rm -f "$artifacts"' EXIT
cargo test --locked --offline -p okilum-sync-controller --test network_policy --no-run --message-format=json > "$artifacts"
executable=$(python3 - "$artifacts" <<'PY'
import json, sys
matches = [v['executable'] for line in open(sys.argv[1]) if (v := json.loads(line)).get('reason') == 'compiler-artifact' and v.get('executable') and v['target']['name'] == 'network_policy']
assert len(matches) == 1, matches
print(matches[0])
PY
)
log="${RUNNER_TEMP:?}/sync-network-policy.log"
# Only the disposable namespace needs privilege; the test and daemons run as the
# caller with their ordinary ownership. No host interfaces/firewall are changed.
sudo -n unshare --net bash -c 'ip link set lo up; exec runuser -u "$1" -- "${@:2}"' bash "$(id -un)" \
    env OKILUM_SYNC_OFFLINE_NAMESPACE=1 OKILUM_SYNC_DIRECT_CLIENT="$binary" TMPDIR="${TMPDIR:?}" \
    "$executable" --ignored --nocapture --test-threads=1 | tee "$log"
for name in managed_network_default_and_switch_round_trip_read_back managed_network_legacy_migration_happens_once_outside_index managed_network_pending_choice_recovers_without_public_reset managed_network_external_options_unchanged_negative_control managed_network_custom_relay_route_refuses_false_local_claim; do
    grep -Fx "test $name ... ok" "$log"
done
grep -F 'test result: ok. 5 passed;' "$log"
