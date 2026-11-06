# Sync on an always-on Linux server (#2048)

A server is optional. Two computers can sync directly; an always-on trusted
computer holds another copy when a laptop is asleep. `okilum sync serve` runs
without a display and uses the same pairing and transfer controller as Settings.
It is part of the Linux desktop package, not a separate `okilum-cored` service.
There is no device list in the vault: receivers use their sharing computer as a
Syncthing introducer. See [plain Syncthing peers](#plain-syncthing-peers).

Use an ordinary account with a home directory. Run every command below as that
same account; otherwise it will use a different Sync identity and journal. Its
private application state and Syncthing configuration must stay outside any
synced vault. Keep a separate backup of the notes.

## Install the published Linux beta without FUSE

These commands are for x86_64 Linux with glibc (Ubuntu 22.04 or newer), `curl`,
`python3`, `sha256sum` and `tar`. The AppImage also links the platform's ordinary
XCB libraries, even in headless mode; on a minimal Debian/Ubuntu host install
`libxcb1` if the launcher reports it missing. No display server is needed.

Download the feed and verify the exact published artifact before extraction:

```bash
set -eu
sync_install=$(mktemp -d)
trap 'rm -rf -- "$sync_install"' EXIT
curl -fsS https://updates.befeast.com/okilum/linux/beta/latest.json > "$sync_install/latest.json"
sync_build=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["build"])' "$sync_install/latest.json")
sync_hash=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["sha256"])' "$sync_install/latest.json")
curl -fL "https://updates.befeast.com/okilum/linux/builds/$sync_build/Okilum-x86_64.AppImage" -o "$sync_install/Okilum.AppImage"
printf '%s  %s\n' "$sync_hash" "$sync_install/Okilum.AppImage" | sha256sum -c -
chmod +x "$sync_install/Okilum.AppImage"
(cd "$sync_install" && ./Okilum.AppImage --appimage-extract >/dev/null)
mkdir -p "$HOME/.local/share/okilum"
test ! -e "$HOME/.local/share/okilum/app-$sync_build"
mv "$sync_install/squashfs-root" "$HOME/.local/share/okilum/app-$sync_build"
ln -sfn "$HOME/.local/share/okilum/app-$sync_build" "$HOME/.local/share/okilum/app"
"$HOME/.local/share/okilum/app/AppRun" sync --help
"$HOME/.local/share/okilum/app/AppRun" sync status
```

Retain the previous `app-<build>` directory when updating. Inspect `sync status`
before changing an existing server. If a server already uses Syncthing, select
that instance explicitly rather than starting another. Verified external reuse
supports Syncthing 2.1.6 and 2.1.7; Okilum bundles 2.1.6 for managed setups.

## Select and share an existing server copy

For a server that already runs Syncthing, use its real configuration and vault
paths in place of the examples:

```bash
"$HOME/.local/share/okilum/app/AppRun" sync select "$HOME/.local/state/syncthing/config.xml"
"$HOME/.local/share/okilum/app/AppRun" sync share /srv/notes
"$HOME/.local/share/okilum/app/AppRun" sync status
```

For an existing synced vault, `share` inspects and explicitly associates that
folder. It preserves the external runtime, versioning, ignores and network
settings. Confirm `connections[].selected`, `managed: false`, the expected
`attached[].folder_id` and canonical path. A conflicting or inaccessible runtime
must be resolved explicitly; do not replace its identity or erase configuration.

For an ordinary vault on a fresh server, first enable the user manager as
described below. Then `sync share /srv/notes` prepares the
bundled runtime after checking the complete local inventory, then shares that
vault with approved computers. It seeds the required service/worktree excludes.
An incompatible existing `.stignore` is reported for review before sharing.

## Keep the server available

For a fresh managed setup, systemd's user manager must also run while logged
out: the bundled Syncthing is a user service. On a new server, enable lingering
for this account and verify the user bus before `sync share` or pairing:

```bash
sync_user=$(id -un)
sync_uid=$(id -u)
sudo loginctl enable-linger "$sync_user"
export XDG_RUNTIME_DIR="/run/user/$sync_uid"
export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
systemctl --user show-environment >/dev/null
```

Existing external Syncthing services keep their own lifecycle. If this host
already has an Okilum service, inspect and reuse it; do not install a second one.

The following system unit uses the current account and installation. It does
not install, replace or restart an existing Syncthing service. If selecting an
external runtime, ensure its own service starts at boot too.

```bash
sync_user=$(id -un)
sync_uid=$(id -u)
sudo tee /etc/systemd/system/okilum-sync-home.service >/dev/null <<UNIT
[Unit]
Description=Okilum Sync home
After=network-online.target user@$sync_uid.service
Wants=network-online.target user@$sync_uid.service

[Service]
User=$sync_user
Environment=HOME=$HOME
Environment=XDG_RUNTIME_DIR=/run/user/$sync_uid
Environment=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$sync_uid/bus
ExecStart=$HOME/.local/share/okilum/app/AppRun sync serve
Restart=on-failure
RestartSec=15

[Install]
WantedBy=multi-user.target
UNIT
sudo systemctl daemon-reload
sudo systemctl enable --now okilum-sync-home.service
systemctl is-active okilum-sync-home.service
journalctl -u okilum-sync-home.service -n 30 --no-pager
"$HOME/.local/share/okilum/app/AppRun" sync status
```

The CLI's `status`, `invite`, `share`, `connect` and `choose` commands run through
the serving process as the same user. **Do not stop the service to pair.** A
control-protocol mismatch after updating asks for a restart of the Okilum
service; it does not authorize resetting either runtime's state.

## Pair a desktop with the server

On the server, create an invitation and leave the command running:

```bash
"$HOME/.local/share/okilum/app/AppRun" sync invite --name home --wait 3600
```

Copy its first stdout line privately. In the desktop beta, open **Get notes from
another computer**, paste the URL and select **Request connection**. Check the
request on the server's terminal and approve only the expected computer. The
URL authorizes one computer, expires after one hour, and must not be published.
The `--wait` option limits how long the CLI waits; it does not extend URL lifetime.
For unattended scripts, `--approve NAME` accepts that displayed name; it is not
independent proof of the requester's identity. Share the URL only with that peer.

On the desktop choose each wanted vault, its empty local folder and **Start
first sync**. A fresh non-empty destination is refused. For a server receiving a
vault instead, invite on the desktop, then run these commands on the server:

```bash
"$HOME/.local/share/okilum/app/AppRun" sync connect '<private pairing URL>' --name home
"$HOME/.local/share/okilum/app/AppRun" sync choose '<computer name or ID>' '<vault name or ID>' /srv/new-notes
```

The new destination can be absent or empty; do not delete working files to force
it. Use `sync status` to discover exact names and IDs. Keep both Okilum processes
available while pairing or fetching a catalog. First pairing needs the same LAN
and the invitation's TCP port reachable; existing Syncthing data ports must also
be reachable through direct transport or the chosen discovery/relay policy.

## Plain Syncthing peers

For a NAS, Android Syncthing client or server without Okilum:

1. In Syncthing, configure the intended device IDs and accept/share the same
   folder on both sides. A plain peer does not consume an Okilum pairing URL.
2. On that plain peer, edit the trusted Okilum sharing device, open **Advanced**,
   enable **Introducer**, and save. Keep **Skip Introduction Removals** disabled
   to follow removal of devices that this introducer introduced. Leave automatic
   acceptance of arbitrary folders off; accept the intended vault explicitly.
3. Only trust an introducer whose device memberships you want to follow. New
   computers sharing that vault through it become direct transport peers of the
   plain server without repeating server setup. Okilum-managed receivers set
   their own sharing computer as introducer automatically after first transfer.

Devices added manually are not introduced devices. Remove their shares and
connections explicitly in Syncthing on the relevant computers. Local removal
cannot recall files already copied. Stopping Okilum is not remote revocation.
An introducer handles Syncthing transport membership, not another computer's
Okilum pairing credentials or catalog authorization.

## Verify the complete route and recover an installation

Check `systemctl is-active`, `sync status` and the selected folder in Syncthing.
Create a harmless note on a desktop and verify its exact bytes on the server,
then edit it on the server and verify the return direction. Add a third desktop
through the same sharing computer; verify its bytes reach the server without
reconfiguring the server. Turn off that sharing computer and verify a change
still travels between the remaining directly connected peers. An idle local
folder alone does not prove this route works.

Before an update record the current `app` symlink and the feed build/source,
then retain the old extracted directory. After installing the new build, restart
only `okilum-sync-home.service` and repeat the status and both-direction checks.
To roll back, stop that service, point `app` to the recorded previous directory,
then start it. Preserve application state and Syncthing state; an older build
may refuse a newer journal schema, in which case use the newer build again.
To stop headless Okilum, disable/stop only this unit. Removing it leaves the
vault files and an independently managed Syncthing service in place. Disable
lingering only if you enabled it here and no other user services need it; do not
stop a shared user manager as part of an Okilum rollback.
