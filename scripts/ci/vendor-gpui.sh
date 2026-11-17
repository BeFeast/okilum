# Sourced by scripts that build vendor/gpui-component's own tests (#653).
# Okilum ships a patched gpui-pre (Cargo.toml [patch.crates-io], #2803) whose
# API the vendor patch stack uses, while the vendor lock pins the crates.io
# gpui-pre family one release older, with exact cross-requirements. Resolve the
# vendor workspace from our own lock for this run (same family, same versions),
# select the patched gpui-pre, and restore the pinned vendor lock on exit so
# vendor-setup.sh --verify still holds afterwards.
VENDOR_GPUI="patch.crates-io.gpui-pre.path='$PWD/vendor/gpui-pre-0.3.3'"
vendor_lock_backup=$(mktemp)
cp vendor/gpui-component/Cargo.lock "$vendor_lock_backup"
trap 'cp "$vendor_lock_backup" vendor/gpui-component/Cargo.lock; rm -f "$vendor_lock_backup"' EXIT
cp Cargo.lock vendor/gpui-component/Cargo.lock
cargo tree --quiet --manifest-path vendor/gpui-component/Cargo.toml --config "$VENDOR_GPUI" \
    -p gpui-base -i gpui-pre --depth 0 | grep -q "vendor/gpui-pre-0.3.3" \
    || { echo "vendor tests: patched gpui-pre not selected" >&2; exit 1; }
