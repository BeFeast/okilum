# Terminal proposal delivery outcomes

Issue #189 repairs native recovery of never-committed Snooze requests whose
absolute deadline has elapsed. A transport error is never proof of no write.

The service preserves the existing successful disposition receipt and adds a
separately typed `not_applied` outcome. It echoes the exact typed request, workspace,
canonical proposal path, UTC timestamp and `deadline_elapsed` reason. Only a
validated proposal owner and an operation without a staged/committed disposition
can receive this outcome. Exact committed replay remains first, including after
its deadline. Storage, source, ownership and identity uncertainty stay errors.

Before returning a terminal outcome the backend durably reserves its operation
and external source identity in the existing exclusive, bounded proposal journal.
The same original request replays the reservation; a changed payload or reused
identity can never become a write. The existing pending projection recovery
capacity is preserved. Publication failure produces no terminal acknowledgement.

A required source binding fences older readers/writers before the first terminal
reservation. A fence with no reservation is a valid interrupted upgrade; terminal
state without its fence is invalid before recovery or writes. Maintenance must
preserve both. No unrelated workspace is enrolled by opening it.

Native verifies all echoed fields before fsyncing its local `.done` receipt.
Original request and outcome remain retained. A terminal notice explicitly says
that nothing was applied; it never reports a disposition saved or goal completed.
Fresh inspection precedes any explicit new action with a new operation identity.
An old backend's generic error, unknown response, or local persistence failure
keeps the exact original request recoverable.

Validation includes competing identities, committed and terminal replay after
restart, faults around durable publication, capacity reservation, source fencing,
and actual native recovery against an isolated fixture. Alpha enrollment and
shared T3 execution are outside this change.

Only elapsed Snooze is terminal in this version. Changed/unavailable sources and
already-rejected proposals still retain the original uncertain operation. This
is not a general cancellation or discard capability.
