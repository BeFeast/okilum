# Lossless first Sync (#2043)

The goal joins two existing folders through the approved paired channel. The
ordinary empty-folder attachment API stays strict: constructing a preview or
adapter never enrolls its destination or publishes canonical bytes. Settings
keeps layout A and adds the review/confirmation inside the existing flow.

## Transaction boundary

The complete inventory binds the paired identities, authorized source/root,
canonical ignore policy, selected managed runtime and both byte revisions.
`Plan` classifies Same, Only here, Only there and Different; consent belongs to
that exact plan. The existing warning requires acknowledgement for little path
overlap. Unsafe/incomplete inventories refuse instead of truncating a preview.

After confirmation, a unique operation stores its immutable manifest and
content-addressed preimages under the private controller's `first-sync/`, outside
both vaults and their deletable indexes. Metadata and blob chunks are bounded,
checksummed and replay-checked. The remote computer verifies the exact same union
from both originals. Staging and preparation never publish a canonical file.

Local Okilum writers drain under the durable per-vault authority. The ordinary
destination retains its runtime's configuration/launch gate and proves that no
configured transport folder overlaps it. The configured source composes local
drain with a selected-folder transport proof. Other folders keep transferring.
Config admission refuses a retained gate instead of waiting while holding the
pairing database, so another paired request can complete or recover the merge.
First-merge requests never exchange device addresses or transport configuration.

For pinned Syncthing v2.1.6, `lib/model/model.go:512` waits for the folder runner's
`StopAndWaitChan` before cleanup; `CommitConfiguration` emits `FolderPaused` at
line 3035 after that wait. A unique temporary folder label binds that event to
this operation, including when an older event reaches its buffer late. The
pre-request cursor and intent are durable before PATCH. PATCH readback alone
never grants quiescence. The managed generation also binds the exact live owner,
child process, executable, REST socket and authenticated REST start time.
Already-paused startup recovery uses the launcher's recorded no-runner fence.
The original label is restored on authorized resume.

Both roots revalidate under those capabilities before the first canonical write.
Publication preserves originals in the private journal first. Replaced canonical
inodes move to unique ignored `.okilum-save-first-sync-*` names, and new bytes
publish with exclusive staging, NOREPLACE and directory fsync. Different files
keep a deterministic collision-safe extra version. A captured unexpected version
stops publication and remains retained; it is never silently overwritten.
Per-file publication gaps recover only against the exact saved original/target
bytes and root authority. Third versions require review.

Both exact union readbacks are durably acknowledged before transport/local writer
admission opens. A completed merge receipt is the only entrance that can enroll
this non-empty destination. Its folder and transfer intent have operation-scoped
ownership and retained witnesses; ordinary unknown non-empty attachment remains
refused. Completed journals remain durable evidence and are never automatic
authority to recreate a later deleted note.

Drop, disconnect, cancellation, an expired preview or a process crash never
release an unfinished hold. Recovery is an explicit operation against its saved
manifest and both preimages. Startup/ordinary refresh must surface pending work,
not silently apply it or enable Sync.

## Verification and remaining integration

The implementation branch adds explicit pinned-runtime controls to
`sync-managed-admission.sh`: a separate holder process proves scoped transfer
exclusion, unrelated-folder progress and recovery across holder/runtime restart;
a two-managed-instance fixture checks preview counts, no-consent/stale controls,
binary chunks, preservation of every original byte version, equal final maps and
both later transfer directions. Nonzero parent verdicts and fixture kill/reap
remain mandatory. These controls are under verification, not release evidence.

This document describes the backend work in progress. Settings integration,
interruption coverage, conflict discoverability, Mac/Windows implementation and
native beta acceptance remain part of #2043. No source merge or passing component
test alone closes its completion issue.
