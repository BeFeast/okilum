# Use a saved Maestro observation in Context

Open **Show observation history → Open saved observation**, inspect its Source,
and choose **Use in Context**. The exact saved file joins the selected goal's
local unreviewed Context draft. Existing choices, pins, guidance, query and full
scope survive. Build and review explicitly; the action does not pin, save a
packet, prepare a stage or call a provider.

An inactive historical link remains eligible. Its saved status and timestamps
remain historical, with `verification: unverified`. A provider status is not
proof of a merged head, passing build or fulfilled goal criterion. Disconnection
neither erases the evidence nor makes it current.

## Admission and ownership

Only the canonical `ai-brain/v1` Maestro `Observation` payload is admitted at
`records_dir/maestro-observation-<id>.md`. Its brain, UUID IDs, selected goal,
observed/remote timestamps, paused flag and structured issue must be valid.
The selected goal's history must uniquely own that observation under the same
link and issue number, and its source-path mapping must match the exact file.
Missing or contradictory history and recovery-required state refuse the action.
Approval receipts containing `review`, `decision_receipt` or `execution_status`
are refused even though they share `record_type: maestro-observation`.

The action captures historical instance, project and repo provenance together
with the observation/link/goal/path/issue tuple and active state. It checks that
tuple again against the current UI snapshot before changing either draft.
Current provider connectivity and a newer active link do not replace the saved
historical identity. These historical fields are not fabricated inside the
canonical Observation or written back to the file.

The shell and backend share exactly four pure DTOs from `tessera-core`:
`Observation`, `Issue`, `Attempt` and `Approval`. Existing backend import paths,
derives and serde behavior remain intact; the later `Approval.dashboard_url`
still accepts its historical omission. Unknown authored metadata remains in the
exact citation bytes. No production shell-to-backend dependency is added.

All [saved Source Context guards](ai-brain-source-context.md) still apply:
clean/current Source, independent guarded read, workspace/goal/editor/adoption
ownership, hydration, scope restrictions, 8 KiB whole-source limit, twenty
citations and 64 KiB combined guidance/excerpts. Same path/revision is a no-op;
older selected revisions need explicit removal. Other-goal evidence is refused
in both goal and project scope.

## Verification

The backend test oracle compiles the actual shell helper, serializes its Citation
output and uses the existing `validate_citation` and `selected_citations`.
It exercises current Runner-produced canonical output and a checked golden
created by original producer `193812c` through link, observation serialization
and unlink. The golden includes the historical omitted dashboard URL and exact
unchanged Markdown. Focused shell checks exercise inactive-history staging,
hydration, duplicate no-op and late history/provenance changes without write RPCs.

Native evidence and independent source/package/CI reviews are tracked in
[issue267](https://git.oklabs.uk/BeFeast/tessera/issues/267). Frontmatter rendering
presentation is a separate [issue268](https://git.oklabs.uk/BeFeast/tessera/issues/268).
