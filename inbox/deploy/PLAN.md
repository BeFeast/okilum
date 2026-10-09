# LAN QA deployment plan (#465)

Target: `ssh tessera-inbox-dev`, DevBox CT119, Debian 13, `/opt/tessera-inbox`.
Compose ingress binds **10.10.0.34:8080**; existing NPM terminates TLS at
`https://inbox-qa.oklabs.uk`. Keep the daemon listener on loopback inside the
Compose network namespace shared with the ingress proxy. The daemon is never
hosted on maestro. Production on Mimir is a separate step.

Keep canonical SQLite data in a private durable volume, separate from the small
fixture vault volume. No real vault mounts or replication. The fixture connector
will be limited to explicitly configured PARA folders and create-only writes.
CLIProxyAPI credentials arrive as a private read-only file from Infisical or
systemd credentials; neither Compose YAML nor DB nor command arguments contain
key values. Bootstrap enrollment is an explicit terminal command, not a logged
container startup action.

CT119 is already in nightly PBS rootfs/volume backup. Before storing real user data,
install an application-consistent pre-backup script: use SQLite's online backup API
(or sqlite3 `.backup`) into a temporary file outside the live DB directory, verify
`PRAGMA integrity_check`, fsync and atomically rename to the completed backup.
The script must exit nonzero on failure; a pre-vzdump hook invokes it and blocks the
application backup on failure. Do not copy a live main DB without its WAL. Restrict
backup directory permissions; include credential references, not credentials, in
the restore instructions. PBS includes the completed backup plus fixture volume.

Before LAN acceptance, restore the completed DB into a disposable private directory
and verify captures, passkey public records and operation identities. Never start
the restored copy with live provider/connector access: restore reconciliation must
precede any outbound operation. Interrupted AI calls become uncertain, never replay.
This plan is not a claim that a hook or restore test has already been installed.

## Compose procedure

For initial installation only, copy a reviewed source tree to `/opt/tessera-inbox/source` on CT119. From
`source/inbox/deploy`, run `sudo docker compose build` then `sudo docker compose up -d`.
The init service creates private data/backups and four allowed fixture folders.
Provision `/opt/tessera-inbox/secrets/cliproxy-key` as god (uid1000), mode0600,
from an Infisical reference; never include it in the source archive or `.env`.
Compose `.env` may hold `AI_ENDPOINT`, `AI_MODEL`, `CLIPROXY_CREDENTIAL_FILE` only.
For capture-only checks, an empty private credential file is sufficient with AI unset.

Enrollment: `sudo docker compose exec inbox tessera-inboxd bootstrap --data-dir
/data --origin https://inbox-qa.oklabs.uk`. Run in a private terminal and open the
printed fragment URL within ten minutes. Do not copy it into issue comments/logs.
The UI can publish only into Projects/Areas/Resources/Archives on the fixture volume.

`pre-backup.sh` invokes the implemented online-backup tool inside the service.
It fails closed on absent DB, failed integrity/table checks or filesystem errors.
Have the HomeLab executor wire this command into CT119's pre-vzdump phase; do not
change the shared PBS job or its other guests from this application deployment.
The produced `/backups/inbox-latest.db` is included in the existing rootfs backup.
For a restore probe, copy that snapshot to a disposable directory, check integrity
and exact captures/operation IDs with sqlite3; never start a second provider-enabled
service against the restored snapshot. Fixture files and DB must be restored together.
Pending publication recovery retains its staging hard link to distinguish its own
file from an unrelated equal-content collision. Do not discard hidden staging files
while operations remain unconfirmed. They are cleaned after journalled publication.

## Ordered restart and deployment (#729)

For an **existing, enrolled CT119 deployment**, use `restart.py` instead of parallel
`compose restart inbox ingress`. Ingress shares Inbox's network namespace: an old
container can keep the old namespace even if local Inbox health is green. Initial
installation/enrollment remains the procedure above; this tool requires one running
Inbox and ingress and does not bootstrap an owner.

CT119 hosts a live Inbox pilot and Oleg’s Syncthing: treat it as production.
Do not copy files, change configuration, restart services, or run this procedure
on CT119 without the manager’s explicit approval of the exact live procedure.
First validate failures and rollback on fixtures and an isolated stand.
Run the approved tool on CT119 with Docker access only in the approved window.
It waits on a deployment lock; coordinate the maintenance window with the manager. Do not run another Compose
mutation alongside it. Never use `down -v`, remove data volumes, restart Syncthing,
or touch `/srv/vault`.

```sh
# Read-only public readiness (creates only an anonymous, expiring login challenge).
python3 inbox/deploy/restart.py --check
# Same-image restart; ingress may already be running.
sudo python3 inbox/deploy/restart.py
# Deploy a reviewed, already-loaded image; mutable tags are resolved to local IDs.
sudo python3 inbox/deploy/restart.py --image tessera-inbox-qa:reviewed
# Same-image nginx configuration change, from a separate staged file.
sudo python3 inbox/deploy/restart.py --nginx-config /path/to/reviewed-nginx.conf
```

Build/import the reviewed image before this procedure; the tool never pulls or
builds. An image deploy changes the running application image, not the host source
checkout. Compose topology, `.env`, credential references and database-schema
migrations are outside this helper's scope: stage and review those separately,
with an explicit backwards-compatible rollback plan. Do not overwrite the live
Compose/nginx files before invoking the helper: it must see the previous config.

Before mutation, the tool runs the existing online SQLite backup, copies the
completed DB through a binary stream into an operator-owned private file,
restores that snapshot into a temporary DB and checks integrity/table counts,
saves both current Docker images, archives the host source, and
saves nginx configuration into a new private
`/opt/tessera-inbox/deployment-state/rollback-*/` directory. `.env`, secrets and
build caches are excluded from the source archive; credential references remain
in the original external configuration. Preserve these backups until acceptance.
Snapshot failure stops before touching either service.

The sequence is explicit: remove only the stateless ingress container, recreate
Inbox with its existing durable volumes, wait for healthy, then recreate ingress.
Readiness requires verified public HTTPS **HTTP 200 at `/`** and **HTTP 200 with a
`publicKey.challenge` from `/api/v1/auth/login/start`**. Redirects, a root-only 200,
and container-local health alone cannot pass. No challenge response/cookie is
logged, and no owner login is completed or bypassed.

On failure the tool restores the old nginx bytes and exact image IDs, repeats the
same ordered procedure and checks public readiness again. The operation still
exits nonzero after a successful rollback. `receipt.json` distinguishes
`deployed_public_ready`, `rolled_back_public_ready`, and `rollback_failed`.
**The DB and other durable volumes are never automatically restored from backup**:
that could erase operations accepted after the snapshot. An incompatible schema
or failed rollback needs explicit recovery using the retained backup and journal
reconciliation; do not replay pending external operations.

`active-image.json` is a Compose override outside the source tree. Subsequent
restarts/deployments must use this helper so they retain the selected exact images.
For manual recovery, include `-f compose.yml`, the existing `compose.override.yml`
(if present), and `-f /opt/tessera-inbox/deployment-state/active-image.json` in that
order. Never remove the active override merely to make a failing service start.
If interrupted during deployment, retain the rollback directory; stop ingress,
restore its saved nginx config and image selection, then run the ordered procedure.

Deterministic tests run in Inbox CI. Isolated integration must additionally exercise
a same-image restart and a config-only/image deployment, and deliberately break
ingress to prove public readiness fails while Inbox health remains green.
Live acceptance is a separately approved same-image restart; failure injection
on CT119 is not authorized by isolated test results or by this document.
Record receipts and public status only; do not put DB contents, challenge bodies,
cookies or credentials into evidence.


The whole ordered-start attempt has a 90-second alarm, including Docker and
HTTP waits. Expiry initiates rollback with a fresh 90-second recovery budget;
this is not a guarantee of total outage below two minutes on a failed host.
The hosted `inbox-restart` acceptance asserts a conservative outage bound under
120 seconds for each tested case, including automatic rollback. It uploads only
sanitized `summary.json`; no DB, test passkey or TLS key is published. See
`LIVE-ACCEPTANCE.md` for the separate, manager-approved live procedure.
