# Linux managed launch and configuration admission (#2582)

`okilum-sync-launch --state=<service-state> --instance=<lifecycle-uuid>` is the
single Linux managed service command, including autonomous systemd restarts.
It consumes the Linux lifecycle journal and prepared `syncthing` home, rather
than the unrelated macOS/Windows supervisor Store/Selection protocol. Packaging
ships the helper beside `okilum`; Enable copies it into durable service state
and pins its native identity and SHA-256. An expired AppImage mount cannot
redirect service launch to PATH.

## Identity and serialization

The preparation's `managed-launch/` directory lives outside vaults and indexes.
Its immutable reference pins the directory/random instance identity. Its binding
pins the canonical home/native identity, certificate, executable path/SHA-256,
and gate/process lock identities. Each folder binds its ID, root/native identity
and `okilum-write-authority::DurableBinding`. Reopening this saved reference
never registers or repairs missing authority. Enclosing authority promotion uses
the existing local-writer protocol. Unknown configured folders, changed roots,
missing/replaced/corrupt bindings, packages, helpers or definitions fail closed.
External homes have no managed-generation/hold ownership; their cooperating
configuration operations still require local writer admission.

The runtime gate serializes launch/enrollment/configuration across processes.
Inside it, configuration acquires a per-vault operation lease before registering
the folder, retaining it through all effects, authenticated readback, journal
acknowledgement and rollback. Synchronous nested operations inherit gate and
admission. `begin_drain` durably closes admission without needing the runtime
gate: an admitted operation finishes; later operations refuse; `try_hold` cannot
succeed before its lease ends. Neither the runtime gate nor REST readback grants
transport quiescence.

Source enrollment (including `.stignore`), remote sharing/membership, receive,
relabel/promotion, folder enrollment/reconciliation, pause/resume, removal's
pause restoration and readiness scans all use admission. Managed runtime-wide
policy and device-address changes require admission for every affected folder.
A closed vault refuses that shared effect without stopping the daemon. The
account-state enrollment lock replaces the old literal `/tmp` registry lock.

On start the helper holds the process singleton and runtime gate, validates
configured folders and attempts actual authority admission. Open folders retain
leases through authenticated startup/readback. Draining/Held/RecoveryPending
folders have `<paused>true</paused>` durably published to offline XML **before
spawn**. No cached `paused=false` is restored, even after authority release:
explicit admitted resume/reconciliation owns that decision. A concurrent drain
after launch admission waits for that operation; #2508 still owns the separate
transport drain. After readiness, unrelated folders transfer on the same runtime.

Every process gets a fresh random generation bound to the instance, executable
digest, PID and kernel start time. `live_generation` requires a live singleton
owner, kernel process/executable identity and ownership of the REST listening
socket inode. Authenticated REST startTime must match the saved ready generation;
stale files/recycled PIDs or a different process on the endpoint refuse. An
already occupied managed endpoint refuses startup before offline config effects.
Linux PDEATHSIG kills the runtime if its launch helper dies; systemd owns the
control group. Startup failures kill and wait the child. This identity evidence
is not a selected-folder quiescence receipt.

## Exact legacy migration

Only a lifecycle journal without a launch binding can migrate. Its legacy unit
must match the previously generated direct-Syncthing definition byte for byte,
including UUID, home and executable. Before stop/unregister, the service adapter
verifies the loaded fragment and absence of drop-ins. After verified stop, it
removes only that definition, binds current folders, then durably publishes the
helper journal/unit. UUID, certificate and home remain unchanged. Interrupted
migration retries under the lifecycle lock. Existing helper bindings never fall
back to migration when state disappears. An unavailable migration root names its
folder ID and path and asks to reconnect the disk or remove that Sync folder.
A missing preparation receipt after
launch registration refuses regeneration before config effects, including
bootstrap/Runtime retries. Partial registration requires explicit recovery; it
is never reset from an empty registry. The autonomous command also checks the
exact unit and loaded definition.

Migration is an explicit process-wide upgrade, not a selected-folder barrier.
Normal holds/restarts never stop the shared daemon to manufacture a proof.
Disable/removal retain their existing explicit service-ownership semantics.

## Package upgrades

Reconcile compares the packaged helper with the durable copied helper. A changed
helper uses the same exact-unit and loaded-definition verification before
stopping the owned service. With the runtime stopped, the process singleton and
runtime gate protect repinning the packaged Syncthing hash; folder authorities,
reference identity, certificate, home and lifecycle UUID remain unchanged.
Helpers are copied to content-addressed paths. The old helper remains intact
until and after the replacement journal is durable, so an interrupted copy or
start can retry without pointing the old journal at new bytes. A modified unit
is refused before any service effect. A missing unit during retry additionally
requires verified stopped/unregistered state. No package upgrade is a transport
quiescence receipt.

## Acceptance and remaining boundary

`scripts/ci/sync-managed-admission.sh` requires non-empty lifecycle/bootstrap/
runtime groups and three named acceptance tests under the #2499 fixture owner.
A separate `FolderController` process stalls at the real PATCH acknowledgement:
its lease blocks drain, readback completes after admission closes, a later
process refuses, and the operation succeeds after explicit release.

Two checksum-pinned v2.1.6 fixtures transfer both folders before a separate
holder dies without release. Following runtime crash, the exact unit ExecStart
fences that vault without a controller pause call; the other folder transfers.
A second crash/Runtime reconcile preserves the fence and changes generation.
Explicit recovery/release/resume then transfers the same withheld change.
Linux bootstrap retry uses the same command. Exact legacy migration and modified
unit/binding controls refuse changes. All service adapters and processes are
fixtures; real user services and external daemons are never modified. The driver
requires all three parent tests and checks for surviving fixture processes.
An admission-bypass negative control must fail the delayed-PATCH drain assertion.

This closes #2582 only. #2508 retains scoped runtime drain receipts, fenced
resume/recovery, stale-event rejection and its full acceptance. Non-empty direct
attachment remains refused. #2504 must compose local and transport proofs before
exclusive apply ownership.

## Composed exclusive ownership (#2504)

Exclusive mutation ownership of one vault is the composition already used by
first Sync (`direct/inventory/merge.rs`): `Authority::begin_drain` closes local
admission and `Hold::try_hold` succeeds only after every admitted writer
finishes. Only then does `ScopedHold::acquire` pause the selected folder and
accept the operation-labelled `FolderPaused` receipt from the live managed
generation. Publication calls `ScopedHold::validate` before every canonical
write. `release_and_resume` (or `resume_committed` / `cancel_unpublished`) is
the only exit; Drop, a closed window, holder death and runtime restart keep the
vault fenced until an explicit `Authority::recover` adopts the exact observed
state.

`issue2504_reader_writers_and_pinned_transport_share_one_restart_safe_hold`
exercises that boundary with production Reader writers in separate processes
and the checksum-pinned runtime together. An admitted `PreparedCopy` keeps the
holder draining and the folder unpaused until it publishes. Later saves and
creates in that vault are refused immediately, and their drafts are retained.
Under the composed hold, the competing inbound change is withheld while a second
folder on the same runtime and an unrelated vault keep working. After the holder
is killed, a restarted Reader process and a restarted `FolderController` resume
are both refused. A runtime crash and relaunch keeps the folder fenced, and the
second folder keeps transferring. Explicit recovery then publishes the same
withheld change, both retained drafts save, and the consumed recovery snapshot
cannot replay. Uncoordinated external editors remain unsupported. Non-empty
direct attachment remains refused.

### Scoped receipt refusal and recovery controls (#2508)

`issue2508_scoped_receipt_refuses_unproven_states_and_recovers_each_boundary`
exercises `ScopedHold` against the pinned runtime. Its refusal controls are an
external runtime, local and transport roots that differ, and a folder already
paused by someone else (no intent is persisted). They also include a completed
receipt offered as recovery proof for a later operation, and a configuration
changed under a live receipt, which `validate`, `release_and_resume`, recovery
and `resume_committed` all refuse. Fault injection rewinds the durable intent
to the boundary a dead holder last reached. For a lost PATCH (intent and cursor
durable, folder never paused), recovery re-submits the pause, withholds the
inbound change and publishes it after release. For a lost acknowledgement
(paused, no receipt), recovery accepts only this operation's labelled
`FolderPaused`. When a holder dies at quiescence, `resume_committed` refuses
another operation ID, restores the original label, and is idempotent. A fresh
operation cannot start while an unfinished hold is pending. The second folder on
the shared runtime is never paused.

### A missing folder root (#2582, Linux)

A configured root that no longer exists (moved, deleted or on an unmounted disk)
cannot be fenced by pausing. Pinned v2.1.6 recreates the root of every paused
folder at startup (`lib/model/model.go`, `initFolders`), and also the root of an
unpaused folder whose local sequence is zero (`newFolder`). It also drops a
folder's index at startup when the folder is absent from its configuration
(`lib/syncthing/syncthing.go`), so removing the folder would resurrect peer
deletions when the root returns.

The launch command therefore leaves a missing root untouched, takes no lease
for it, and lets the runtime start only when all of these hold:

- the folder is unpaused, so Syncthing only reports `ErrPathMissing`, publishes
  nothing and propagates no deletion;
- its local sequence was non-zero in the last live authenticated generation.
  Each launch records this in the private `indexed-folders.json`. Evidence only
  grows for a configured instance, because a paused folder reports no sequence.
  While the runtime runs, the launcher's supervision loop adds a folder as soon
  as it has index history, so a folder added or first filled after launch needs
  no restart. Creating a
  folder instance (`add_paused_folder`, under configuration admission and the
  launch gate) revokes it, so a re-added ID never inherits the old instance's
  history. No-op reconciliation (the 2-second Sync refresh) and a failed
  removal keep it;
- the configured path names exactly the saved (canonical) root. Any drift is
  refused, because an absent path cannot be canonicalized;
- a writer lease is admitted for its bound authority without the root, through
  `DurableBinding::admit_without_root` (same admission lock, Open state and
  shared writers lock as an ordinary writer). The launcher holds it from the
  decision through child startup, exactly like a present root's lease, so no
  drain can complete in that window. A root that returned during startup must
  then match the saved root identity, or the folder is paused before the
  leases are released.

Such a folder starts unpaused, because only a startup pause recreates a root.
Before the child starts, the launch command fences it in the offline
configuration. It replaces the folder's `markerName` with a unique
`.okilum-missing-root-<uuid>` and records the original in
`fenced-folders.json` before `config.xml` is written, so a launcher killed in
between still knows the marker it replaced. Pinned v2.1.6 `CheckPath` requires
`<root>/<markerName>`, and `CreateMarker` never creates a non-default marker.
So no directory that appears at the path, before or after child start and
including a foreign one with `.stfolder`, can be scanned or synced, and a
folder with history is never recreated. The original marker is restored only
when the saved root identity is back and the authority admits a writer through
an Open admission. That happens at readiness (a drain that began during
startup keeps it fenced), in the launcher's 2-second supervision loop, or
offline at the next launch. A held folder whose root returns gets its real
marker back and stays closed by the hold fence until explicit recovery.

Supervision watches every managed folder for the whole life of the launch
command, not only the folders fenced at launch. Whenever a saved root goes
missing, is replaced, or is configured at another path while the runtime runs,
including again after an unfence, it fences that folder again with a fresh
unique marker. The current marker is recorded first. A folder without index
history is also paused at runtime, because changing its marker restarts it and
pinned v2.1.6 `newFolder` would recreate a sequence-zero root, while
`restartFolder` into a pause never does. The record notes that pause, so the
unfence undoes exactly it; a user's or the hold fence's pause is never
recorded and never undone. If an unfence restored the marker but could not
drop its record, the next supervision round drops the stale record without
touching pause. A launch undoes a fence pause only while the folder still
carries a fence marker. Remaining window: up to one 2-second round between
a root disappearing and its fence. Supervision takes the launch gate only for
an actual transition, because configuration admission refuses a busy gate.

Every other missing root (paused, without index history, with an unfinished or
unreadable hold, or unbound) keeps the launch fail-closed, as before. The runtime
does not start, and nothing creates the root. Once the root returns, the normal
launch fence applies; a held folder starts paused until explicit recovery.
Runtime-wide effects (network policy, device addresses) hold the same
root-independent lease for an absent root, after checking that its configured
path names the saved root. Configuration effects on the missing folder itself
are refused.

Not covered: the managed-runtime upgrade (`initialize`) still refuses a missing
root with its explicit message. There is no live-generation evidence at that
point, and an unbound folder's hold cannot be checked. macOS and Windows are
unchanged.

Pinned-runtime controls:

- `issue2582_missing_root_never_stops_the_runtime_across_launches`: admitted
  no-op reconciliation keeps the evidence. Across two launches with one root
  missing, the folder is fenced and never paused, the other folder transfers,
  the root is never recreated and no deletion propagates. While the runtime
  runs, a foreign directory with a `.stfolder` marker at the path stays fenced
  and receives nothing. When the saved root returns, it is unfenced and syncs.
- `issue2582_launcher_killed_after_fencing_keeps_the_original_marker`: the
  launch command is killed after fencing `config.xml` and before the child
  starts, then started again. Once the root returns, `.stfolder` is restored
  and the folder transfers.
- `issue2582_root_lost_while_running_is_fenced_again_every_time`: twice in
  one runtime session the root is unplugged and a foreign Syncthing directory
  takes its path. Each time the folder is fenced again, never paused, and no
  byte moves in either direction while the other vault transfers; once the saved
  root returns it is unfenced and syncs. A folder with no index history that
  loses its root is fenced and paused, its root is not recreated, and on return
  it is unfenced, resumed, and syncs its first file.
- `issue2582_stale_fence_record_never_resumes_a_user_pause`: the launcher's
  state directory is read-only while a history-free folder is unfenced, so the
  record write fails. Once writes work again, supervision drops the stale
  record and leaves the folder as it is. In a second round, the user pauses the
  folder and the runtime restarts with the stale record on disk; the folder
  stays paused.
- `issue2582_root_swapped_before_start_never_syncs`: a foreign Syncthing
  directory takes the missing path between the launch decision and child start
  (fixture-only launch pause). No byte moves in either direction while the
  other vault transfers. When the saved root returns, it is unfenced and syncs.
- `issue2582_unsafe_missing_roots_fail_closed_without_recreating_them` covers a
  user-paused folder, a sequence-zero folder, and a dead holder's unfinished
  hold. For each, the launch is refused twice without creating the root. For
  the held folder, its returning root starts paused, the competing change is
  withheld while the other folder transfers, and the change publishes only
  after explicit recovery and resume.
- `issue2582_missing_root_admission_is_held_through_startup`: a fixture-only
  pause holds the launcher between its decision and child start while the root
  returns and a drain begins. The drain stays pinned until startup completes,
  and readiness keeps the folder fenced. The competing change is withheld while
  the sibling transfers, before and after the holder dies. The next
  generation fences the held folder by pause and publishes after recovery.
- `issue2582_redirected_missing_root_is_refused`: an indexed folder ID whose
  configured path is redirected to another absent path is refused twice
  without creating it. Once that destination appears with its own hold, the
  launch is still refused and nothing is published into it.
- `issue2582_folder_added_after_launch_gains_evidence_without_restart`: a
  folder enrolled after launch gets evidence from the live runtime. After an
  unmount, the next launch starts and fences it while the other vault transfers.
- `issue2582_readded_folder_id_never_inherits_index_evidence`: a removal whose
  REST call fails keeps the evidence. Re-adding the same ID from an empty share
  revokes it, and the history-free instance earns none; with its root removed,
  the launch is refused twice and the root is not recreated.
