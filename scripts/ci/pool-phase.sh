#!/usr/bin/env bash
# Report one phase from the pool receipt. Exit 0 only when that phase ran and
# every record of it passed (a sharded gate records a phase once per shard).
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
failed = [item for item in match if int(item.get("code", 1)) != 0]
for item in failed:
    where = f" on shard {item['shard']}" if item.get("shard") else ""
    print(f"pool phase {name} failed{where}: exit {item.get('code')}", file=sys.stderr)
if failed:
    sys.exit(1)
shards = sorted({item["shard"] for item in match if item.get("shard")})
if shards:
    print(f"pool phase {name} passed on shards: {', '.join(shards)}")
PY
