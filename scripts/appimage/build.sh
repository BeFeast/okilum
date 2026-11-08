#!/usr/bin/env bash
# Unsigned exact-source QA artifact. No feed, signing or installation side effects.
#
# OKILUM_LOCAL_DEV=1 is for scripts/dev/okilum-dev-linux.sh only: a dev AppImage
# for the machine that builds it. It takes OKILUM_BUILD_VERSION from the caller,
# skips the Ubuntu 22.04 base check, linuxdeploy stripping (its bundled strip
# rejects newer distribution libraries) and the CI provenance record, and may use
# OKILUM_CARGO_PROFILE. The result is never a release or beta artifact.
set -euo pipefail
local_dev=${OKILUM_LOCAL_DEV:-0}
if [[ $local_dev == 1 ]]; then
    : "${OKILUM_BUILD_VERSION:?OKILUM_LOCAL_DEV=1 needs OKILUM_BUILD_VERSION}"
    [[ $OKILUM_BUILD_VERSION =~ ^[1-9][0-9]*$ ]] || { echo 'OKILUM_BUILD_VERSION must be a number' >&2; exit 2; }
    # SOURCE_SHA must describe the bytes: committed, unmodified tracked files only.
    git diff --quiet HEAD -- || { echo 'Local dev build needs a clean checkout' >&2; exit 1; }
    source_sha=$(git rev-parse HEAD)
    cargo_profile=${OKILUM_CARGO_PROFILE:-release}
    # Production identity: never the PR acceptance harness, whatever the shell has.
    unset GITHUB_REF_NAME
    export NO_STRIP=1
    # Link like the CI job (forgejo-linux.yml): check-elf.py needs the
    # DT_RELACOUNT that GNU bfd writes and mold does not. The last -fuse-ld wins.
    export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C link-arg=-fuse-ld=bfd"
    rm -rf dist/appimage/AppDir dist/appimage/Okilum-x86_64.AppImage
else
    : "${GITHUB_SHA:?}"
    test "$(git rev-parse HEAD)" = "$GITHUB_SHA"
    test "$(. /etc/os-release; echo "$VERSION_ID")" = 22.04
    source_sha=$GITHUB_SHA
    cargo_profile=release
    export OKILUM_BUILD_VERSION="$((5000 + GITHUB_RUN_NUMBER))"
fi
export CC=/usr/bin/cc
export OKILUM_SOURCE_COMMIT="$source_sha"
export OKILUM_RELEASE_VERSION="0.1.$OKILUM_BUILD_VERSION"
scripts/vendor-setup.sh
scripts/vendor-setup.sh --verify
python3 scripts/brand-assets.py verify
# Only PR artifacts expose an explicit local-fixture QA entry. Main/release
# artifacts cannot accept fixture trust, even with the QA CLI arguments.
# forgejo-linux-pr/hourly-<sha> is the write-once beta producer ref: production.
build_features=()
if [[ "${GITHUB_REF_NAME:-}" == forgejo-linux-pr/* && "${GITHUB_REF_NAME}" != forgejo-linux-pr/hourly-* ]]; then
    build_features=(--features appimage-acceptance-harness)
fi
cargo build --profile "$cargo_profile" --locked -p okilum-shell -p okilum-sync-controller --bin okilum --bin okilum-sync-launch "${build_features[@]}"
# Cargo names the dev profile's directory "debug"; others use the profile name.
profile_dir=$cargo_profile
[[ $cargo_profile != dev ]] || profile_dir=debug
binary="$(realpath -m "${CARGO_TARGET_DIR:-target}")/$profile_dir/okilum"
launch_binary="$(realpath -m "${CARGO_TARGET_DIR:-target}")/$profile_dir/okilum-sync-launch"
mkdir -p dist/appimage/evidence
python3 scripts/appimage/check-elf.py "$binary" \
    2>&1 | tee dist/appimage/evidence/elf-before-deploy.log
"$binary" --help
mkdir -p dist/appimage/tools
cd dist/appimage
# URLs may move, but a changed tool/runtime fails closed against these byte pins.
# A pinned copy from an earlier local build is reused; CI always starts empty.
fetch() {
    if ! echo "$3  tools/$1" | sha256sum -c --status - 2>/dev/null; then
        curl --fail --location --retry 2 "$2" --output "tools/$1"
    fi
    echo "$3  tools/$1" | sha256sum -c -
    chmod +x "tools/$1"
}
fetch linuxdeploy https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d
# Upstream release 1.9.1 publishes this SHA256 in its release asset digest:
# https://github.com/AppImage/appimagetool/releases/expanded_assets/1.9.1
fetch appimagetool https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
fetch runtime https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64 617aa77314d57c8b8e4fae9c3f0b35af7b30d5529cb69e98f64f92510ed98a46
# Prove the pinned runtime needs no host libfuse (or any other shared library).
# Positive control: the same inspection must detect the system shell's loader.
readelf -lW /bin/sh > evidence/runtime-positive-control.log
grep -q INTERP evidence/runtime-positive-control.log
{ readelf -lW tools/runtime; readelf -dW tools/runtime; } > evidence/runtime-elf.log
cat evidence/runtime-elf.log
if grep -Eq 'INTERP|\(NEEDED\)' evidence/runtime-elf.log; then
    echo 'Pinned AppImage runtime unexpectedly requires dynamic loading' >&2
    exit 1
fi
echo 'APPIMAGE_STATIC_RUNTIME_PASS (dynamic shell positive control passed)'
# Extraction mode avoids any FUSE requirement on the build runner.
export APPIMAGE_EXTRACT_AND_RUN=1
./tools/linuxdeploy --appdir AppDir --executable "$binary" \
    --executable "$launch_binary" \
    --executable /usr/bin/gpgv \
    --library "$(pkg-config --variable=libdir alsa)/libasound.so.2" \
    --desktop-file ../../scripts/arch/okilum.desktop \
    --icon-file ../../crates/okilum-shell/assets/brand/app-icon-light.svg \
    --icon-filename okilum
# linuxdeploy excludes ALSA by default. The audio viewer links it at startup,
# so force the loader library into the image; device configuration stays native.
test -e AppDir/usr/lib/libasound.so.2
# A newer host's PipeWire/Pulse plugin may require newer ALSA symbols and a
# different plugin directory. Prefer its matching host loader when available.
rm -f AppDir/AppRun
install -m755 ../../scripts/appimage/apprun.sh AppDir/AppRun
if [[ $local_dev != 1 ]]; then
    install -Dm644 /usr/share/doc/libasound2/copyright AppDir/usr/share/licenses/alsa-lib/copyright
fi
python3 ../../scripts/appimage/check-elf.py AppDir/usr/bin/okilum \
    2>&1 | tee evidence/elf-after-deploy.log
install -Dm644 ../../LICENSE AppDir/usr/share/licenses/okilum/LICENSE
install -Dm644 ../../THIRD_PARTY_NOTICES.md AppDir/usr/share/licenses/okilum/THIRD_PARTY_NOTICES.md
printf '%s\n' "$source_sha" > AppDir/SOURCE_SHA
# Stage after linuxdeploy stripping so the checksum-pinned Go ELF stays exact.
# An explicit local archive supports offline fixture/build inputs with the same pins.
syncthing_bundle_args=(--appdir AppDir)
if [[ -n "${OKILUM_SYNCTHING_ARCHIVE:-}" ]]; then
    syncthing_bundle_args+=(--archive "$OKILUM_SYNCTHING_ARCHIVE")
fi
python3 ../../scripts/syncthing/linux_bundle.py "${syncthing_bundle_args[@]}"
# Keep the dependency inventory beside the immutable source marker.
find AppDir/usr/lib -type f -printf '%f\n' | sort > bundled-libraries.txt
ARCH=x86_64 ./tools/appimagetool --runtime-file tools/runtime AppDir Okilum-x86_64.AppImage
sha256sum Okilum-x86_64.AppImage > SHA256SUMS
# The provenance record describes a CI run; a local dev build has none.
[[ $local_dev == 1 ]] || python3 ../../scripts/appimage/provenance.py
