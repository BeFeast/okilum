# Sync between your computers

Sync copies a vault's notes and attachments between approved computers using
Syncthing. On Linux and macOS, Okilum can set this up from a fresh installation with its
bundled runtime. You do not need an Okilum account, a service, or a server.
After the first transfer, saved changes and deletions sync automatically in both
directions. Keep a separate backup: Sync can copy an accidental deletion too.

On macOS, an enabled managed connection requests one guarded helper restart per
app process if authenticated helper health fails, including when an app update
replaces the old helper's executable. Existing signing, ownership, saved intent
and device-identity checks apply. The failure remains visible until a later
authenticated helper and REST check succeeds; a restart request alone does not
clear it. This does not rebind vault identities or change pairing/folders. Those
repairs still require the explicit Fix action. Isolated Reader profiles do not
restart the account-wide helper.

This describes the shipped Linux and macOS flow. The
[Linux outcome](https://git.oklabs.uk/BeFeast/okilum/issues/2039#issuecomment-46260)
records the fresh two-computer run on beta 7538. macOS beta 29262 or later supports
fresh setup in either role; [#2489](https://git.oklabs.uk/BeFeast/okilum/issues/2489)
records the native Mac flow, both-direction file checks and walkthrough. Windows
is **coming** in [#2490](https://git.oklabs.uk/BeFeast/okilum/issues/2490).

## Connect Linux and Mac computers

1. **On the computer that already has your notes**, open the vault and choose
   **Share with another computer…** from the app menu. In Settings → Sync, find
   the vault under **Local vaults** or **Vaults** and select **Share with my
   computers**. An ordinary local vault can be the source; it need not already
   be configured in Syncthing.
2. Select **Add computer**. In the modal, give this computer a recognizable name,
   then select **Create pairing link** and **Copy link**. Send the link privately
   to the new computer and keep Okilum open here. The current controller's link
   pairs one computer and expires after **one hour**. Both computers must be on
   the **same LAN for this first pairing**.
3. **On the new computer**, choose **Get notes from another computer** on the
   welcome screen, or **Add computer → I have a link from another computer** in
   Sync settings. Enter this computer's name, paste the link and select
   **Request connection**. The wizard keeps waiting while you switch computers;
   a lost approval reply does not require another link or request.
4. **Back on the first computer**, check the name in the visible approval banner.
   Select **Approve** only for the computer you intended to connect, or **Reject**.
   An OS notification also directs you back to Okilum. Only approval makes the
   authorized vault catalog available to the new computer.
5. **On the new computer**, select **Choose folder** for each wanted vault under
   **Available vaults**. Select an existing folder or create one in the native
   folder dialog. These choices are staged until **Start first sync**. If an
   unknown destination already has files, Okilum leaves them untouched and asks
   you to choose an empty folder; do not delete a working copy to make setup proceed.
6. Follow the first transfer in the wizard or close it and inspect the vault's
   state on the Sync page. The received vault opens when the controller reports
   it ready. Save a small edit with **Cmd+S** on Mac or **Ctrl+S** on Linux, check it on the other computer,
   then save a change there to check the return direction.

On Linux, Okilum prepares its bundled connection only after checking that no
existing or unavailable runtime needs attention. If a connection is already
present, explicitly select **Use connection**; Okilum does not silently take it
over or start a second daemon. See [existing Syncthing](#existing-syncthing).

On Mac, use the installed signed beta. **Share with my computers** or
**Request connection** prepares the bundled Sync connection. Allow local network
access when macOS asks. If macOS requires Login Items approval, allow Okilum Sync
in System Settings → General → Login Items, then use **Try again**. Keep the
existing Sync state and paired identity; opening Settings alone does not start
another connection.

Computers pair once. Further authorized vault selections between that pair do
not need a new URL. Sharing another ordinary vault makes it available to already
approved computers too. Keep the source Okilum open while the other computer is
choosing a vault; bootstrap/catalog serving is separate from file transport.
The managed Linux background service can continue transferring files after the
window closes. Offline or sleeping peers catch up when connected again; a paired
entry or local idle state alone does not prove the other computer is caught up.

The in-app **Help: connect a computer** opens the bundled
[written guide](help/sync/setup.md). [Pairing details](sync-pairing.md) explain
what the URL and confirmation authorize.

## Computer network settings (#2047)

The managed Linux connection uses **public Syncthing discovery, encrypted relays
and NAT traversal by default**. They help approved computers find and reach each
other when direct connections are blocked. File contents are encrypted between
the Syncthing devices. Discovery services can see device IDs and advertised
addresses; relays see encrypted traffic and connection metadata. These services
do not authorize computers or publish a vault catalog. First pairing still
requires the same LAN.

Okilum also exchanges **explicit Syncthing peer addresses** over the authenticated
pairing/catalog channel, bound to the approved device identity. It saves usable
literal TCP/QUIC LAN addresses and applies them alongside Syncthing's `dynamic`
address to managed peers. A stable listen port survives restart, and refresh
updates address hints. Public discovery is not the only way a managed pair can
find each other. External runtimes keep their existing address behavior.

In Settings → Sync, **Only local network and VPN** applies to every vault using
the selected managed connection **on this computer**. It is not a vault-wide or
fleet-wide switch. Turning it on disables global discovery, relays, NAT traversal
and STUN, removes the dynamic relay listener, and keeps local discovery and the
stable direct listener. Turning it off restores the public defaults. The saved
choice survives restart; older managed Linux installations migrate to the default
once, rather than having the default reapplied on every refresh.

The switch keeps explicit peer addresses. It does not change OS routing or
firewall rules: for a private connection those addresses must be reachable over
your LAN or VPN. Custom relay routes require review before local-only mode can
be reported. If a peer stays disconnected, check the named computer, its network
and the firewall/TCP-port guidance in Settings. Local idle is not proof of a
working peer connection.

A paired computer that is asleep or switched off is not an error when its
vaults keep syncing through another connected computer in the same Syncthing
folder (for example one its share peer introduced). Settings then reads, for
example, "hub is connected. idunn is offline and will catch up when it
reconnects." The firewall/TCP-port hint appears only after 30 seconds and only
when no connected computer carries one of that computer's bidirectional vaults.
`okilum sync status` reports the same observation per paired computer as
`connection` (`connected`, `relayed` with the carrying devices, or
`unreachable`). These statuses come from the managed Linux connection.

With external Syncthing, Settings shows the current network options and directs
you to change them in Syncthing. Okilum does not apply its managed defaults or
provide a mutating network switch for that connection.

## Conflicts and previous versions

A note open for reading or clean editing updates when a changed file arrives,
including Syncthing's replace-by-rename writes. If you have unsaved edits, Okilum
keeps your text and shows a conflict notice instead of overwriting it. It names
the sending computer only when it can verify that origin.

For this editor conflict, deliberate **Save** or **Keep both versions** saves your
local text beside the incoming file as
`<name>.okilum-conflict-<time>-<random>.<extension>`. The time is local; the random
part is not a computer ID. For example, a Markdown note keeps its `.md` extension.
The incoming file stays unchanged. After a copy has been saved, Reload preserves
later local edits in another copy before loading the incoming file; a failed
preservation leaves your edits open. This is the shipped [#2486](https://git.oklabs.uk/BeFeast/okilum/issues/2486)
behavior. Okilum copies deliberately avoid Syncthing's `.sync-conflict-` naming
pattern so Syncthing does not prune them as transport conflicts. Syncthing can
also create its own conflict copies. Neither kind means the conflict was resolved.

Managed source and receiving folders use Syncthing staggered versioning with
30-day retention by default, including idempotent reconciliation of existing
owned folders. Settings → Sync changes retention on this computer. Overwritten
notes appear in **Note history**; deleted notes appear in **Recover notes…** with
an exclusive Restore action. A newer local edit blocks replacement; successful
replacement retains its preimage in application history. See
[previous Sync versions](source-history.md#previous-versions-from-sync-2045).
This is transport history, not a snapshot of every local edit. Existing external
folder/version settings are preserved. Linux [in-note conflict review](sync-conflicts.md)
handles both copy kinds with side-by-side differences and Keep this / Keep that /
Keep both (#2044). The Settings → Sync inventory also offers explicit recovery of
interrupted decisions. Both originals are retained outside the index and appear
in Note history / Recover notes.

## Existing Syncthing

If you already sync a vault with Syncthing, select its runtime with **Use
connection**, then **Choose existing synced vault**. Okilum inspects the vault's
canonical folder, folder ID, device identity and accessible runtime. Check the
details and explicitly select **Use this connection**. This associates the
existing replica; it does not recreate the folder or change its current shares.
Later authorized vault connections make only the scoped additions they need.

Okilum preserves the external daemon's lifecycle, existing peer roles, folder
settings and network options. It never replaces its complete configuration.
Multiple candidates, inaccessible configuration, overlapping folders or changed
identities must be resolved explicitly. The bundled runtime is pinned to
Syncthing **2.1.6**; verified external reuse accepts **2.1.6 or 2.1.7**, not an
arbitrary installed version. Reuse consent is not consent to change network policy.

## An optional always-on computer and plain Syncthing peers

Two computers need no server. An always-on Linux computer running Okilum can
receive an empty local copy through the same steps and keep Syncthing running to
hold that copy while another peer is off. It is another trusted peer, not a
required hub or an Okilum service. It can exchange changes with its connected
peers while the other computer is unavailable.

For three or more computers, managed receivers trust the computer sharing the
vault as their Syncthing introducer after first transfer. It introduces the
other computers sharing that vault, creating direct transport connections.
An introducer does not grant Okilum catalog/pairing credentials to every peer.

[Headless installation and plain Syncthing guidance](sync-headless.md) cover
`okilum sync serve`, explicit external selection, pairing through a running
service and enabling a trusted sharing device as introducer on a plain peer.
Existing server services and configurations remain under their own control.
A server holding an ordinary synced vault can read it. Encryption for servers
the owner does not trust remains open; transport encryption does not solve that.

## Removal, ignores, and remaining work

Settings offers [Remove computer](#remove-a-computer-2042) with propagated
sharing/pairing revocation. For a manually configured plain/external Syncthing
relationship outside the introducer, remove its shares/connections in Syncthing
on the relevant computers.
Stopping a local app or disabling its optional Sync UI is not remote revocation.
Already copied files remain on a disconnected or lost computer; they cannot be
recalled. A new invitation does not revoke an earlier pairing.

The [30 October decisions and 1 November clarification](https://git.oklabs.uk/BeFeast/okilum/issues/2039)
set the direction. These parts remain unfinished:

- [#2043](https://git.oklabs.uk/BeFeast/okilum/issues/2043): preview and lossless
  first merge when the destination already has files. Local preview groundwork
  does not enable this flow. Merged PRs
  [#2511](https://git.oklabs.uk/BeFeast/okilum/pulls/2511) and
  [#2513](https://git.oklabs.uk/BeFeast/okilum/pulls/2513) supply inventory/preflight groundwork, not a complete first-merge UI.
- Untrusted-server encryption and the limits of removal for already copied data.

The old service/hub/passkey enrollment is gone from the desktop user flow
([#2049](https://git.oklabs.uk/BeFeast/okilum/issues/2049)). Its implementation
remains in the repository for independent consumers; it is not a setup requirement.

## Code references

All references below were checked in the `origin/main` revision named above.
Credentials, paired identities, address hints and operation journals stay in
private application state outside the vault and disposable index.

- [Linux Settings actions and runtime selection](../crates/okilum-shell/src/reader_settings_sync.rs),
  [visible labels and catalog](../crates/okilum-shell/src/reader_settings_sync/view.rs),
  [welcome entry](../crates/okilum-shell/src/reader_startup.rs), and
  [implicit bootstrap policy](../crates/okilum-sync-controller/src/linux_bootstrap.rs).
- [Invitation lifetime, approval and durable pairing](../crates/okilum-sync-controller/src/direct/mod.rs)
  and [authenticated wire protocol](../crates/okilum-sync-controller/src/direct/wire.rs).
- [Ordinary source sharing and ignores](../crates/okilum-sync-controller/src/direct/source.rs),
  [empty first receive, versioning and promotion](../crates/okilum-sync-controller/src/direct/transfer.rs),
  [existing-replica inspection](../crates/okilum-sync-controller/src/direct/existing.rs), and
  [supported versions](../crates/okilum-sync-controller/src/daemon.rs).
- [Explicit peer addresses](../crates/okilum-sync-controller/src/direct/addresses.rs),
  [network policy and migration](../crates/okilum-sync-controller/src/network_policy.rs), and
  [managed Linux service](../crates/okilum-sync-controller/src/lifecycle.rs).
- [Open-note updates](../crates/okilum-shell/src/reader_editor/open_note_sync.rs),
  [editor preservation and conflict-copy naming](../crates/okilum-shell/src/reader_editor.rs), and
  [in-app Help entry](../crates/okilum-shell/src/reader_sync_help.rs).

The [direct-pairing engineering notes](sync-direct-pairing.md) cover the protocol,
readiness checks and remaining lifecycle limits. The older
[compatibility evidence](sync-compatibility.md) describes isolated hub/client
fixtures, not today's user setup.

## Interrupted note creation (#2569)

New managed policies exclude `.okilum-create-*`, just as they exclude safe-save
staging. These are unpublished service artifacts, never notes or evidence of a
completed save. New receiving folders also exclude this staging prefix without
changing the source computer's ignore file.

Existing owned source/receiver folders upgrade automatically only from the
exact earlier generated LF policy. A durable before/after intent records the
folder configuration and exact bytes; the selected authenticated daemon, vault
identity and readback must still match. The folder pauses only for this scoped
change and returns to its previous pause state. A lost acknowledgement replays
that intent; a second refresh makes no further changes.

Custom rules, comments, CRLF, negations, includes, moved roots and unrelated
pending transitions require review. Settings shows a path-specific warning;
Okilum preserves those bytes rather than guessing precedence. Existing external
folders are not enrolled by the migration. No staging file is deleted by this
change. Shared policy editing/explicit migration is tracked by #2046.

## One shared ignore policy (#2046)

Settings → Sync → **Shared exclusions** on a vault previews the shared rules and
this computer's resulting `.stignore`. **Save shared policy** writes the ordinary
synced file `.okilum/sync-ignore`. The first local line is
`#include .okilum/sync-ignore`; the remaining device-local block is preserved
byte for byte, including comments, CRLF and local negations. Shared rules come
first, so local exceptions cannot re-include protected service data. Computer
and server-local additions are allowed. They are not promoted into fleet rules.

For a legacy vault, the editor offers `_Schema/stignore.template` as the initial
shared source when that regular file exists. It adds the required index,
save/create staging and worktree exclusions, including `/worktrees`. Without a
template it offers those rules plus Git and Obsidian state exclusions. The user
reviews the actual source before saving. Changes to either canonical or local
bytes after preview refuse the save; no guessed merge replaces them.

An existing owned receiver/source with the exact generated local policy adds
the include when the shared file arrives. Custom or divergent local rules are
reported for explicit preview/apply instead. Editing the shared file later
updates every including Syncthing peer, without rewriting its local block.
Multiple policy conflict copies, missing includes, symlinks, unsupported patterns
and policies excluding their own file produce visible warnings. They are never
silently resolved or replaced. External folder association does not authorize
automatic ignore migration.

A missing include is an error in pinned Syncthing 2.1.6. During first receive,
Okilum therefore validates and seeds the policy from the authenticated share
response while the new owned folder is paused, before installing `.stignore`.
The normal synced file takes over after transfer starts. Existing non-empty
unknown destinations remain refused; this does not enable #2043.

For a plain Syncthing server, first let the shared file arrive under its current
rules. Back up that server's `.stignore`, then prepend the one include line while
retaining all device-local rules. Review the server's readback and an ordinary
note transfer before removing a former template dependency. Installing that line
on a live server is a host change; follow its administrator's approval boundary.
Do not point an include at a missing file or replace a local policy blindly.

## Remove a computer (#2042)

In Settings → Sync select **Remove** on a computer, check its name/device ID,
then explicitly confirm **Remove computer**. The removed computer keeps its own
files. No copied file can be recalled. A headless computer uses
`okilum sync remove '<exact name or device ID>'`; ambiguous names are refused,
and `sync status` lists exact transport identities, including introduced peers.

The request removes sharing from Okilum-associated vaults and revokes the old
pairing/catalog authorization. It does not delete vault files or global devices
used by unrelated external Syncthing folders. A removed computer that receives
the request pauses its own associated vaults while retaining its copy. Plain
Syncthing peers follow introduction removals; manually configured relationships
outside that introduction still require their administrator's explicit action.

Okilum sends the removal through its authenticated paired connections and the
sharing/introducer computer to the other peers. A disconnected computer follows
when it reconnects. Pending delivery is visible; requests and acknowledgements
survive restarts outside the index. Old credentials retained for lost-reply
recovery can replay that recorded removal, pull only pending deliveries scoped
to the same pairing and acknowledge them; they cannot read a catalog, share a
vault or change transport settings. A fresh invitation is needed
before a removed computer can be approved again.

There is no device list in the vault. First setup, common-folder authorization,
selected daemon/root checks, exact membership PATCH/readback and existing
external ownership boundaries remain in effect.
