#!/usr/bin/env bash
# One blocking process; never compile Rust on the worker's orchestration host.
set -euo pipefail
exec python3 "$(dirname "$0")/gh_check.py" "$@"
