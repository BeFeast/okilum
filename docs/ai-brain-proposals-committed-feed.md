# Committed proposal feed — issue175 / parent166

A2 connects actual new Inbox captures, saved Attention decisions and accepted
engine results to the internal A1 queue through a durable ordered source feed.
Enrollment exists **only in Rust fixtures**. There is no product API, CLI flag,
provider request, draft/adoption UI, config enrollment, live migration or backfill.

## Ownership and activation

Runner owns the publication sequence. It does not use timestamps, directory
order, UUID order, index events or SourceStore receipt filenames as ordering.
The five-field proposal identity remains unchanged. A feed additionally binds a
canonical epoch UUID, activation UUID and positive policy version, outside that
identity tuple. Only records created after the explicit activation are eligible.

SourceStore's existing exact JSON binding gains a required `required_proposal_feed`
key. Old implementations reject the extra key when opening a fresh handle.
Feed-aware writable handles retain the exact binding they opened and recheck it
under `writer.lock` before *every* source write, including receipt replay. The
same lock protects enrollment, so an upgraded handle opened before enrollment
cannot slip through a replay/write afterward. Feed-aware read-only retrieval
handles accept the extension but have no source-write or enrollment authority.

An already-open **old binary** cannot retroactively acquire this fence. Future
production enrollment must drain every old open writer and complete actual
binary compatibility checks first. This PR does not claim otherwise or enable
production enrollment. The required-binding API is an internal library seam,
not user-facing enrollment permission.

The recoverable activation transaction is: flush the quiet old pending batch;
write a preparing required binding; persist the same active identity in Runner
state; create the A1 ready header; mark the required binding active. Preparing
blocks writes. Restart resumes the same transaction and never relabels a legacy
A1 journal or silently initializes missing state under an already-active binding.
Active startup inspects the A1 header/cursor without recovery writes before the
ordinary Runner checkpoint. Corrupt/missing authoritative state, wrong owner,
epoch or policy refuses rather than silently disabling the feed.

The A1 schema string stays v1 with explicit optional `feed_binding` and exact
`event_bytes` receipt fields. Legacy unbound A1 fixtures retain their format.
An unbound store cannot be opened as bound, and an old strict reader refuses the
new bound shape. Header compatibility is checked before Running recovery.

## Producer checkpoint and immutable spool

Each producer stages a candidate in the **same Runner checkpoint** as its pending
canonical SourceWrites. It freezes original record/goal identity, received time,
path and revision plus exact source-operation references. SourceStore recovery
records retain original actor/source/result bytes; current edited Markdown is
never substituted as the trigger source.

Inbox and decision require create-only source operations. Result candidates
additionally require the exact result, stage and goal operations from their
accepted outcome batch. Original source metadata and the result/stage/goal graph
are validated against retained canonical requests/receipts. Source writes with
expected revisions are allowed for the dependent stage/goal, not the new result.
No result event is published until all three receipts are durable. Every crash
cut reuses the same candidate and operations. Result review edits, seen receipts,
ordinary notes, progress, tokens and proposal writes do not trigger this adapter.

The final canonical batch checkpoint assigns contiguous publication numbers and
stores the **exact serialized segment string** in durable Runner state. Typed
structs and ordered maps keep new serialization independent of serde_json's
feature graph. Once staged, bytes are never regenerated on replay or maintenance.

A separate bounded pump writes immutable files named by epoch and fixed-width
sequence in `proposal-feed-v1`. Each segment is at most64KiB. It uses file fsync,
no-clobber publication, and directory fsync before dropping staging or advancing
the spill cursor. Existing exact bytes are an idempotent replay; differing bytes
are refused and retained for recovery. Intake/startup never enumerate the feed
directory. Each pump spills/consumes at most8 events and reads only the exact next
sequence. Staged entries themselves remain in authoritative Runner state.

Optional spool/consumer IO failure cannot turn an acknowledged capture, decision
or result into failure or block later core writes. Each optional cursor checkpoint
retains its immediately preceding feed snapshot if persistence is uncertain;
it never reloads or changes the selected goal's core state. An immutable segment
may safely replay, and A1 is at most one event ahead of the source acknowledgement.
Actual disk exhaustion affecting canonical/state durability remains an ordinary
core durability failure; there is no unlimited-capacity claim.

## Consumer and maintenance boundary

The consumer verifies epoch/sequence, original source metadata and exact retained
receipts, then supplies the next event and its exact-byte SHA-256 to A1. A1 commits
the event digest, trigger, initial attempt and cursor atomically. Only durable
acceptance/exact replay advances Runner's acknowledgement. Changed bytes, even
with identical trigger identity, cannot masquerade as a lost-response replay.

A1's queue32 and16MiB limits stop **only the consumer**. The durable source backlog
has no proposal-specific quota and does not block unrelated Inbox, Attention or
engine-result writes. No garbage collection deletes feed or source receipts in
this slice. Maintenance must preserve candidates, exact staged/spooled bytes,
epoch/header, A1 receipts/cursors and existing context/source operational state.
The derived index remains disposable; its explicit read-only SourceStore handle
can read and rebuild an enrolled fixture.

A2 has no provider owner. Before generation is attached, replace per-pump A1
`open_bound` recovery with a coordinated store lifecycle: opening a consumer must
not interrupt a live provider attempt. This is a required B integration task,
not evidence that generation is already running here.

## Verification and scope

Tests cover producer replay/no backfill, source edits, saved decision vs seen,
all three result batch crash cuts, activation checkpoint cuts, epoch/owner/header
refusal before Runner writes, stale/read-only SourceStore handles, immutable byte
replay, optional IO/late acknowledgement failures, two-goal route preservation,
40+ core captures past queue capacity, and enrolled index read/rebuild. Fixtures
model process/persistence boundaries, not every filesystem's physical power loss.

Actual old-binary open/write controls and current backend/maintenance compatibility
are recorded separately with executable hashes. No live brain or service is
changed by these tests. No production activation is authorized by their success.
