# LAN computer pairing and existing replica association

## Approved product decision

Oleg, 2026-11-01, recorded in epic #2039 and issue #2041: pair computers once
using a pairing URL. The existing computer generates the URL; the new computer
pastes it and supplies its name. The existing computer confirms that name and
computer identity. Only after confirmation does the new computer receive its
private authorized vault catalog. It selects a vault and a local destination;
an empty destination begins receiving automatically. Later attachments between
these paired computers need no new invitation. No service, account, T3 install,
public catalog, public rendezvous, or custom relay is involved. The first slice
requires both computers on the same LAN. The URL is not a filesystem path.
Unknown nonempty destinations retain the lossless preview/no-deletion gate.
There is no invitation-lifetime setting in the UI.

This supersedes the service/passkey product flow in `sync-pairing.md`. That
service implementation remains available to its existing independent consumers.
The earlier approved 2026-10-30 direct-peer topology and safe first-sync decisions
remain in force. A successful source build does not establish native acceptance.

## Application API

`okilum_sync_controller::direct::Controller` is shared by Linux and macOS.
`open(state, configs)` creates/loads only a private operational journal and device
identity. It does not listen, discover processes, start Syncthing or mutate its
configuration. State must be outside canonical folders and disposable indexes.
A lifetime lock prevents two controllers from owning the same journal.

- `inspect_existing(configs, vault)` authenticates the explicitly inventoried
  daemon, certificate, selected folder and canonical path using GET requests.
  Incomplete/unavailable inventory, ambiguous ownership and overlaps fail closed.
- `attach_existing(inspected, true)` re-inspects that scope, saves the explicit
  association, and makes that folder available for future owner-approved computer
  pairing. This association itself never changes peers, folder settings or files.
- `begin_pairing(name)` opens the application LAN listener and creates the URL.
  `begin_pairing_at` supplies an explicit bind/advertised IP for isolated fixtures.
  A computer whose paired computers already pin an endpoint on it invites at
  that same endpoint instead of a new random port, so every computer reaches
  it at one address (#2589).
- `refresh()` listens again on every distinct endpoint a paired computer pins on
  this computer, not only the latest invitation's, so computers paired through
  earlier invitations on other ports keep reaching it without pairing again.
  A pin is this computer's when it shares the address this computer last
  listened on and is not a computer this one joined (its catalog or a received
  vault). An address no longer assigned here is skipped; any other bind failure
  (a port taken by another program) is an error and never replaces a pin.
- `redeem(url, name)`, `refresh()`, `approve(request_id)`, `reject(request_id)` and
  `cancel()` implement the confirmation ceremony. Only incoming pending requests
  can be approved locally. The first authenticated requester claims an invitation;
  another identity or changed request cannot replace it invisibly.
- The approval response includes the authorized catalog in `Snapshot.catalog`.
  `catalog(peer_id)` subsequently refreshes it using the durable paired capability.
  Refresh retains the catalog. Authorization is the set of explicitly attached
  source vaults present at owner confirmation, never arbitrary filesystem paths.
- `attach(peer_id, vault_id, destination)` requires an authorized catalog entry,
  one authenticated existing daemon and the empty-folder/overlap gate before any
  remote share or local configuration mutation. It saves intent before external
  work, adds only the selected folder membership, preserves external device
  settings, applies reviewed ignores while paused and starts first receive.
- `refresh()` automatically reconciles owned initial receives. `Snapshot.preparing`
  remains true until guarded promotion finishes; Settings must continue polling
  while it is true. No additional Enable or promotion button is required.
- `set_configurations` accepts paths from the separately verified platform runtime
  bootstrap. An empty inventory is an explicit bootstrap-required error, not
  evidence permitting a second daemon.
- `shutdown()` is nonblocking: revoke all retained clones and shut down the
  application's listening socket. It performs no network request, filesystem
  operation or Syncthing lifecycle action. Last-controller drop also terminates
  its listener. Disabling Sync must revoke application NetworkAccess and call it.
- `has_paired_computers_or_vaults(state)` reads the saved journal without taking
  its lock or creating it. When it is true and Sync is on, the desktop app opens
  the controller in the background at launch (and when Sync is turned back on)
  and refreshes it like `okilum sync serve`, so paired computers are answered
  without opening Settings → Sync (#2589). Settings reuses that controller.

### Fleet removal (#2042)

`sync_computers()` combines paired identities with the members of associated
Syncthing folders. `remove_computer(device_id)` records a durable removal before
changing any folder. Exact before/after folder plans make a lost REST reply
replayable; removed computers keep their files and pause their associated copies.
Unrelated folders and external device relationships remain unchanged.

The paired channel is directional: both journals pin the inviter's listener.
Joiners pull pending removals from that listener during refresh, apply the
operation, then acknowledge it. Inviters never try to push to their own endpoint.
Offline deliveries and ACK loss survive restart. A departing introducer denies
application authority immediately, then waits for the other Okilum receivers to
detach its introduction provenance and acknowledge before pausing its transport.
This preserves the surviving receivers' direct relationships. Pending/offline
acknowledgements remain visible; files already copied cannot be recalled.
A removed pairing retains only
the credentials needed for its recorded removal replay, scoped pull and ACK;
it cannot read a catalog, exchange transport hints or grant new membership.
The introducer mesh carries folder membership removals to plain Syncthing peers
that trust the introducer with introduction removals enabled. No device list is
stored in the vault. Settings shows pending delivery until acknowledgement.

### `okilum sync` while another process owns Sync (#2589)

One process owns a computer's Sync journal (its lifetime lock). When that owner
is `okilum sync serve` (the always-on hub's service) or the app with Sync
started, it listens on `control.sock` inside the private journal folder (0700,
socket 0600). Other `okilum sync` commands of the same user find it there and
run each step through it, so `invite --approve-first` pairs a new computer
while the hub keeps serving. Output and exit codes are the same as when the
command owns Sync itself. Both ends check the other's user ID; there is no TCP
listener. Paths are made absolute before they are sent. A different control
protocol asks for a restart of the owner after updating. A socket left by an
owner that exited answers nothing and is replaced by the next owner. While the
app runs without a started Sync controller, commands still refuse with exit 3;
`--quit-app` quits it as before. The client and the owner must use the same
profile (`OKILUM_STATE_DIR`, XDG directories). Windows has no Sync commands.

`configuration_paths()` returns `Result<Vec<PathBuf>>`. Linux includes its existing
process/config inventory; macOS includes standard user configuration paths only.
These paths do not prove absence of custom/unknown macOS daemons.

## Wire and authority

The version-one protocol uses bounded length-prefixed JSON over direct LAN TCP.
Each frame contains a 96-bit random nonce and AES-256-GCM ciphertext; protocol
version and capability UUID are authenticated associated data. The encrypted body
is Ed25519-signed by a durable per-computer identity. Requests pin the intended
recipient, responses echo the fresh request nonce, and both directions verify the
expected signing identity. The URL pins the first computer's public key and holds
an independent random 256-bit invitation capability in its fragment. No REST key,
local path, source files, browser session, account credential or vault catalog is
in the URL. Treat the complete URL as sensitive; do not log or persist it in vaults.

The owner sees the new computer's signed name and full key fingerprint before
approval. A different computer cannot replace that request. Approval commits a
separate random per-peer capability before returning the private catalog. Later
requests require both that capability and the paired signing key. The invitation
cannot approve a second computer. Its authenticated original requester may retry
to recover a lost approval response during the original lifetime. Its durable
paired credential survives invitation expiry. Syncthing's own certificate-bound
TLS separately authenticates the actual file-transfer peer.

Implementation bounds: invitations last one hour, with no UI setting; at most
64 paired computers per controller; 100-byte names; 256 KiB maximum wire frame;
three-second connection/read/write limits; a single bounded connection worker.
Unknown versions, capabilities, signatures, recipients, fields and stale responses
fail closed. No unauthenticated operation lists vaults. Encryption/signature failures
close the connection without diagnostic credentials.

## Runtime and delivery boundaries

`daemon::CLIENT_VERSION` remains **v2.1.6**, the bundled, provenance/checksum-pinned
runtime. External reuse has a separate explicit compatibility allowlist containing
**v2.1.6 and v2.1.7**. Other releases, prereleases and legacy hubs cannot silently
become a selected client. Supporting external v2.1.7 does not replace the packaged
runtime, its provenance, installation gates or native evidence.

The empty-folder path starts **receiveonly**, keeps staggered versioning at 30
days and retains conflict copies on that new local folder. A durable ownership
UUID, initial paused-folder UUID label,
exact expanded configuration witness, canonical path and directory device/inode
bind ownership. The display label can then change through the witnessed name
intent described below. Only that new owned folder may become **sendreceive** after
the
guarded direct-peer readiness interval. Two fresh authenticated source observations
surround local scans; source generation/start time/configuration and counters must
remain stable, the initial remote sequence must be received, and both sides must
be settled with zero need, errors and receive-only local delta. Both 2.1.x peers
use file-byte counters; the legacy hub directory-byte adjustment is not applied.
The durable Promoting intent precedes PATCH; lost-ack recovery reads back only the
exact owned folder. If PATCH did not happen, restart requires fresh observations.
Legacy journals without an ownership witness never acquire promotion authority.
The approved receiving Syncthing ID is signed during pairing, displayed for old-
computer confirmation, and bound to subsequent share and readiness requests.
Unknown nonempty directories are rejected; no speculative merge or lossless-
preview implementation is provided. This does not bypass the native data gate.
Executed Linux two-daemon evidence and exact source hashes are recorded in
[sync-direct-bidirectional-evidence.md](sync-direct-bidirectional-evidence.md). Existing source settings and global
network/discovery/relay/lifecycle settings are retained. Syncthing REST has no CAS;
external administrative writers still require coordination.

macOS fresh-runtime inventory and signed-package preparation are composed in
`sidecar/macos/bootstrap.rs`. The installed signed beta prepares on explicit
Share or Request connection, checks the claimed helper and authenticated REST
identity, then enrolls or receives the selected vault. It never starts a fallback
daemon. macOS beta 29262 includes bundled-app executable authentication (#2676);
[#2489](https://git.oklabs.uk/BeFeast/okilum/issues/2489) records actual signed-beta
native source/receiver UI, file and video evidence. Native compile tests alone
remain insufficient to establish that flow.

Remaining boundaries include symmetric catalog serving from a formerly receiving
computer and general catalog authorization changes. Ordinary-source sharing below
grants its own additions; fleet removal is described above. A running source
controller serves additional authorized vault selections without a new URL, and
paired identities/capabilities are durable.

All acceptance uses isolated temporary states/configs and disposable daemons.
Never mutate the live brain, working configuration, peers or installed app, or
start a second daemon. A failing harness must preserve its evidence and clean up
its owned processes before another attempt.

## Ordinary Linux source sharing (#2367)

Settings → Sync lists known and currently open full vaults. **Share with my
computers** enrolls an ordinary vault in the already selected bundled runtime;
**Shared with approved computers** records completion. **Choose existing synced
vault** retains its existing read-only association flow for external replicas.
`Controller::share_vault` itself never prepares, launches, registers or stops a
daemon. The platform executor prepares the managed connection on explicit Share
before calling this source-enrollment API.

`share_vault(prepared, vault)` authenticates the existing private preparation
receipt, pinned 2.1.6 executable digest and selected daemon/certificate before
mutation. It records a stable random folder ID, root device/inode, ignore bytes
and creation intent in `direct.json`, outside both vault and index. It creates
only that new folder paused, records its expanded configuration witness, then
unpauses and atomically saves the association and catalog grants for **all already
approved computers**. Later approved computers receive the source through the
existing pairing catalog. Repeated sharing keeps the ID and configuration;
interrupted creation/unpause can resume only with the original identity/witness.
This extends the older confirmation-time-only catalog authorization above.

The new source excludes `.okilum-index` and `.okilum-save-*` (the Reader's canonical
per-machine patterns), `/.claude/worktrees` and `/worktrees`. `.claude/skills`
remains shared. An absent `.stignore` is created without replacing another file;
an existing compatible policy is preserved byte for byte, including custom rules
and line endings. Missing required exclusions, includes, negations or symlinks
stop enrollment for explicit policy review. A changed policy never silently
replaces the recorded bytes. No canonical notes are deleted or overwritten by
source enrollment. The receiving controller retains the existing receive-only
readiness/promotion gate and version history.

Unknown nonempty destinations remain stopped before remote sharing or local
folder creation. They require the distributed lossless preview tracked in #2043.
The isolated `source_sharing::` integration tests in `direct_bidirectional` use
two actual managed preparations and the pinned unchanged 2.1.6 binary. PR CI's
`sync-source-sharing` job explicitly runs all five ignored tests and verifies
each name plus `5 passed`, including peer-address, skipped-share, nonempty refusal and receiver
label migration/recovery controls.
Native idunn/muninn acceptance belongs to the Sync owner after beta delivery.

### Disposable CI fixture staging (#2527)

Both source-sharing and network-policy jobs obtain the pinned runtime through
`scripts/ci/sync-fixture-stage.py`. The helper verifies the archive checksum and
the extracted executable checksum before publishing. Each writer uses a unique
temporary file in the destination directory, flushes and fsyncs its contents,
closes the writable descriptor, then atomically renames and fsyncs the directory.
Closing before rename prevents concurrent exec from observing a writable inode.

Verified executables are read-only and cached at
`~/.cache/okilum-sync-fixture/<binary-sha256>/syncthing`. This also isolates new
trials from older jobs extracting over the legacy version-named directory. No
job truncates an existing executable, and concurrent cold-cache downloads never
share a partial archive. Derived fixture caches remain disposable.

The staging unit controls hold the legacy `tar -xzf` path open during exec to
reproduce ETXTBSY, then check atomic replacement of a running ELF, exec exactly
at publication, concurrent cold-cache writers/downloaders and checksum failures.
The real lifecycle driver publishes and executes four replacements concurrently
while two pinned daemons stay live in its own cache. Normal exit, panic, SIGKILL
and orphan-guard controls still require all owned fixture processes to disappear.

## Guided Linux first run (#2368)

The welcome screen's **Get notes from another computer** opens Settings directly
at the receiving pairing step and persists Sync module consent. Opening Sync (or
starting either pairing action) prepares the bundled Linux runtime implicitly
only when the complete authenticated inventory has neither an existing candidate
nor an unavailable configuration. The bootstrap retains its final process/config
rescan, checksum, private state and identity guards. A discovered external instance,
even a single one, requires explicit **Use connection**; it never authorizes a
second daemon or an implicit takeover. Ordinary Refresh never bootstraps.

Action completions publish the new runtime and controller snapshot immediately.
An invitation, pending confirmation, initial receive or paired computer keeps the
revocable background refresh alive. The current page follows layout A (#2615): status, Computers, Vaults, and
collapsed Advanced. Pairing steps live in a native modal with contained focus;
incoming approval is also visible in Reader and other Settings sections.
Technical errors stay behind Details, with the complete redacted chain logged
as sync_action_failed. See [Sync settings](sync-settings.md).

After approval, the receiving computer chooses a vault and a local folder. The
new-folder dialog proposes a safe single component of the vault name under HOME;
the existing-folder picker remains available. Creating that explicitly chosen
folder is staged until Start first sync, which reuses the new-vault
empty-directory guard for each chosen vault. The controller still refuses
unknown nonempty destinations and retains its receiveonly/readiness/promotion
rules. A vault selected in this session opens automatically after the controller
reports initial receive complete; refreshes never open unrelated attached vaults.
Disabling Sync revokes work and outstanding picker results.

Shell tests replace only the blocking worker boundary and exercise real welcome
clicks, module persistence, action completion, timer-driven incoming/confirmation
publication and result bounds in the Settings viewport. Separate inventory-policy
tests retain positive controls for implicit preparation and negative controls for
existing/unavailable instances. Native beta recording remains separate acceptance.

## Receiver display names and runtime publication (#2400)

A received folder takes its display label from the selected computer's authorized
catalog. Creation still uses a private UUID label while paused, so a display name
cannot become a lost-create ownership credential. After saving the complete
configuration witness, the controller records the catalog name as a durable
label-only intent, PATCHes only `label`, checks the full readback, and advances the
witness. An interrupted label acknowledgement accepts only the old witnessed
label or that recorded name; path, membership, version history and promotion
checks remain in force.

Reconciliation also relabels owned 7504 receivers, including already bidirectional
ones. This migration changes no path, folder type, devices or vault files. Legacy
receivers without an ownership witness retain their existing hold. Local recent
vaults and window titles read the durable received-name projection without
starting Sync or writing a vault name file; an explicit synced display-name file
keeps its existing precedence.

On Linux, a verified managed preparation says **Synced by Okilum**; an attached
external runtime says **Uses your existing Syncthing**, with **Paused** appended
when appropriate. The runtime inventory is read again before publishing an action
result, so sharing/attaching publishes its current vault count along with the
updated controller snapshot. Peer connectivity remains separate evidence (#2397).

### Departing introducer handoff (#2042)

A newly promoted receiver may not yet have received the introducer's next
Syncthing cluster config. Before removing that introducer, its private removal
journal freezes the final membership and dial hints of each authorized common
vault. Both the initiating receiver and other paired receivers acknowledge only
after installing their scoped surviving relationships. Missing transport devices
may be added only in an Okilum-managed runtime; existing device settings remain
untouched. The snapshot carries no pairing keys, device roles or folder settings,
and is never stored in the vault. Replays require the same removal identity and
snapshot. Unrequested vaults and snapshots from a different computer are refused.

The departing introducer denies ordinary application authority immediately and
pauses its copy after those acknowledgements. Invitation URLs, approval and
existing pairing credentials keep their established formats. This handoff is part
of the new removal operation; old journals default to no pending handoff.

An introducer with a plain Syncthing member cannot prove that member retained its
surviving introductions. Root removal therefore stops before changing folders
and asks for an explicit handoff on those computers. Leaf removal through the
plain peer's introducer remains supported. Automatic root departure for plain
peers needs a separate design/host-authority decision; no host API access is
inferred from consent to install a shared-ignore include.
