#!/usr/bin/env bash
# Full required Linux gate, shared by the local runner and hosted PR pilot.
# Optional part (#3198): `all` (default), `checks` (everything but the workspace
# census) or `workspace` (only the census); the hosted gate runs the two halves
# as parallel jobs so neither hits the 53 min bridge budget.
set -euo pipefail
part=${1:-all}
case "$part" in
    all | checks) ;;
    workspace) exec bash scripts/ci/linux-rust-tests.sh workspace ;;
    *)
        printf 'check-linux.sh: unknown part %q (all, checks, workspace)\n' "$part" >&2
        exit 2
        ;;
esac
source "$(dirname "${BASH_SOURCE[0]}")/test-nofile.sh"
cargo fmt --check
# Shipped icons must be the approved Okilum mark (#977).
python3 scripts/rebrand/check_brand.py
cargo clippy --workspace --all-targets -- -D warnings
python3 -m unittest discover -s scripts/updater -p 'test_*.py'
python3 -m unittest discover -s scripts/arch -p 'test_*.py'
python3 -m unittest discover -s scripts/windows -p 'test_*.py'
python3 -m unittest discover -s scripts/releases -p 'test_*.py'
python3 -m unittest discover -s scripts/uninstall -p 'test_*.py'
python3 scripts/dev/test-sprint-report.py
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
bash scripts/ci/linux-rust-tests.sh "$part"
