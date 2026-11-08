#!/usr/bin/env bash
# Rebuild the static upstream runtime with the per-user extraction namespace.
# The source and dependency archives are checksum-pinned; no app signing occurs.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
destination=$(realpath -m "${1:?runtime destination required}")
controls=${2:-}
test -z "$controls" || test "$controls" = --controls
revision=fedfad51acc2860b2916a1eef32ba4a500ff31ef
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
python3 "$root/scripts/pinned_download.py" \
    --url "https://codeload.github.com/AppImage/type2-runtime/tar.gz/$revision" \
    --sha256 e27ea3c837e9c660e1a9a2993b673ae93bbb53077330efba8654031e302d4e8e \
    --destination "$scratch/source.tar.gz"
tar -xzf "$scratch/source.tar.gz" -C "$scratch"
source_dir="$scratch/type2-runtime-$revision"
image="okilum-appimage-runtime:${revision:0:12}"
docker build --tag "$image" --file "$source_dir/scripts/docker/Dockerfile" "$source_dir"
build() {
    docker run --rm --user "$(id -u):$(id -g)" \
        --volume "$source_dir:/ws" --workdir /ws "$image" \
        bash scripts/build-runtime.sh
}
if [[ $controls == --controls ]]; then
    build
    cp "$source_dir/out/runtime-x86_64" "$scratch/baseline-runtime"
    # A cleanup-only mutation keeps upstream's shared extraction name. The
    # overlap control must fail here before testing private launch lifetimes.
    sed -i 's/nftw(path, \&rm_recursive_callback, 0,/nftw(path, \&rm_recursive_callback, 16,/' "$source_dir/src/runtime/runtime.c"
    build
    cp "$source_dir/out/runtime-x86_64" "$scratch/shared-runtime"
    sed -i 's/nftw(path, \&rm_recursive_callback, 16,/nftw(path, \&rm_recursive_callback, 0,/' "$source_dir/src/runtime/runtime.c"
fi
git -C "$source_dir" apply "$root/scripts/appimage/runtime-per-user.diff"
printf '%s\n' "$revision-okilum-per-user" > "$source_dir/src/runtime/version"
build
mkdir -p "$(dirname "$destination")"
install -m755 "$source_dir/out/runtime-x86_64" "$destination"
if [[ $controls == --controls ]]; then
    sudo python3 "$root/scripts/appimage/runtime-controls.py" \
        --runtime "$destination" --baseline "$scratch/baseline-runtime" \
        --shared-runtime "$scratch/shared-runtime"
fi
