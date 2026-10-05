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

Copy a reviewed source tree to `/opt/tessera-inbox/source` on CT119. From
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
