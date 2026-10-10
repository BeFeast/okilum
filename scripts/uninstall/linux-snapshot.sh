#!/usr/bin/env bash
# Snapshot for Linux uninstall acceptance (#974): every path under $HOME plus
# Okilum's entries in /tmp, one "F <path>" per line. Compare with diff.py.
# Usage: linux-snapshot.sh OUT
set -euo pipefail
out=${1:?output file}
# diff.py checks names only below this home, so a QA home path may say okilum.
echo "# home $HOME" > "$out"
{
  find "$HOME" -mindepth 1 2>/dev/null
  find /tmp -maxdepth 1 -name 'okilum-*' 2>/dev/null
} | sed 's/^/F /' | LC_ALL=C sort -u >> "$out"
echo "snapshot: $(wc -l < "$out") entries -> $out"
