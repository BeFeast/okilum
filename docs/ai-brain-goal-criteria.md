# Editing saved goal outcome criteria

The goal's Details surface offers **Edit outcome criteria** on a managed workspace
whose backend advertises `goal_criteria_edit`. It reads the current canonical goal,
lets the person edit saved descriptions and append new criteria, and saves through
the existing revision-aware source journal. Existing criterion IDs, order,
confirmation choice and unknown values stay fixed. New IDs are allocated once when
adding a row; only new unsaved rows can be removed or change confirmation choice.

The review accepts 1–64 criteria, nonempty descriptions of at most 8 KiB each, and
at most 8 MiB of complete UTF-8 source. Unsupported YAML gets an explanation to use
Source deliberately. A single ordinary plain `criteria:` subtree may normalize
quoting, indentation and comments; its unknown mapping values must remain equal.
Every byte outside that subtree, including other metadata and the complete body,
stays unchanged. Goal title, canonical status, prior Discussion turns, evaluations
and human evidence are not rewritten.

## Guarded command and admission

`goal_criteria_get` and `goal_criteria_write` require `ai-brain/workspace-v1` and an
exact `expected_workspace`. The write carries `goal_id`, the original exact
`SourceWrite` request and original `SourceSnapshot` base. It is a separate command:
an older backend rejects it rather than ignoring a new optional generic-write guard.
Generic `source_write` retains its existing behavior.

The service owner mutex and `Runner::with_goal` bind checks and writing to the
original goal. A first or unresolved write is allowed only with no current dispatch
or with `outcome_ready`, `cancelled`, or `discarded` and consistent saved stage
state. Terminal outcomes must match the stage, operation and engine binding.
Prepared, submitting, running, indeterminate and unknown states refuse the edit.
UI availability is explanatory; the backend checks again when saving.

Before flushing pending projections or checking first-write admission, the backend
looks up the exact SourceStore operation. A matching durable receipt is validated
against request, base, preimage, digest, path and outcome and may be replayed after
later source or stage changes. An unresolved intent does not bypass admission.
Unreadable or oversized journal data is an error. Without a durable receipt, prior
projections flush, current eligibility is checked, and SourceStore performs its CAS.
Stale source retains original/current/proposed conflict data; no automatic merge
child or fallback write is issued.

Completion is a derived projection. Both selected snapshot and goal list use the
same per-goal definition/evidence validity check. Editing criteria on a completed
goal can display it as blocked without rewriting its canonical completed status or
retiring historical evidence.

## Desktop retention and recovery

Before sending, the existing EditorRecovery store retains the exact source draft,
base and guarded command. No additional store or recovery schema is introduced.
Retries keep operation, goal and bytes. Guarded conflicts retain their pending
command through restart; resolving different bytes requires deliberate discard and
a fresh criteria review. Recovery may expose original/current/proposed Source for
inspection, but cannot silently turn the guarded operation into generic Source Save.

Cancel before Save writes nothing. Navigation and normal close/Quit prompt while
the transient form is open. Unsaved form crash recovery is not promised. After
Save begins, existing durable source recovery owns the operation. A local protection
failure preserves the original command in memory, blocks generic saving and normal
close, and offers explicit retry. A retained pending operation is recoverable even
if its view/process closes before acknowledgement.

After acknowledgement the desktop separately reads current canonical source. The
receipt proves the earlier save only. A later external edit is shown as current;
a missing source or failed readback is reported separately with readback retry.
Recovery of another goal's save keeps that source visible rather than presenting
its acknowledgement as an edit of the current selected goal.

An older shell may reject this pending command but preserves the recovery file.
An older backend rejects the unknown operation. Neither path migrates state or
falls back to an unguarded write.
