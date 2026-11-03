#!/usr/bin/env bash
set -euo pipefail
source scripts/ci/test-nofile.sh
cargo test --locked -p okilum-shell --bin okilum issue1588_ -- --nocapture --test-threads=1
