#!/usr/bin/env bash
# Real pinned managed-runtime source sharing; never a system service/user daemon.
set -euo pipefail
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
# A filter typo must fail, even though cargo considers zero matching tests green.
log="${RUNNER_TEMP:?}/sync-source-sharing.log"
cargo test --locked -p okilum-sync-controller --lib direct:: -- --nocapture
cargo test --locked -p okilum-sync-controller --test direct_bidirectional source_sharing:: -- --ignored --nocapture --test-threads=1 | tee "$log"
for name in managed_source_share_empty_receive_and_bidirectional_hashes managed_source_skipped_share_negative_control managed_source_nonempty_destination_refused_without_writes managed_peer_addresses_without_discovery_negative_then_hashes_and_restart; do
    grep -Fx "test source_sharing::$name ... ok" "$log"
done
grep -F 'test result: ok. 4 passed;' "$log"
