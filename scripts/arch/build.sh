#!/usr/bin/env bash
set -euo pipefail
: "${TESSERA_BUILD_VERSION:?}"
[[ $TESSERA_BUILD_VERSION =~ ^[1-9][0-9]*$ ]]
export TESSERA_SOURCE="$PWD"
export TESSERA_RELEASE_VERSION="0.1.$TESSERA_BUILD_VERSION"
export TESSERA_SOURCE_COMMIT="$(git rev-parse HEAD)"
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
export RUSTFLAGS='-C link-arg=-fuse-ld=mold'
if ! command -v rustup >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
fi
rustc --version
source scripts/ci/release-cache.sh
mkdir -p dist/arch
cp scripts/arch/PKGBUILD dist/arch/PKGBUILD
sed -i "s/^pkgver=.*/pkgver=0.1.$TESSERA_BUILD_VERSION/" dist/arch/PKGBUILD
cd dist/arch
makepkg --cleanbuild --noconfirm

release_cache_stats
