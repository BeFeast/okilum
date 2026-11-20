# LAN QA deployment plan (#465)

Target: `ssh okilum-inbox-dev`, DevBox CT119, Debian 13, `/opt/okilum-inbox`.
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

For initial installation only, copy a reviewed source tree to
`/opt/okilum-inbox/source` on CT119. From
`source/inbox/deploy`, run `sudo docker compose build` then `sudo docker compose up -d`.
The init service creates private data/backups and four allowed fixture folders.
Provision `/opt/okilum-inbox/secrets/cliproxy-key` as god (uid1000), mode0600,
from an Infisical reference; never include it in the source archive or `.env`.
Compose `.env` may hold `AI_ENDPOINT`, `AI_MODEL`, `CLIPROXY_CREDENTIAL_FILE` only.
For capture-only checks, an empty private credential file is sufficient with AI unset.

Enrollment: `sudo docker compose exec inbox okilum-inboxd bootstrap --data-dir
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

For an **existing, enrolled CT119 deployment**, use `restart.py`, never a bare
`compose restart inbox` or a parallel `compose restart inbox ingress`. Ingress
uses `network_mode: service:inbox`: after Inbox alone is recreated, ingress
stays in the old namespace and the public route answers 503 while Inbox
`/health` is green. A parallel restart can start ingress before Inbox is back.
Initial installation and enrollment remain the procedure above; this helper
requires one running Inbox and ingress and never bootstraps an owner.

CT119 hosts the live Inbox pilot, the T3 question bridge and Oleg's Syncthing:
treat it as production. Copying files, changing configuration, restarting
services or running this helper there needs the manager's explicit approval of
the exact procedure in `LIVE-ACCEPTANCE.md`. Never use `down -v`, remove data
volumes, restart Syncthing or touch `/srv/vault`.

```sh
# Public readiness only (creates an anonymous, expiring login challenge).
python3 restart.py --check
# Read-only drift check: script invocation vs running containers, then public readiness.
python3 restart.py --preflight --activate-script S --rollback-script S
# Same-image restart; ingress may already be running.
python3 restart.py --activate-script S --rollback-script S
# Deploy a reviewed, already-loaded image (never pulled or built here).
python3 restart.py --activate-script S --rollback-script S --image okilum-inbox-qa:reviewed
# Same-image nginx change from a separately staged, reviewed file.
python3 restart.py --activate-script S --rollback-script S --nginx-config /path/to/reviewed-nginx.conf
```

`S` is a reviewed shell script that holds the established literal Compose
invocation (`cd` to the deploy directory, then `sudo -n docker compose
--env-file /opt/okilum-inbox/runtime.env up -d`). The helper reads it and never
executes it. Activate and rollback scripts must carry the same invocation; one
file may serve as both. Its privilege prefix (`sudo -n`) is also used for the
helper's raw `docker inspect/save` calls, so the operator needs no Docker group.

Runtime configuration is part of the rollback identity (the 2026-10-09 incident:
a rebuilt Compose command without `--env-file` recreated Inbox with empty AI
arguments, and the rollback repeated it). The helper keeps cwd, file order,
overrides, project and external env-files, refuses dynamic shell setup or
exports, and requires the running containers' Compose labels to agree on
project, file order, working directory and env-file. Missing provenance stops.
Before any snapshot or mutation it compares command, entrypoint, environment,
bind/volume mounts, tmpfs, rootfs mode, published ports, networks and the exact
namespace owner against `docker inspect`. Errors carry no values. This is a
bounded fidelity check: other Docker options need a reviewed extension first.

Before mutation, the helper runs the online SQLite backup, streams the
completed DB into an operator-owned private file, restores it into a temporary
DB and checks integrity and table counts, saves both running images, archives
the host source (without `.env`, secrets, data, backups or build caches) and
the nginx bytes into a new private `deployment-state/rollback-*/` directory.
A snapshot failure stops before either service is touched.

The sequence: remove only the stateless ingress container, recreate Inbox with
its durable volumes, wait for `healthy`, recreate ingress, then require public
HTTPS **200 at `/`** and **200 with `publicKey.challenge` from
`/api/v1/auth/login/start`**. Redirects, a root-only 200 or local health cannot
pass. No owner login is completed or bypassed; no challenge or cookie is logged.
One 90-second alarm covers the whole ordered start, Docker and HTTP waits
included.

On failure the helper restores the old nginx bytes and exact image IDs, repeats
the ordered start (its own 90-second budget) and rechecks public readiness; it
still exits nonzero. `receipt.json` records `deployed_public_ready`,
`rolled_back_public_ready` or `rollback_failed`. **The DB and other volumes are
never restored automatically**: that would erase operations accepted after the
snapshot. `active-image.json` (outside the source tree) pins exact images; for
manual Compose commands keep `-f compose.yml`, the existing
`compose.override.yml` and `-f /opt/okilum-inbox/deployment-state/active-image.json`
in that order.

Unit tests (`test_restart.py`) run in the agent pre-merge gate and the Inbox
lane. The hosted `inbox-restart` commit-ci lane (`integration_restart.py`)
builds this Dockerfile on a disposable GitHub runner and exercises env-file,
credential-mount and bind-IP drift refusal, same-image, explicit-image and
config-only restarts, and a broken ingress that must fail readiness while Inbox
is healthy and roll back automatically. It uploads only a sanitized summary.
