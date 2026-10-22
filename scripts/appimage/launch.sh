#!/usr/bin/env bash
# Exercise the shipped runtime and launcher without a smoke-supplied library path.
set -euo pipefail
exec env -u LD_LIBRARY_PATH APPIMAGE_EXTRACT_AND_RUN=1 "$@"
