# Sync between your computers

Sync copies a vault's notes and attachments between approved computers using
Syncthing. On Linux, Okilum can set this up from a fresh installation with its
bundled runtime. You do not need an Okilum account, a service, or a server.
After the first transfer, saved changes and deletions sync automatically in both
directions. Keep a separate backup: Sync can copy an accidental deletion too.

This describes the Linux flow on `origin/main`, checked at
`547a8c4c96be464a4d66921447c6c93e5ad90162` (2026-11-04). The
[Linux outcome](https://git.oklabs.uk/BeFeast/okilum/issues/2039#issuecomment-46260)
records the fresh two-computer run on beta 7538. macOS is **coming** in
[#2489](https://git.oklabs.uk/BeFeast/okilum/issues/2489); Windows is **coming** in
[#2490](https://git.oklabs.uk/BeFeast/okilum/issues/2490). Platform code and bundled
binaries alone do not establish that the same setup works there today.

## Connect two Linux computers

1. **On the computer that already has your notes**, open the vault and choose
   **Share with another computer…** from the app menu. In Settings → Sync, find
   the vault under **Vaults on this computer** and select **Share with my
   computers**. It becomes **Shared with approved computers**. An ordinary local
   vault can be the source; it need not already be configured in Syncthing.
2. Give **This computer** a recognizable name. Select **Add computer**, then
   **Copy pairing URL**, and send the URL privately to the new computer. Keep
   the first computer's Okilum open. The URL pairs one computer, expires after
   **10 minutes**, and is not a folder path. Both computers must be on the
   **same LAN for this first pairing**. There is no lifetime setting to adjust.
3. **On the new computer**, choose **Get notes from another computer** on the
   welcome screen. This opens Sync and enables the optional Sync feature.
   Enter its name under **This computer**, paste the URL into **Paste pairing
   URL**, and select **Connect**. You can also reach this step in Settings → Sync.
4. **Back on the first computer**, check the requesting computer's name and
   displayed identities. Select **Confirm computer** only for the computer you
   intended to connect; reject an unexpected request. Only then can the new
   computer see the vaults authorized for it.
5. **On the new computer**, select the vault under **Available vaults** and
   select **Choose local folder**. Okilum suggests a new folder based on the
   vault name. Alternatively, **Use existing folder** lets you choose an empty
   folder. Confirm the local destination. **A non-empty destination is refused**:
   merging two existing copies with a lossless preview is still
   [#2043](https://git.oklabs.uk/BeFeast/okilum/issues/2043). Do not delete a
   working copy to make setup proceed.
6. Wait for **Bidirectional Sync active**. The first transfer starts automatically
   and the received vault opens when it is ready. There is no extra Enable,
   daemon setup, or promotion step. Save a small edit with **Ctrl+S**, check it
   on the other computer, then save a change there to check the return direction.

Opening Sync prepares the bundled Linux Syncthing only after checking that no
existing or unavailable runtime needs attention. If a connection is already
present, explicitly select **Use connection**; Okilum does not silently take it
over or start a second daemon. See [existing Syncthing](#existing-syncthing).

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

New managed source and receiving folders use Syncthing staggered versioning with
a 30-day maximum age and retain transport conflict copies. This is transport
history, not a guarantee of a snapshot of every local edit. Existing external
folder/version settings are preserved. Linux [in-note conflict review](sync-conflicts.md)
handles both copy kinds with side-by-side differences and Keep this / Keep that /
Keep both (#2044). The Settings → Sync inventory also offers explicit recovery of
interrupted decisions. Both originals are retained outside the index and appear
in Note history / Recover notes. Thirty-day history on every device with restore
from **Note history** remains tracked in #2045.

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

For three or more computers, do not assume receiving a vault also makes that
computer a catalog source, or that pairing with one computer automatically
connects every other computer. Symmetric catalog serving and a synchronized
all-device list are not delivered. Add the intended peer connections explicitly
through supported setup or existing Syncthing configuration.

A plain Syncthing server without Okilum is configured in Syncthing, with the
intended device IDs and folder shares on each side. Existing configurations can
be reused as above. Headless Okilum and an introducer-based plain-peer workflow
remain [#2048](https://git.oklabs.uk/BeFeast/okilum/issues/2048); current direct
pairing creates peers with `introducer: false` and `autoAcceptFolders: false`.
A server holding an ordinary synced vault can read it. Encryption for servers
the owner does not trust remains open; transport encryption does not solve that.

## Removal, ignores, and remaining work

The direct Settings flow does not yet offer fleet-wide device removal or
propagated revocation. To disconnect a plain/external Syncthing peer, remove its
folder shares and device connections in Syncthing on the relevant computers.
Stopping a local app or disabling its optional Sync UI is not remote revocation.
Already copied files remain on a disconnected or lost computer; they cannot be
recalled. A new invitation does not revoke an earlier pairing.

The [30 October decisions and 1 November clarification](https://git.oklabs.uk/BeFeast/okilum/issues/2039)
set the direction. These parts remain unfinished:

- [#2042](https://git.oklabs.uk/BeFeast/okilum/issues/2042): a device list inside
  the vault, all-to-all connections and propagated additions/removals.
- [#2043](https://git.oklabs.uk/BeFeast/okilum/issues/2043): preview and lossless
  first merge when the destination already has files. Local preview groundwork
  does not enable this flow. Unmerged PRs
  [#2511](https://git.oklabs.uk/BeFeast/okilum/pulls/2511) and
  [#2513](https://git.oklabs.uk/BeFeast/okilum/pulls/2513) are not shipped support.
- [#2045](https://git.oklabs.uk/BeFeast/okilum/issues/2045) and
  [#2048](https://git.oklabs.uk/BeFeast/okilum/issues/2048): the complete
  history and headless/plain-peer journeys described above.
- [#2046](https://git.oklabs.uk/BeFeast/okilum/issues/2046): one shared ignore
  policy for the whole vault. Today source enrollment seeds/verifies local
  `.stignore` rules and first receive applies approved ignores; Syncthing does
  not transport `.stignore`. Indexes, save temporaries and worktrees are excluded;
  `.claude/skills` can sync. Incompatible existing policy requires review.
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
