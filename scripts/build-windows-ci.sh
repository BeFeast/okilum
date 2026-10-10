#!/usr/bin/env bash
# Cross-compile a portable read-only diagnostic. Never publish to an update feed.
set -euo pipefail
cd "$(dirname "$0")/.."
output=${1:-target/windows-dist}
mkdir -p "$output"
output=$(realpath "$output")
export OKILUM_SOURCE_COMMIT
OKILUM_SOURCE_COMMIT=$(git rev-parse HEAD)
export OKILUM_BUILD_VERSION="${OKILUM_BUILD_VERSION:-${OKILUM_SOURCE_COMMIT:0:8}}"
export OKILUM_RELEASE_VERSION="${OKILUM_RELEASE_VERSION:-0.1.0-windows-diagnostic-${OKILUM_SOURCE_COMMIT:0:8}}"
export OKILUM_WINDOWS_ICON="$output/okilum.ico"
python3 scripts/windows-icon.py crates/okilum-shell/assets/brand/app-icon-light.svg "$OKILUM_WINDOWS_ICON"
scripts/vendor-setup.sh --verify
# GPUI's build script is Linux-hosted, so offline fxc shaders cannot be generated.
# This optimized profile selects its existing Windows runtime HLSL compiler.
# Disable defaults in both shell and core: Windows never compiles managed writes.
export RUSTFLAGS="-C target-feature=+crt-static"
source scripts/ci/release-cache.sh
export OKILUM_SCCACHE="${RUSTC_WRAPPER:-}"
export RUSTC_WRAPPER="$PWD/scripts/windows-rustc.py"
export RC_x86_64_pc_windows_msvc="$PWD/scripts/windows-rc.py"
cargo xwin build --locked --target x86_64-pc-windows-msvc \
    --profile windows-diagnostic -p okilum-shell --no-default-features
cp "${CARGO_TARGET_DIR:-target}/x86_64-pc-windows-msvc/windows-diagnostic/okilum.exe" "$output/okilum.exe"
# The sync supervisor (#1013) ships in the package; the Enable flow stages it outside
# `current`. Same profile and flags as the shell, so shared dependencies are reused.
cargo xwin build --locked --target x86_64-pc-windows-msvc \
    --profile windows-diagnostic -p okilum-sync-supervisor
cp "${CARGO_TARGET_DIR:-target}/x86_64-pc-windows-msvc/windows-diagnostic/okilum-sync-supervisor.exe" "$output/okilum-sync-supervisor.exe"
python3 scripts/windows/pe.py gui "$output/okilum-sync-supervisor.exe"
python3 scripts/third-party-notices.py --stage "$output"
cp docs/windows-diagnostic.md "$output/README.md"
cargo metadata --locked --format-version 1 > "$output/metadata.json"
python3 - "$output" <<'PY'
from pathlib import Path
import json, shutil, sys
output = Path(sys.argv[1])
packages = json.loads((output / 'metadata.json').read_text())['packages']
package = next(p for p in packages if p['name'] == 'gpui-pre-windows')
assert package['version'] == '0.3.3', 'Recheck the runtime HLSL packaging on GPUI upgrades'
source = Path(package['manifest_path']).parent
shaders = output / 'gpui-shaders' / 'src'
shaders.mkdir(parents=True, exist_ok=True)
for name in ('shaders.hlsl', 'color_text_raster.hlsl', 'alpha_correction.hlsl'):
    shutil.copyfile(source / 'src' / name, shaders / name)
shutil.copyfile(source / 'LICENSE-APACHE', shaders.parent / 'LICENSE-APACHE')
PY
# Built again after signing on the signer runner (#1104), so it is one script.
python3 scripts/windows/portable-zip.py "$output" "$OKILUM_RELEASE_VERSION"

release_cache_stats
