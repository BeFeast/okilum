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
back to migration when state disappears. A missing preparation receipt after
launch registration refuses regeneration before config effects, including
bootstrap/Runtime retries. Partial registration requires explicit recovery; it
is never reset from an empty registry. The autonomous command also checks the
exact unit and loaded definition.

Migration is an explicit process-wide upgrade, not a selected-folder barrier.
Normal holds/restarts never stop the shared daemon to manufacture a proof.
Disable/removal retain their existing explicit service-ownership semantics.

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
