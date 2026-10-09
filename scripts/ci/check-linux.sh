#!/usr/bin/env bash
# Full required Linux gate, shared by the local runner and hosted PR pilot.
set -euo pipefail
cargo fmt --check
# Shipped icons must be the Okilum mark, never the Tessera one (#977).
python3 scripts/rebrand/check_brand.py
cargo clippy --workspace --all-targets -- -D warnings
python3 -m unittest discover -s scripts/updater -p 'test_*.py'
python3 -m unittest discover -s scripts/arch -p 'test_*.py'
python3 -m unittest discover -s scripts/windows -p 'test_*.py'
python3 -m unittest discover -s scripts/releases -p 'test_*.py'
bash -n scripts/releases/mirror.sh
python3 scripts/brand-assets.py verify
python3 scripts/test-third-party-notices.py
bash -n scripts/build-macos-ci.sh scripts/updater/sign-bundle.sh scripts/ci/check-macos.sh
python3 -m unittest discover -s scripts/ci -p 'test_*.py'
python3 scripts/test-maintenance-matrix.py
cargo test -p okilum-core -p okilum-shell -p okilum-sync
cargo check -p okilum-shell --no-default-features
cargo test -p okilum-core --no-default-features --test portable_reader
