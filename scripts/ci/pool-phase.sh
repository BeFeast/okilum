#!/usr/bin/env bash
# Report one phase from the pool receipt. Exit 0 only when that phase ran and passed.
set -euo pipefail
name=${1:?phase name}
receipt=${OKILUM_POOL_RECEIPT:?}
python3 - "$receipt" "$name" <<'PY'
import json, sys
path, name = sys.argv[1], sys.argv[2]
try:
    phases = json.loads(open(path).read())
except FileNotFoundError:
    print(f"pool receipt missing: {path}", file=sys.stderr)
    sys.exit(1)
match = [item for item in phases if item.get("name") == name]
if not match:
    print(f"pool phase not run: {name}", file=sys.stderr)
    sys.exit(1)
code = int(match[-1].get("code", 1))
if code != 0:
    print(f"pool phase {name} failed: exit {code}", file=sys.stderr)
    sys.exit(1)
PY
