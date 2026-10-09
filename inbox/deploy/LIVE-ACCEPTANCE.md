# #729 live acceptance proposal — not authorized

CT119 hosts the live Inbox pilot and Oleg's Syncthing. This document is a
proposal, not permission to execute. The manager must approve this exact
procedure and window separately. First pass isolated container integration.
Do not inject ingress failures on CT119; test these only on the isolated stand.

## Scope and preflight

1. Record the reviewed commit and script checksum. Stage only the approved
   `restart.py` and its sibling `invocation.py` outside the source tree, at a manager-approved path. Do not
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
   arguments. This is a same-image restart only. Include the established external
   `--activate-script /opt/tessera-inbox/604-activate.sh --rollback-script /opt/tessera-inbox/604-rollback.sh`; resolved runtime preflight must pass. The tool acquires
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
sys.path.insert(0, str(pathlib.Path(sys.argv[1]).resolve().parent))
spec = importlib.util.spec_from_file_location("restart", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
state = pathlib.Path("/opt/tessera-inbox/deployment-state")
backup = pathlib.Path(sys.argv[2]).resolve()
assert backup.parent == state and backup.name.startswith("rollback-")
d = m.Deployment(pathlib.Path("/opt/tessera-inbox/source/inbox/deploy"),
                 state, "https://inbox-qa.oklabs.uk",
                 invocation=m.Invocation.from_scripts(
                     pathlib.Path("/opt/tessera-inbox/604-activate.sh"),
                     pathlib.Path("/opt/tessera-inbox/604-rollback.sh")))
with (state / "deploy.lock").open("a") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    images = json.loads((backup / "images.json").read_text())
    d.run(["docker", "image", "load", "--input", str(backup / "images.tar")])
    d.nginx.write_bytes((backup / "nginx.conf").read_bytes())
    d.select_images(images)
    d.bounded_start()
    print("Previous images/config publicly ready; durable reconciliation still required")
PYRECOVER
```

## Downtime and outstanding prerequisite

Backup happens before planned outage. Outage begins when ingress is removed
and ends only after public readiness succeeds. The helper now arms a 90-second deadline covering the whole ordered startup,
including Docker, health and public checks. Expiry initiates automatic rollback;
rollback has its own 90-second limit. This does not guarantee recovery within
120 seconds on a failed host. Stop after any unexpected outcome and report it.
The hosted integration reports conservative outage bounds from ingress removal
through public readiness, including rollback, and asserts each is under 120 seconds.

The first live attempt stopped before service mutation because `sudo docker cp`
created an unreadable root-owned 0600 snapshot. No restart occurred. The helper
now streams the snapshot into a private file owned by its operator and restores
it into a temporary DB before any service mutation. Hosted tests must validate
this fix before another live procedure is proposed.

CT141 configuration remains unchanged. Real isolated integration runs only on a
disposable GitHub-hosted runner via the proposed `inbox-restart` commit-ci lane.
It builds the canonical Inbox Dockerfile at the tested SHA, uses the deployment
nginx image/config and shared namespace topology, and creates a synthetic enrolled
DB and delivered reply. It receives no CT119 access or production credentials.
The fixture HTTPS edge uses an ephemeral trusted certificate; no TLS bypass.
Only sanitized timings, image IDs and assertions are uploaded, never the DB/key.
Live acceptance remains pending a hosted PASS and a new manager-approved procedure.


The subsequent live attempt on 2026-10-09 failed because the helper omitted
that external env-file. It recreated Inbox with empty AI arguments; automatic
rollback repeated the same configuration error. Authorized manual recovery
with the original env-file restored public readiness at 00:33:44 UTC. The
attempt-to-recovery upper bound was 218 seconds. Delivery tables were unchanged;
only five question `observed_at` freshness timestamps advanced. Keep the PR
WIP until the external-env regression passes hosted integration and the manager
approves any further live acceptance. Do not reuse the old wrapper/checksums.
