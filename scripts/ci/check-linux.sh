#!/usr/bin/env bash
# Full required Linux gate, shared by the local runner and hosted PR pilot.
set -euo pipefail
cargo fmt --check
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
cargo test -p tessera-core -p tessera-shell -p tessera-sync
cargo check -p tessera-shell --no-default-features
cargo test -p tessera-core --no-default-features --test portable_reader
