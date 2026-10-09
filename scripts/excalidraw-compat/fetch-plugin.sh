#!/usr/bin/env bash
# Test-only oracle for #478: AGPL code is fetched at test time and not distributed.
# Downloads zsviczian/obsidian-excalidraw-plugin (AGPL-3.0) at a pinned commit,
# verifies its sha256 and unpacks it OUTSIDE the repository. Never commit,
# package or upload the unpacked sources.
# Usage: fetch-plugin.sh DEST_DIR
set -euo pipefail
COMMIT=f30b4c5d3dcb66ac76ced8f05d9e95409ee94c79
SHA256=6d4eb0337d17284514898983609eeb742f2436d6c6cc1fd59ebee5da869a814b
dest=${1:?destination directory}
mkdir -p "$dest"
archive="$dest/plugin-$SHA256.tar.gz"
if [ ! -f "$archive" ] || ! echo "$SHA256  $archive" | sha256sum -c --quiet -; then
  curl -sSfL --retry 3 -o "$archive.part" \
    "https://codeload.github.com/zsviczian/obsidian-excalidraw-plugin/tar.gz/$COMMIT"
  mv "$archive.part" "$archive"
fi
echo "$SHA256  $archive" | sha256sum -c --quiet -
rm -rf "$dest/plugin"
mkdir "$dest/plugin"
tar -xzf "$archive" -C "$dest/plugin" --strip-components=1
echo "plugin $COMMIT verified at $dest/plugin"
