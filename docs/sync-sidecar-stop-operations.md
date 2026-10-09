# Revision-bound supervisor stop operations

Design slice for #588, pending one review before implementation. This changes no
running service or journal format today. It covers lifecycle Disable/Remove and
`update::Stop`/`Rollback`; native ownership/signature/discovery and complete Sync
acceptance remain separate gates. See [the lifecycle contract](sync-sidecar-lifecycle.md).

## Authority and record

Keep one private, per-instance transactional store outside installation/vault/index.
Replace the controller-lifetime `LockedJournal` with short exclusive transactions
opened against the same verified directory identity. No controller waits for IPC
while holding this lock. The supervisor uses that same lock, not a second mutex.

The version-2 `sidecar.json` envelope atomically owns lifecycle **and** update state:

```text
schema: 2
journal_epoch: random non-nil UUID (created once, retained across restarts)
revision: u64 (starts at 1; checked increment, never wrap/reset)
binding: complete existing Binding
intent: Enabled | Disabled | Removed
update: null | { update_id, previous, candidate, phase, rolled_back }
stop: null | { operation_id, authorized_revision, scope, reason }
```

`scope` includes installation, instance and the independently verified supervisor
generation. `reason` is Disable, Remove, UpdateStop(update_id), or
UpdateRollback(update_id). The authority token is the entire tuple
`(journal_epoch, authorized_revision, operation_id, scope, reason)` plus Binding.
`authorized_revision` equals the envelope revision when armed. It is not a PID,
a wire claim, a public discovery record or a replacement for native peer checks.

Every authoritative mutation increments revision, including Enable, a new intent,
update selection/phase changes and operation completion. It clears the old stop
or explicitly arms a fresh operation at the new revision. Removed is terminal.
Missing, corrupt, unknown-schema, replaced-directory or revision-overflow state
fails closed.

**Retry versus new action.** A request is a retry only if the stored `stop` is
non-null and the request's whole token `(journal_epoch, authorized_revision,
operation_id, scope, reason)` equals it byte for byte, with the envelope revision
still equal to `authorized_revision`. A retry reads and reuses that operation
without rewriting it or bumping the revision. Everything else is a new action: it
commits a new revision and mints a new `operation_id`, even when Intent, Binding,
scope and reason match an earlier operation. Callers never infer "retry" from
Intent; they either hold the token they armed or read the stored one under the
lock. A token that matches in every field but `operation_id`, or whose revision
is no longer current, is refused as stale rather than adopted.

**Revision exhaustion.** Reaching `u64::MAX` is not a realistic runtime event
(one mutation per nanosecond takes centuries) and migration cannot cause it: it
runs once per instance, writes revision 1, and the v2 envelope is the sole
authority afterwards. If it happens anyway the instance fails closed: no stop is
armed, no native effect runs, status reports `RevisionExhausted`. Nothing resets
the counter automatically. Remediation is an explicit operator repair: with the
supervisor verified stopped, the repair command writes a fresh envelope with a new
random `journal_epoch` and revision 1 and the same Binding/intent. Every token
from the old epoch is then refused, so reset cannot revive an old authorization.
Tests cover the overflow refusal and the epoch-change repair.

## Lock / effect sequence

1. **Prepare under lock.** Validate Binding, current intent/update phase and native
   generation. Persist and flush the requested intent/phase and fresh stop token
   in one atomic replacement. No native effect follows a failed flush. Release
   the lock before IPC. Preparation does not launch, register or generate identity.
2. **Authenticate and authorize.** Carry the token in protocol-v2 Stop. The server
   authenticates the captured peer, then acquires the instance lock using the same
   absolute deadline as the exchange. Load authoritative state; require exact
   Binding, epoch, current revision, stored token, reason/phase and generation.
   Missing/mismatched authority denies Stop, including repeated requests.
3. **Stop with the lease held.** Retain the exclusive transaction through bounded
   owned-tree stop/reap, so concurrent Enable cannot invalidate authorization
   during the effect. Never call back into a controller or another lock holder.
   Release the lock before sending a response. Echo the full token as well as
   request ID/scope; cache completion only for that exact token and generation.
   No durable completion receipt is inferred from an in-memory cache.
4. **Revalidate before the next effect.** The controller receives a correlated
   Stopped reply, reacquires the lock within the original budget, and compares the
   same token/current revision. While holding it, revalidate native ownership and
   perform unregister (Disable/Remove) or the permitted update phase transition.
   Then atomically clear the operation and increment revision. A stale reply has
   no authority to unregister, select, start, or overwrite newer state.

The lock is released for IPC waiting, **not** between final authorization and its
native effect. Native-effect adapters must not recursively acquire this lock.
If a synchronous OS call outlives the budget, an admitted worker retains the lease
and captured resources until terminal return; caller timeout is not permission to
release them. Lock contention/deadline, Stopping, lost reply, write error or unknown
exit leaves durable work pending. There is no retry loop that extends the budget.

A crash after an effect but before its completion commit replays the same stored
operation under ownership checks. Absence of an endpoint/task or a recycled PID
is not proof of tree exit. If trustworthy completion cannot be reconstructed,
recovery stays pending; this design does not invent a successful receipt. A fresh
supervisor generation cannot accept an old operation; a recovery transaction must
verify new native ownership and arm a new token while preserving terminal intent.

## ABA example

| Event | Durable state | Consequence |
| --- | --- | --- |
| Disable A | revision 41, Disabled, stop A/41 for generation G | Controller A releases lock and waits. |
| Enable B | revision 42, Enabled, stop null | A is invalidated; registration/start uses verified ownership. |
| Disable C | revision 43, Disabled, stop C/43 | Same Intent and Binding as at 41, different authority. |
| Delayed Stop A or reply A | still revision 43 | Server refuses A before stopping; controller refuses A before unregister. |

If the server acquired the lease at 41 first, Enable waits until the stop effect
finishes. Enable can then commit 42, but A's later unregister still fails revision
validation. Equality of Binding + Disabled alone would incorrectly pass both cases.

## Update Stop and Rollback

Update state moves into the same envelope; `update.json` is no longer a second
writer or a source of Enabled intent. UpdateStop requires the matching update_id
in Stop; UpdateRollback requires that update_id in Rollback. Both follow the same
prepare/unlock/IPC/relock sequence. After confirmed Stop, changing Stop to Select
consumes the operation in a new revision. Rollback must reverify/select the saved
previous runtime under current authority; restart is allowed only if current
lifecycle intent is still Enabled. Disable/Remove supersedes an in-flight update
stop by committing a fresh revision/operation. A stale updater cannot restore
Enabled, select/start its old candidate or commit its old phase. Each subsequent
phase reacquires authority; no obsolete in-memory Update is saved wholesale.

## Explicit migration of old journals

Read-only snapshot of a legacy `{binding, intent}` record remains read-only. It
cannot mint a revision or authorize protocol-v2 Stop. No journal remains inert:
only explicit Enable may create one. Before the first mutating reconciliation of
an existing legacy instance, take the verified instance lock and read both legacy
`sidecar.json` and optional `update.json` with the existing strict parsers. Reject
corruption, unknown fields, an update without lifecycle, or different Bindings.
The lifecycle intent is authoritative; never import the update's stale Enabled.

Build one v2 envelope with a fresh epoch, revision 1, identical Binding/intent,
preserved update payload/phase/rollback flag and a fresh update_id if present.
Set stop to null: legacy Disabled/Removed or Stop/Rollback is preserved intent,
not a fabricated stop authorization or successful receipt. Atomically replace and
flush **sidecar.json** before any effect, then separately arm a verified operation
in revision 2 or later. Migration itself does not stop/start, adopt a process,
change device identity, reset Removed or make an update restartable.

Do not attempt an atomic two-file rewrite. The v2 envelope is the sole authority
once committed; leftover legacy update.json is ignored and can be retained as
recovery evidence. A crash before the replacement leaves legacy authority; after
it, recovery uses only the v2 envelope. Flush failure permits no OS effects even
if replacement became visible. Older binaries' strict parser rejects the new
sidecar envelope: downgrade is an explicit recovery operation, never fallback to
legacy update.json or automatic reconstruction/reset of an epoch/revision.

## Wire compatibility and acceptance before wiring

Bump IPC VERSION to 2 and strictly validate command-specific fields: Stop requires
one token, Status carries none; no permissive token defaults. A v1 peer/response,
nil identity or unexpected token is refused before effects. Native identity and
shared deadlines remain mandatory. A revision does not authenticate the caller.

Tests must cover failed durable writes with a positive stop control; lock release
before IPC and bounded server contention; both ABA orderings above; lost reply and
same-token retry; superseded update Stop/Rollback; deadline while a worker retains
its lease; old/new generation refusal; crash before/after native effect and flush;
legacy absent/Enabled/Disabled/Removed and every update phase; partial migration,
corruption, mismatched Binding, overflow and old-binary rejection. Real native
journal durability and supervisor exit remain required, not inferred from fakes.

## Implementation status

Landed as library code, not yet wired into Reader, installers or a native supervisor:
`sidecar::authority` (envelope, tokens, transitions, migration; #888), the Unix
`sidecar::store` (per-transaction lock with an absolute deadline, exact-next-revision
commits, explicit idempotent migration, operator epoch repair; #894), IPC v2 with
the token in Stop and an exact echo (#897), `Controller` on transactions (#901) and
`update` Stop/Rollback/phases on the same envelope (this slice; `update.json` is now
read only by migration). Platforms without an authenticated IPC channel (both native
adapters today) stop natively and unregister under the same lock instead of sending
a token. Still open: a native `OwnedRuntime` that takes the lock in `authorize_stop`,
authenticated generation discovery, a Windows store with DACL-checked file
replacement, and real native crash/durability and supervisor-exit acceptance.
