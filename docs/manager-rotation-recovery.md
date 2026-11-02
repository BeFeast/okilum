# Local Manager rotation recovery (#1473)

This source slice adds an explicit Linux Manager option, **Rotation policy…**.
It configures local missed-window recovery and prepares a reviewed draft. It does
not create a scheduled task, launch or settle a Manager, send a message, or establish
live/native acceptance of automatic rotation.

The form starts disabled. The user supplies an IANA timezone, two distinct local
times, and optional exact reviewed draft text. **Save policy** is the only authority
allocation operation. Enabling sets the existing PolicyStore cursor to the consent
instant; it does not backfill earlier windows. **Review missed window** explicitly
claims the latest eligible local window and mounts the existing reconciliation
coordinator. Pending questions, unknown question status and an active Manager
operation hold this action. No timer performs these operations.

Opening the form restores an existing pending claim locally. Without retained
reviewed text, Resume remains disabled. Resume preserves authored text, attachments
and typed metadata, and uses the existing durable composer barrier. Defer and Cancel
change only the claim. Configuration edits must be saved or explicitly reloaded
before reviewing the saved policy. Reload replaces only the configuration form;
it does not replace the Manager composer.

## Durable canonical authority

The base is the existing `ProjectView.state` durable application-state directory,
never a vault guess or the deletable index. For the exact canonical origin, project
ID and Manager thread ID, compute SHA256 over the UTF-8 JSON array
`[origin, project, manager]`. The relative namespace is:

```text
manager-rotation-v1/<64 lowercase hex digits>/
  1.owner
  2.owner
  ...
  identity.lock
  checkpoints/1.rotation
  checkpoints/2.rotation
  ...
```

Every immutable owner record contains and validates the complete identity; a hash
collision or misplaced namespace cannot select another owner. New namespace
directories use mode 0700 and owner records use mode 0600. Symlink namespaces,
noncanonical/gapped record names, oversized records, unknown JSON fields, invalid
history, and mismatched owner/checkpoint data are refused.

Owner log sequence and policy-owner lifecycle revision have separate meanings.
For a new identity, explicit configuration writes sequence 1, lifecycle 1,
**Preparing**; configures the existing PolicyStore for that exact owner; then
writes sequence 2, lifecycle 1, **Ready**. A later explicit configuration allocates
lifecycle 2 and uses two more immutable log records. Checkpoint claim/ack/CAS
revisions never allocate lifecycle authority. Lifecycle is strictly monotonic
within the full identity namespace. Explicit configuration for a replacement
identity allocates authority in that identity's namespace; returning to a previously
configured identity preserves its recorded history. Restart, opening, paint and
identical visibility notifications never allocate revisions.

One create-only identity lock serializes Configure with the coordinator's guarded
initial/fresh reads and final checkpoint load, lease check and existing CAS. The
lock is held through synchronous persistence, never across an await, UI callback,
or composer barrier. Record publication flushes a private temporary file, hard-links
it create-only to the next sequence, then flushes the directory.

A missing owner record alongside a checkpoint cannot synthesize authority. A
Preparing record, corrupt/replaced identity, mismatched policy or interrupted lock
holds recovery instead of guessing or overwriting. Interrupted lock cleanup is not
automatic. The complete prior records and draft bytes remain available for explicit
recovery. A valid Preparing allocation can be superseded only by another explicit
configuration CAS; reads do not complete it.

## Independent UI access and draft durability

Menu construction captures full identity, view generation, native connection epoch
and the exact client Arc. Click validates them again, together with current access
and physical visibility. A stale menu cannot retarget a rebound view. Save/Review
also revalidate their current target and use cancellable request leases.

Close, physical hide, rebind and access/connection lifecycle invalidation revoke
pending requests before their completion can publish UI state. These leases remain
independent of the persisted policy-owner revision. The original typed/Discuss
state and snapshot/barrier/checkpoint APIs are unchanged. Reconciliation acknowledges
only after `Ok(Ok(()))` from the existing durable writer and fresh exact owner,
checkpoint and snapshot validation under the identity lock. `draft_flush` is never
an acknowledgement.

Offline source fixtures cover explicit Save/Review, read-only restart, Preparing
and corrupt/missing authority refusal, stale config/owner CAS, Configure versus a
held acknowledgement, actual coordinator persistence and authority replacement,
stale menus, and close/hide/rebind cancellation. Compilation and fixture execution
require a separately granted focused canonical turn; source is not runtime proof.
