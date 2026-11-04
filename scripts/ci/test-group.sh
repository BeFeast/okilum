#!/usr/bin/env bash
# Fast admission is Linux; retain the existing native-platform fast.sh route.
set -euo pipefail
if [[ ${RUNNER_OS:-Linux} == Linux ]]; then
    exec python3 "$(dirname "$0")/test-group.py" "$@"
fi
shift 2
exec "$@"
