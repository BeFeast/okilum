# Native proposal inspection and disposition

Issue #183 exposes the canonical #182 list/get/Reject/Snooze API in a distinct
Attention Suggestions group. Scope is native shell only. Provider generation,
Retry, Use draft/adoption and production enrollment are not enabled here.

Suggestions fetch one explicitly chosen owner at a time: Inbox (nullable owner)
or an exact goal ID, with bounded pages. Similar goal titles retain visible IDs.
Changing suggestion ownership or reading a proposal never selects a working goal,
creates an engine/task operation, or replaces source/Context/composer drafts.

Inspection separates generation attempt, optional generated content, unverified
status and disposition. Original trigger and canonical source links, frozen
provider/model/attempt identity, omissions, stale reasons and disposition history
remain visible. Rejected and future-snoozed items can be inspected through Show
rejected / snoozed; elapsed snooze becomes visible without generation. Unknown
future disposition states (including adoption) are displayed read-only.

Snooze requires an explicit absolute RFC3339 date/time and offset, converted to
UTC before retaining the request. The view displays the corresponding local
offset where the OS supplies one, otherwise labels UTC and unavailable local
zone honestly. Pre-submit deadline edits are retained per proposal in this
process; only submitted operations are promised across restart.

A separately typed native proposal-disposition outbox uses immutable requests
and fsynced publication under the existing workspace/client identity. Receipt
validation binds operation, proposal ID, exact canonical path, previous/new
revisions, actor, action and timestamp. The API does not return goal/workspace in
its receipt: those are bound by the retained request and guarded transport.
Unknown delivery retains the original operation. Recovery sends the same request,
including expired snooze deadlines; backend committed replay remains authoritative.
A typed durable terminal receipt may retire an expired never-committed Snooze
without applying it; see [the terminal contract](../ai-brain-proposal-terminal.md).
No generic successful response clears an outbox entry. Late receipts reconcile
their own entry without navigating away or replacing another form's values.

Validation in progress: source tests plus an actual linux-test-host native run against
an isolated exported A3 fixture, with exact GUI/backend hashes and cleanup.
Neither source tests nor backend-only compatibility imply full release readiness.
