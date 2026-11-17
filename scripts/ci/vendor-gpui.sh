# Sourced by scripts that build vendor/gpui-component's own tests (#653).
# Okilum ships patched gpui-pre crates (Cargo.toml [patch.crates-io], #2803)
# whose API the vendor patch stack uses, while the vendor lock pins the
# crates.io family one release older, with exact cross-requirements. For this
# run: patch the same crates as Cargo.toml, give the vendor lock our family
# (scripts/ci/vendor_gpui_lock.py), check the patched gpui-pre is selected,
# and restore the pinned vendor lock on exit (vendor-setup.sh --verify).
# Use as: cargo ... --manifest-path vendor/gpui-component/Cargo.toml "${VENDOR_GPUI[@]}"
VENDOR_GPUI=()
for crate in gpui-pre gpui-pre-windows gpui-pre-macos gpui-pre-apple; do
    VENDOR_GPUI+=(--config "patch.crates-io.$crate.path='$PWD/vendor/$crate-0.3.3'")
done
vendor_lock_backup=$(mktemp)
cp vendor/gpui-component/Cargo.lock "$vendor_lock_backup"
trap 'cp "$vendor_lock_backup" vendor/gpui-component/Cargo.lock; rm -f "$vendor_lock_backup"' EXIT
python3 scripts/ci/vendor_gpui_lock.py Cargo.lock vendor/gpui-component/Cargo.lock
cargo tree --quiet --manifest-path vendor/gpui-component/Cargo.toml "${VENDOR_GPUI[@]}" \
    -p gpui-base -i gpui-pre --depth 0 | grep -q "vendor/gpui-pre-0.3.3" \
    || { echo "vendor tests: patched gpui-pre not selected" >&2; exit 1; }
