#!/usr/bin/env bash
# Ephemeral hosted Ubuntu runners only; keep the existing ten-minute step ceiling.
set -euo pipefail
exec timeout --signal=TERM --kill-after=15s 9m \
    python3 "$(dirname "$0")/hosted-ubuntu-apt.py" "$@"
