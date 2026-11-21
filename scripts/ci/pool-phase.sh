#!/usr/bin/env bash
# Report one phase from the pool receipt. Exit 0 only when that phase ran and
# every record of it passed (a sharded gate records a phase once per shard; a
# phase an earlier failure skipped on one shard fails even when it passed on
# another).
#
# pool-phase.sh --overall: exit 0 only when every expected shard passed and no
# record anywhere failed or was skipped, so a phase without its own reporter
# step can never leave a green gate.
set -euo pipefail
name=${1:?phase name or --overall}
receipt=${OKILUM_POOL_RECEIPT:?}
python3 - "$receipt" "$name" "${OKILUM_POOL_SHARDS:-}" <<'PY'
import json, sys
path, name, shards_path = sys.argv[1], sys.argv[2], sys.argv[3]


def load(file, what):
    try:
        value = json.loads(open(file).read())
    except (OSError, ValueError) as error:
        print(f"pool {what} unreadable: {file}: {error}", file=sys.stderr)
        sys.exit(1)
    if not isinstance(value, list) or not all(isinstance(item, dict) for item in value):
        print(f"pool {what} is not a list of records: {file}", file=sys.stderr)
        sys.exit(1)
    return value


def bad(item):
    if item.get("skipped"):
        return "skipped after an earlier failure"
    try:
        code = int(item.get("code"))
    except (TypeError, ValueError):
        return f"no exit code ({item.get('code')!r})"
    return None if code == 0 else f"exit {code}"


def where(item):
    return f" on shard {item['shard']}" if item.get("shard") else ""


phases = load(path, "receipt")
if name == "--overall":
    failures = [f"pool phase {item.get('name')} failed{where(item)}: {reason}"
                for item in phases if (reason := bad(item))]
    if not shards_path:
        failures.append("OKILUM_POOL_SHARDS is unset: shard states unknown")
        shards = []
    else:
        shards = load(shards_path, "shard states")
        if not shards:
            failures.append("no pool shard ran")
    failures += [f"pool shard {shard.get('shard')} {shard.get('state')} (rc {shard.get('rc')})"
                 for shard in shards if shard.get("state") != "pass"]
    for failure in failures:
        print(failure, file=sys.stderr)
    if failures:
        sys.exit(1)
    print(f"pool gate passed: {len(shards)} shard(s), {len(phases)} phase record(s)")
    sys.exit(0)
match = [item for item in phases if item.get("name") == name]
if not match:
    print(f"pool phase not run: {name}", file=sys.stderr)
    sys.exit(1)
failed = [(item, reason) for item in match if (reason := bad(item))]
for item, reason in failed:
    print(f"pool phase {name} failed{where(item)}: {reason}", file=sys.stderr)
if failed:
    sys.exit(1)
shards = sorted({item["shard"] for item in match if item.get("shard")})
if shards:
    print(f"pool phase {name} passed on shards: {', '.join(shards)}")
PY
