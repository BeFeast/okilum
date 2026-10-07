#!/usr/bin/env bash
# Cross-compile a portable read-only diagnostic. Never publish to an update feed.
set -euo pipefail
cd "$(dirname "$0")/.."
output=${1:-target/windows-dist}
mkdir -p "$output"
output=$(realpath "$output")
export TESSERA_SOURCE_COMMIT
TESSERA_SOURCE_COMMIT=$(git rev-parse HEAD)
export TESSERA_BUILD_VERSION="${TESSERA_BUILD_VERSION:-${TESSERA_SOURCE_COMMIT:0:8}}"
export TESSERA_RELEASE_VERSION="${TESSERA_RELEASE_VERSION:-0.1.0-windows-diagnostic-${TESSERA_SOURCE_COMMIT:0:8}}"
export TESSERA_WINDOWS_ICON="$output/tessera.ico"
python3 scripts/windows-icon.py crates/tessera-shell/assets/brand/app-icon-light.svg "$TESSERA_WINDOWS_ICON"
scripts/vendor-setup.sh --verify
# GPUI's build script is Linux-hosted, so offline fxc shaders cannot be generated.
# This optimized profile selects its existing Windows runtime HLSL compiler.
# Disable defaults in both shell and core: Windows never compiles managed writes.
export RUSTFLAGS="-C target-feature=+crt-static"
source scripts/ci/release-cache.sh
export TESSERA_SCCACHE="${RUSTC_WRAPPER:-}"
export RUSTC_WRAPPER="$PWD/scripts/windows-rustc.py"
export RC_x86_64_pc_windows_msvc="$PWD/scripts/windows-rc.py"
cargo xwin build --locked --target x86_64-pc-windows-msvc \
    --profile windows-diagnostic -p tessera-shell --no-default-features
cp "${CARGO_TARGET_DIR:-target}/x86_64-pc-windows-msvc/windows-diagnostic/tessera.exe" "$output/tessera.exe"
python3 scripts/third-party-notices.py --stage "$output"
cp docs/windows-diagnostic.md "$output/README.md"
cargo metadata --locked --format-version 1 > "$output/metadata.json"
python3 - "$output" "$TESSERA_RELEASE_VERSION" <<'PY'
from pathlib import Path
import hashlib, sys, zipfile
output = Path(sys.argv[1])
import json, shutil
packages = json.loads((output / 'metadata.json').read_text())['packages']
package = next(p for p in packages if p['name'] == 'gpui-pre-windows')
assert package['version'] == '0.3.3', 'Recheck the runtime HLSL packaging on GPUI upgrades'
source = Path(package['manifest_path']).parent
shaders = output / 'gpui-shaders' / 'src'
shaders.mkdir(parents=True, exist_ok=True)
for name in ('shaders.hlsl', 'color_text_raster.hlsl', 'alpha_correction.hlsl'):
    shutil.copyfile(source / 'src' / name, shaders / name)
shutil.copyfile(source / 'LICENSE-APACHE', shaders.parent / 'LICENSE-APACHE')
archive = output / f'tessera-{sys.argv[2]}-x86_64.zip'
with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as bundle:
    for name in ('tessera.exe', 'README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.md'):
        bundle.write(output / name, name)
    for path in sorted((output / 'gpui-shaders').rglob('*')):
        if path.is_file():
            bundle.write(path, path.relative_to(output))
(output / (archive.name + '.sha256')).write_text(
    hashlib.sha256(archive.read_bytes()).hexdigest() + '  ' + archive.name + '\n')
print(archive)
PY

release_cache_stats
