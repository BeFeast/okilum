# #729 live acceptance proposal — not authorized

CT119 (`okilum-inbox-dev`) hosts the live Inbox pilot, the T3 question bridge
and Oleg's Syncthing. This document is a proposal, not permission to execute.
The manager must obtain Oleg's approval of this exact procedure and window.
The previous approval (2026-10-09) is consumed. Do not inject ingress failures
on CT119; failure and rollback are exercised only on the hosted stand.

## State observed read-only on 2026-11-17

- Deployment root `/opt/okilum-inbox` (the pre-rebrand path no longer exists).
  Compose project `okilum-inbox-qa`, working dir
  `/opt/okilum-inbox/source/inbox/deploy`, files `compose.yml`,
  `compose.override.yml`, `/opt/okilum-inbox/deployment-state/active-image.json`,
  env-file `/opt/okilum-inbox/runtime.env` (all from container labels).
- Services `inbox`, `ingress` (namespace `container:<inbox>`) and `forgejo`.
  This procedure recreates only `inbox` and `ingress`.
- The existing `604-*.sh`/`642-*.sh` scripts still `cd` into the pre-rebrand
  path and cannot be used as the invocation source.
- `god` may run only `/usr/bin/docker` through passwordless sudo.

## Staging (part of the approval)

On CT119 as `god`, into a new private directory outside `/opt`:

```sh
D=$HOME/.cache/okilum-qa/729/<reviewed-sha>
mkdir -p -m 700 "$D"
# restart.py and invocation.py byte-for-byte from the reviewed commit; record sha256sum.
cat > "$D/compose-invocation.sh" <<'EOF'
#!/bin/sh
set -eu
cd /opt/okilum-inbox/source/inbox/deploy
sudo -n docker compose --env-file /opt/okilum-inbox/runtime.env up -d
EOF
sha256sum "$D"/restart.py "$D"/invocation.py "$D"/compose-invocation.sh
```

The helper reads `compose-invocation.sh`, never executes it. Without `-f` it
resolves the unambiguous default pair `compose.yml` + `compose.override.yml`
and appends `active-image.json`, which must equal the running labels.

## Read-only preflight

1. No other deploy in progress; free space covers two saved images
   (~330 MB on 2026-11-17) plus DB and source snapshots.
2. `python3 "$D/restart.py" --check`: HTTPS root 200 and anonymous WebAuthn
   challenge.
3. `python3 "$D/restart.py" --preflight --activate-script "$D/compose-invocation.sh" --rollback-script "$D/compose-invocation.sh"`:
   labels, env-file provenance, command/env/mounts/ports/networks/namespace
   match, then public readiness. Any failure is a stop; do not edit live files
   to get past it.
4. Record Syncthing `MainPID`/state and GUI status, image IDs, and private
   table fingerprints of a backup copy (no DB contents in logs).

## The one mutation

```sh
python3 "$D/restart.py" --activate-script "$D/compose-invocation.sh" --rollback-script "$D/compose-invocation.sh"
```

Same image, same config: no `--image`, no `--nginx-config`. Under
`deployment-state/deploy.lock` it snapshots DB/source/images/nginx into a new
`deployment-state/rollback-<id>/`, removes ingress, recreates Inbox, waits for
healthy, recreates ingress and checks HTTPS root + challenge. Expected outage
≈30 s (hosted same-image runs on 2026-10-09: 29.3 s). Hard ceiling of the
attempt: 90 s, then automatic rollback with its own 90 s.

Success: JSON receipt `deployed_public_ready`, unchanged image IDs, ingress
`NetworkMode` = `container:<new inbox id>`, public readiness repeated, table
and delivered-ack fingerprints unchanged apart from legitimate new operations
or freshness timestamps, Syncthing PID/GUI unchanged.

## Rollback

Automatic: on any failure the helper restores the saved nginx bytes and image
IDs, repeats the ordered start and exits nonzero with `rolled_back_public_ready`.
The live DB is never restored from the snapshot.

On `rollback_failed` or an interrupted process, stop and notify the manager.
Proposed manual recovery, only with the exact rollback directory printed by
this attempt:

```sh
python3 - "$D/restart.py" /opt/okilum-inbox/deployment-state/rollback-EXACT-ID "$D/compose-invocation.sh" <<'PYRECOVER'
import fcntl, importlib.util, json, os, pathlib, sys
os.umask(0o077)
sys.path.insert(0, str(pathlib.Path(sys.argv[1]).resolve().parent))
spec = importlib.util.spec_from_file_location("restart", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
state = pathlib.Path("/opt/okilum-inbox/deployment-state")
backup = pathlib.Path(sys.argv[2]).resolve()
assert backup.parent == state and backup.name.startswith("rollback-")
script = pathlib.Path(sys.argv[3])
d = m.Deployment(pathlib.Path("/opt/okilum-inbox/source/inbox/deploy"), state,
                 "https://inbox-qa.oklabs.uk", invocation=m.Invocation.from_scripts(script, script))
with (state / "deploy.lock").open("a") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    d.docker("image", "load", "--input", str(backup / "images.tar"))
    d.nginx.write_bytes((backup / "nginx.conf").read_bytes())
    d.select_images(json.loads((backup / "images.json").read_text()))
    d.bounded_start()
    print("Previous images/config publicly ready; durable reconciliation still required")
PYRECOVER
```

If that fails too, preserve all volumes and evidence and escalate: no DB
restore, journal replay, volume removal or unrelated service changes.
Preserve the earlier `rollback-4a28…` and `rollback-ddba…` directories.

## History

- First attempt (2026-10-08) stopped before any mutation: `sudo docker cp`
  produced a root-owned snapshot. The helper now streams it into an
  operator-owned file and restore-checks it first.
- Second attempt (2026-10-09) omitted the external env-file; Inbox came up with
  empty AI arguments and the rollback repeated it. Recovery with the original
  invocation took ≤218 s. The helper now derives the invocation from a script,
  checks Compose label provenance and refuses runtime drift before mutation.
