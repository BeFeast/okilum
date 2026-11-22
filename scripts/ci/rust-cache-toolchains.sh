#!/usr/bin/env bash
# Leave only the rust-toolchain.toml toolchain installed before Swatinem/rust-cache.
#
# rust-cache hashes the version of every installed toolchain (`rustup toolchain
# list`) into its key. GitHub's ubuntu-22.04 images differ in their preinstalled
# stable Rust (19.11-20.11: image 20261103 ships 1.99.1, 20261111 ships 1.100.0),
# so one commit got a different key on each image, and beta-appimage-cache.yml's
# prune keeps only the newest entry: a beta producer that landed on the other
# image found no cache and compiled all 847 crates (12-20 min instead of ~6,
# #3043). With the pinned toolchain alone, the key depends on rust-toolchain.toml
# and the job env only, whatever image the runner has.
#
# Run from the repository root (rustup resolves the override file from the cwd).
set -euo pipefail

# No toolchain argument: install the one rust-toolchain.toml names, with its
# components and profile (rustup >= 1.28).
rustup toolchain install
pinned=$(rustup show active-toolchain)
pinned=${pinned%% *}
test -n "$pinned"

rustup toolchain list --quiet | while read -r toolchain _; do
  [ -n "$toolchain" ] && [ "$toolchain" != "$pinned" ] || continue
  echo "Removing preinstalled toolchain $toolchain (not pinned)"
  rustup toolchain uninstall "$toolchain"
done

left=$(rustup toolchain list --quiet | awk 'NF { print $1 }')
if [ "$left" != "$pinned" ]; then
  echo "Expected only $pinned installed, found: $left" >&2
  exit 1
fi
echo "Only the pinned toolchain is installed: $pinned ($(rustc -V))"
