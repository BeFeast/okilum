#!/usr/bin/env bash
# Download pinned, unchanged Linux test binaries; never install/start system services.
set -euo pipefail
cd "$(dirname "$0")/.."
cache="${XDG_CACHE_HOME:-$HOME/.cache}/tessera-sync-fixture"
mkdir -p "$cache"
for spec in '1.29.5 b05cb12f6f58309612194cf1126b5ac090525e2c40e6bd64cc59b347dccbe441' '2.1.6 524ef4e1df1850b719e2378c8369956f4e5f2ab9a46fdf7f45f2b925012147aa'; do
    read -r version checksum <<< "$spec"
    archive="syncthing-linux-amd64-v$version.tar.gz"
    if [[ ! -f "$cache/$archive" ]]; then
        curl --fail --location --retry 2 "https://github.com/syncthing/syncthing/releases/download/v$version/$archive" -o "$cache/$archive.part"
        mv "$cache/$archive.part" "$cache/$archive"
    fi
    (cd "$cache"; echo "$checksum  $archive" | sha256sum --check)
    tar -xzf "$cache/$archive" -C "$cache"
done
export TESSERA_SYNC_HUB="$cache/syncthing-linux-amd64-v1.29.5/syncthing"
export TESSERA_SYNC_CLIENT="$cache/syncthing-linux-amd64-v2.1.6/syncthing"
cargo test -p tessera-sync --test compatibility -- --ignored --nocapture
