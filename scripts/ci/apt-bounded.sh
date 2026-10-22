#!/usr/bin/env bash
# CI only: bound locks, stalled downloads and the entire apt invocation.
# Run as root (sudo on the hosted runner, already root in smoke containers).
set -euo pipefail
# Overrides let hosted regression tests exercise deadlines without waiting minutes.
exec timeout --signal=TERM --kill-after="${OKILUM_APT_KILL_AFTER:-15s}" \
    "${OKILUM_APT_TIMEOUT:-4m}" apt-get \
    -o Acquire::Retries=2 \
    -o Acquire::http::Timeout=30 \
    -o Acquire::https::Timeout=30 \
    -o DPkg::Lock::Timeout=60 \
    -o APT::Update::Error-Mode=any \
    "$@"
