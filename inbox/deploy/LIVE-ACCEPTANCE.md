# #729 live acceptance proposal — not authorized

CT119 hosts the live Inbox pilot and Oleg's Syncthing. This document is a
proposal, not permission to execute. The manager must approve this exact
procedure and window separately. First pass isolated container integration.
Do not inject ingress failures on CT119; test these only on the isolated stand.

## Scope and preflight

1. Record the reviewed commit and script checksum. Stage only the approved
   `restart.py` outside the source tree, at a manager-approved path. Do not
   update the application source, Compose topology, credentials or images.
2. Confirm no concurrent deploy; verify current Inbox health, running ingress,
   current image IDs, and available disk space sufficient for saved images,
   source and DB snapshots. Record only IDs/status/free space, never env values.
3. Run `python3 /approved/path/restart.py --check`: root HTTPS 200 and an
   anonymous WebAuthn challenge must pass. Do not complete an owner login.
4. Record durable operation/ack counts and IDs through an approved read-only
   query, stored privately; avoid DB contents in public logs. Confirm pending
   work can tolerate the window. An absent query/probe is a stop condition.

## Exact mutation

5. Run `sudo python3 /approved/path/restart.py` once, with no image or config
   arguments. This is a same-image restart only. The tool acquires
   `/opt/tessera-inbox/deployment-state/deploy.lock`, creates a private
   `rollback-<id>/` snapshot (online DB, source, images, nginx and image IDs),
   and writes `active-image.json` selecting existing exact images.
6. The tool removes only ingress, recreates Inbox with existing volumes,
   waits for Inbox health, recreates ingress, then verifies public HTTPS root
   and WebAuthn challenge. No volume removal or database restoration occurs.
   Syncthing, `/srv/vault`, systemd, `.env`, credentials and enrollment remain
   outside the procedure.
7. Require `deployed_public_ready`, healthy Inbox and a new ingress sharing
   its network namespace. Repeat public readiness; reconcile durable IDs and
   acknowledgements from step 4, allowing legitimate new operations. Verify
   no replay/duplicate ack. Preserve private snapshots and sanitized receipt.

## Rollback

On ordinary failure the helper restores saved nginx bytes and old image IDs,
then performs the same ordered restart and public checks. It exits nonzero;
require `rolled_back_public_ready`, then repeat step 7. Never restore the
snapshot DB over the live DB: that can erase newly accepted operations.

For process interruption or `rollback_failed`, stop and notify the manager.
The following manual recovery is part of the proposed approval scope, only
for the exact rollback directory emitted by this invocation:

1. Acquire the same deployment lock; ensure the original process has exited.
2. Load that directory's `images.tar` with `docker image load --input` if its
   exact saved image IDs are missing. Restore only saved `nginx.conf` bytes.
3. Use `Deployment.select_images(json.loads(saved_images_json))` from the
   reviewed helper to write `active-image.json`, then `Deployment.ordered_start()`
   with the same compose/state/origin defaults while holding the lock. This
   removes ingress before recreating Inbox, then ingress, and checks public
   readiness. It does not take another snapshot or select a mutable tag.
4. Re-run step 7. If recovery fails, preserve all volumes/evidence and escalate;
   no DB restore, journal replay, volume deletion or unrelated service changes.

For steps 1–3, after replacing the two reviewed paths below, the exact command
is (the backup path must be the receipt from this attempt, never “latest”):

```sh
sudo python3 - /approved/path/restart.py /opt/tessera-inbox/deployment-state/rollback-EXACT-ID <<'PYRECOVER'
import fcntl, importlib.util, json, os, pathlib, sys
os.umask(0o077)
spec = importlib.util.spec_from_file_location("restart", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
state = pathlib.Path("/opt/tessera-inbox/deployment-state")
backup = pathlib.Path(sys.argv[2]).resolve()
assert backup.parent == state and backup.name.startswith("rollback-")
d = m.Deployment(pathlib.Path("/opt/tessera-inbox/source/inbox/deploy"),
                 state, "https://inbox-qa.oklabs.uk")
with (state / "deploy.lock").open("a") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    images = json.loads((backup / "images.json").read_text())
    d.run(["docker", "image", "load", "--input", str(backup / "images.tar")])
    d.nginx.write_bytes((backup / "nginx.conf").read_bytes())
    d.select_images(images)
    d.ordered_start()
    print("Previous images/config publicly ready; durable reconciliation still required")
PYRECOVER
```

## Downtime and outstanding prerequisite

Backup happens before planned outage. Outage begins when ingress is removed
and ends only after public readiness succeeds. Each health/readiness phase has
an approximately 120-second polling budget; individual Docker commands allow
300 seconds and HTTP requests allow 10 seconds. These are not an end-to-end
outage bound. Automatic rollback can extend downtime substantially. Do not
promise a short or bounded maintenance window until isolated measurements
establish normal restart and rollback durations and the manager accepts them.

Current blocker: CT141's unprivileged LXC denies the OCI `/proc` mount even for
`docker run --rm --network none python:3.13-alpine ...`. A scratch-only Docker
daemon starts, but no container can execute. No LXC configuration was changed.
An approved container-capable stand (or an approved CT141 nesting procedure)
is required before this proposal is ready for live approval.
