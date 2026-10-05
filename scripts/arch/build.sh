#!/usr/bin/env bash
set -euo pipefail
: "${TESSERA_BUILD_VERSION:?}"
[[ $TESSERA_BUILD_VERSION =~ ^[1-9][0-9]*$ ]]
export TESSERA_SOURCE="$PWD"
export TESSERA_RELEASE_VERSION="0.1.$TESSERA_BUILD_VERSION"
export TESSERA_SOURCE_COMMIT="$(git rev-parse HEAD)"
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2
export RUSTFLAGS='-C link-arg=-fuse-ld=mold'
if ! command -v rustup >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.96.1
fi
rustup toolchain install 1.96.1 --profile minimal
mkdir -p dist/arch
cp scripts/arch/PKGBUILD dist/arch/PKGBUILD
sed -i "s/^pkgver=.*/pkgver=0.1.$TESSERA_BUILD_VERSION/" dist/arch/PKGBUILD
cd dist/arch
makepkg --cleanbuild --noconfirm
