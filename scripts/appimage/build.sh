#!/usr/bin/env bash
# Unsigned exact-source QA artifact. No feed, signing or installation side effects.
set -euo pipefail
: "${GITHUB_SHA:?}"
test "$(git rev-parse HEAD)" = "$GITHUB_SHA"
test "$(. /etc/os-release; echo "$VERSION_ID")" = 22.04
export CC=/usr/bin/cc
export OKILUM_SOURCE_COMMIT="$GITHUB_SHA"
export OKILUM_BUILD_VERSION="$((5000 + GITHUB_RUN_NUMBER))"
export OKILUM_RELEASE_VERSION="0.1.$OKILUM_BUILD_VERSION"
scripts/vendor-setup.sh
scripts/vendor-setup.sh --verify
python3 scripts/brand-assets.py verify
cargo build --release --locked -p okilum-shell
mkdir -p dist/appimage/tools
cd dist/appimage
# URLs may move, but a changed tool/runtime fails closed against these byte pins.
fetch() {
    curl --fail --location --retry 2 "$2" --output "tools/$1"
    echo "$3  tools/$1" | sha256sum -c -
    chmod +x "tools/$1"
}
fetch linuxdeploy https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d
fetch appimagetool https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage 95cbe7cce9717fce90c484e34052ee7c7f1d7635b33c12525b4776826a7d29b6
fetch runtime https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64 617aa77314d57c8b8e4fae9c3f0b35af7b30d5529cb69e98f64f92510ed98a46
# Extraction mode avoids any FUSE requirement on the build runner.
export APPIMAGE_EXTRACT_AND_RUN=1
./tools/linuxdeploy --appdir AppDir --executable ../../target/release/okilum \
    --desktop-file ../../scripts/arch/okilum.desktop \
    --icon-file ../../crates/okilum-shell/assets/brand/app-icon-light.svg \
    --icon-filename okilum
install -Dm644 ../../LICENSE AppDir/usr/share/licenses/okilum/LICENSE
install -Dm644 ../../THIRD_PARTY_NOTICES.md AppDir/usr/share/licenses/okilum/THIRD_PARTY_NOTICES.md
printf '%s\n' "$GITHUB_SHA" > AppDir/SOURCE_SHA
# Keep the dependency inventory beside the immutable source marker.
find AppDir/usr/lib -type f -printf '%f\n' | sort > bundled-libraries.txt
ARCH=x86_64 ./tools/appimagetool --runtime-file tools/runtime AppDir Okilum-x86_64.AppImage
sha256sum Okilum-x86_64.AppImage > SHA256SUMS
python3 ../../scripts/appimage/provenance.py
