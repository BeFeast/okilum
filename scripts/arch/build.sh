#!/usr/bin/env bash
# Packaging consumes verified production bytes. There is no compiler fallback.
set -euo pipefail
: "${OKILUM_PREBUILT_DIR:?Resolve required CI production payload first}"
export OKILUM_SOURCE="$PWD"
OKILUM_SOURCE_COMMIT=$(git rev-parse HEAD)
export OKILUM_SOURCE_COMMIT
OKILUM_PREBUILT_DIR=$(realpath "$OKILUM_PREBUILT_DIR")
export OKILUM_PREBUILT_DIR
[[ ${OKILUM_SETTINGS_UI_HARNESS:-0} == 0 ]] || { echo 'Production packaging rejects harness payloads' >&2; exit 1; }
# Explicit workflow routing only; an untrusted receipt cannot select this mode.
# The default trusted consumer and immutable signed-package reuse stay unchanged.
if [[ ${OKILUM_FORK_VALIDATION:-0} == 1 ]]; then
  exec python3 scripts/arch/fork-production.py package --prebuilt "$OKILUM_PREBUILT_DIR"
fi
export OKILUM_BUILD_VERSION
OKILUM_BUILD_VERSION=$(python3 - <<'PY'
import importlib.util, json, os
from pathlib import Path
spec = importlib.util.spec_from_file_location('payload', 'scripts/ci/package-linux-binary.py')
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)
root = Path(os.environ['OKILUM_PREBUILT_DIR'])
r = json.loads((root / 'receipt.json').read_text())
m = p.validate_receipt(r, os.environ['OKILUM_SOURCE_COMMIT'], p.config())
binary = root / 'payload/okilum'
p.require(binary.is_file() and not binary.is_symlink() and p.digest(binary.read_bytes()) == m['binary_sha256'], 'Prebuilt binary changed')
actual = p.inspect(binary, m)
p.require(all(actual[k] == m['elf'][k] for k in actual if k != 'runtime_libraries'), 'Prebuilt ELF/build identity changed')
print(m['build'])
PY
)
export OKILUM_RELEASE_VERSION="0.1.$OKILUM_BUILD_VERSION"
mkdir -p dist/arch
# Repackaging the same executable can change makepkg timestamps. An existing
# archive owns these package bytes, even if feed/catalog publication was interrupted.
archive_status=$(python3 scripts/arch/reuse-package.py --source "$OKILUM_SOURCE_COMMIT" \
  --prebuilt "$OKILUM_PREBUILT_DIR" --output dist/arch)
if [[ $archive_status == reused ]]; then
  exit 0
fi
cp scripts/arch/PKGBUILD dist/arch/PKGBUILD
cp scripts/arch/okilum.install dist/arch/okilum.install
cp "$OKILUM_PREBUILT_DIR/receipt.json" dist/arch/production-receipt.json
sed -i "s/^pkgver=.*/pkgver=0.1.$OKILUM_BUILD_VERSION/" dist/arch/PKGBUILD
(cd dist/arch && makepkg --cleanbuild --noconfirm)
# Detect stripping or any other makepkg transformation before upload/publication.
python3 - <<'PY'
import glob, importlib.util, os
from pathlib import Path
spec = importlib.util.spec_from_file_location('payload', 'scripts/ci/package-linux-binary.py')
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)
packages = glob.glob('dist/arch/okilum-*.pkg.tar.zst')
p.require(len(packages) == 1, 'Missing/ambiguous Arch package')
p.package_receipt(Path(packages[0]), os.environ['OKILUM_SOURCE_COMMIT'], int(os.environ['OKILUM_BUILD_VERSION']))
PY
