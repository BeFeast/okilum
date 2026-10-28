#!/usr/bin/env bash
# Full required Linux gate, shared by the local runner and hosted PR pilot.
set -euo pipefail
cargo fmt --check
# Shipped icons must be the approved Okilum mark (#977).
python3 scripts/rebrand/check_brand.py
cargo clippy --workspace --all-targets -- -D warnings
python3 -m unittest discover -s scripts/updater -p 'test_*.py'
python3 -m unittest discover -s scripts/arch -p 'test_*.py'
python3 -m unittest discover -s scripts/windows -p 'test_*.py'
python3 -m unittest discover -s scripts/releases -p 'test_*.py'
python3 -m unittest discover -s scripts/uninstall -p 'test_*.py'
for script in scripts/releases/mirror.sh scripts/ci/apt-bounded.sh scripts/ci/hosted-ubuntu-apt.sh scripts/ci/ubuntu-smoke-apt.sh scripts/appimage/smoke.sh; do
    bash -n "$script"
done
python3 scripts/brand-assets.py verify
python3 scripts/test-third-party-notices.py
for script in scripts/build-macos-ci.sh scripts/updater/sign-bundle.sh scripts/ci/check-macos.sh scripts/ci/supervisor-bundle.sh scripts/ci/check-supervisor-bundle.sh; do
    bash -n "$script"
done
python3 -m unittest discover -s scripts/ci -p 'test_*.py'
python3 -m unittest discover -s scripts/perf -p 'test_*.py'
python3 scripts/test-maintenance-matrix.py
cargo test -p okilum-appimage --all-features
cargo test -p okilum-core -p okilum-shell -p okilum-sync -p okilum-t3
# #588: keep shared durable pairing recovery covered on both Unix platforms.
cargo test --locked -p okilum-sync-controller --lib enrollment::
cargo test --locked -p okilum-sync-controller --test enrollment_storage
cargo test --locked -p okilum-sync-controller --lib sidecar::macos::bundle::
cargo test --locked -p okilum-sync-controller --lib sidecar::macos::connection::
cargo test --locked -p okilum-sync-controller --lib daemon::
cargo test --locked -p okilum-sync-controller --lib presentation::
cargo test --locked -p okilum-sync-controller --test daemon_access
# A second, explicit compiled distribution must refuse updater work even when
# inherited AppImage/update-receipt variables would otherwise enable it.
OKILUM_SPRINT_BUILD=1 OKILUM_RELEASE_VERSION='Sprint CI' \
    APPIMAGE=/nonexistent/OkilumSprint.AppImage OKILUM_APPIMAGE_UPDATE_RECEIPT=/nonexistent/receipt \
    cargo test --locked -p okilum-shell --bin okilum --features linux-sprint sprint_distribution -- --test-threads=1
cargo check -p okilum-shell --no-default-features
cargo test -p okilum-core --no-default-features --test portable_reader
# Vendored gpui-kit tests when the patch stack changes (otherwise a skip line).
bash scripts/ci/vendor-tests.sh
