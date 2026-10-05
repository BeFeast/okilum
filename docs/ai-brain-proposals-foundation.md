# Internal proposal foundation — issue171 / parent166

A2 now supplies the dormant [committed feed adapter](ai-brain-proposals-committed-feed.md).
The description below records the original A1 boundary; production enrollment,
generation and canonical draft/adoption remain disabled.

This is implementation sub-slice A1 of the proposed
[proposal contract](https://git.oklabs.uk/BeFeast/tessera/pulls/167).
It is a private, deliberately unused library module. No Runner, service startup,
provider, source projection, API command, capability or GUI opens the store.
Nothing is enrolled or generated in an existing workspace by this change.

## Implemented boundary

`Identity::id` hashes the five contract fields in fixed order using UTF-8 decimal
byte-length prefixes and SHA-256. UUIDs must use canonical spelling; policy is a
positive integer. Source revision is a bounded opaque string. JSON serialization
is never an identity input. Tests include independent Python-generated fixed
hashes for normal and Unicode/delimiter input plus an unframed collision control.

A future committed-source adapter must supply a brain-owned, strictly ordered
stream containing only durable Inbox captures, saved Attention decisions and
engine results. It must verify original canonical ownership/path/revision/time
before calling this module. This adapter is **not implemented here**: the typed
`CommittedTrigger` is a fixture/internal seam, not proof that arbitrary callers
have established canonical durability. Source intents, edits, partial tokens,
progress, index events and proposals must not enter that stream.

`initialize` explicitly persists the starting cursor before accepting events;
`open` refuses absent stores and cannot silently enroll/backfill a workspace.
Only the next contiguous event advances the cursor. Event receipt, source metadata,
initial attempt and cursor are atomically written in one bounded operational
journal under a process lock, outside the derived index. Exact replays retain the
same proposal and attempt. Changed event binding, goal, path or original time is
rejected. A policy/revision change cannot regenerate an already committed record.
History at/before activation and sequence gaps are errors.

The queue admits at most32 queued attempts per brain. A full queue returns backlog
with the previous cursor; the same unqueued event can be supplied after capacity
becomes available. A duplicate event at the next sequence can advance the cursor
without allocating another queue entry. The entire retained journal is bounded
at16MiB; reaching that bound refuses the mutation before advancing the cursor.
Every commit also reserves the extra state-name bytes needed for running-to-
interrupted recovery. This includes duplicate event receipt admission while an
attempt runs; otherwise a valid near-limit journal could become impossible to
reopen. Exact-boundary tests prove refusal without mutation at MAX-2 and successful
recovery from MAX-4 to MAX bytes. Receipt compaction is deferred, so callers must
treat the storage bound as explicit backpressure.

The first transition to running freezes non-secret provider/model references and
an exact input hash durably and returns one dispatch opportunity. Replaying the
same transition never grants a second one, even if its state is still running.
There is at most one running attempt per brain. Startup recovery durably changes
running to interrupted without another attempt, requeue or network retry. It is
idempotent across subsequent opens. No explicit Retry is implemented in A1.

Writes use a temporary file, file fsync, atomic replacement and directory fsync.
An uncertain persistence error poisons that instance: reads/mutations refuse until
it is dropped and reopened. This prevents an unpersisted in-memory success or
uncertain replacement from falsely acknowledging a cursor/dispatch. Recovery must
also commit successfully before returning an opened store. Fault-injection tests
cover both sides of replacement for enqueue, lost dispatch acknowledgement and
interrupted recovery; they model process/persistence boundaries, not hardware
power-loss behavior of every filesystem.

## Deferred work

Canonical draft/disposition projection, adoption coordination, retry/disposition
operation IDs, queue-to-provider integration, source stream ownership and cursor
enrollment, bounded citation capture, model generation, native wire commands and
forms, and preserving maintenance enrollment all remain later slices. The internal
A1 journal is not exported as canonical knowledge and makes no user-visible
proposal/disposition or exactly-once remote execution claim.

No release/install or live data mutation is part of this PR. Before connecting any
production caller, the source boundary, older-binary refusal and actual writer /
maintenance-reader compatibility must be implemented and reviewed per the parent
contract. Existing runtime/storage and externally controlled T3 development remain
unchanged.
