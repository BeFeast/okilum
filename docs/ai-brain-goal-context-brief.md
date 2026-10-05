# Goal context brief

Issue [160](https://git.oklabs.uk/BeFeast/tessera/issues/160) adds a derived,
read-only view of a goal's saved inputs. It introduces no canonical record,
operational journal, provider request, Chat prompt change, or engine action.

The existing workspace-bound protocol accepts:

```json
{"schema":"ai-brain/workspace-v1","id":1,"expected_workspace":{"brain_id":"…","root":"…"},"op":"goal_context_brief","goal_id":"…"}
```

Use the exact `workspace` object returned by `capabilities` for
`expected_workspace`. Capability `goal_context_brief: true` advertises support.
The normal successful response `data` contains:

| Field | Meaning |
| --- | --- |
| `schema` | `tessera-goal-brief/v1` |
| `goal_id`, `goal_revision` | Selected canonical goal and exact source revision |
| `generation` | Deterministic SHA-256 of the visible derived data, excluding this field |
| `inputs` | Latest saved result first, then saved Attention replies newest first |
| `remaining_criteria` | Existing Criterion objects not proven by existing runtime checks |
| `omissions` | Objects with `path` and machine-readable `code` |
| `complete` | False when any source/evidence/budget omission is present |

Each input has `id`, `kind` (`decision` or `result`), `title`, `actor_id`
(null for results), `received_at`, `verification`, `body` (display Markdown),
and `citation` (the **existing** Citation shape). Attention replies preserve their
actor/source identity and remain `unverified`; they are user input, not proof of
completion. The latest result preserves its recorded verification state.

Citations contain the full, exact canonical Markdown including frontmatter. This
keeps actor, source and verification metadata available to existing context packet,
T3 and AI export readers without a schema upgrade. Each source appears once as an
input; original source bytes are never rewritten or semantically deduplicated.

The UI must show inputs and missing evidence before selection. Explicitly selected
citations are merged by exact path/revision into the existing chosen citation set,
retaining existing manual pins. Use ordinary `context_prepare` and context review
for the resulting packet. A brief query does not create, change, review or pin a
packet. Re-querying does not authorize replacing already reviewed packet text.
Existing source revision validation rejects stale citations and reviewed packets.

The reader uses canonical decision files and retained Attention ownership receipts.
Known replies cannot move to another goal by changing their frontmatter owner.
Missing, invalid, oversized, or unreadable candidates are omitted visibly; contents
with unknown ownership never enter an input. A restored canonical-only reply is
eligible if all identity and provenance fields validate. A canonical file already
saved while its local receipt is recovering remains knowledge, not execution.
The last saved result is located by reverse goal-stage order; an unavailable latest
stage/result is reported, with no silent substitution of an older outcome.

Bounds are 1,024 decision candidates, 8,192 bytes per exact citation, 20 inputs,
and 48 KiB of cited source bytes. Canonical result/evidence reads use the existing
reviewed-context source bound of 1 MiB, independently of citation admission. A valid
large result can therefore prove a criterion even when its full canonical source
cannot be included as a citation; that citation omission remains visible. Evidence
and any admitted citation come from the same exact source snapshot. Exceeding the inventory bound rejects the query;
input/source limits produce omissions rather than partial source excerpts.
Criteria use the existing retained-goal/evidence rules conservatively; unavailable
evidence remains unproven and is reported. This view never marks a goal complete,
selects a next action, claims semantic supersession, starts work, or controls Maestro.

Older backends need not understand the new query, but context packets prepared from
its citations use the existing canonical schemas and revision checks. Hiding the
brief when the capability is absent is sufficient; no journal migration is needed.

## Explicit Discussion decisions

[Keep as goal decision](ai-brain-discussion-decision.md) admits an additional
`discussion-decision` input: an exact saved user turn explicitly reviewed and saved.
It stays unverified and carries original actor/conversation/turn/revision provenance
in its full canonical citation. Attention decision admission is unchanged. Both
decision kinds share the existing inventory, count and byte budgets; malformed,
changed, unavailable and oversized records remain visible omissions. Saving does
not rewrite any reviewed packet or stage envelope.

## Manual-only Discussion decisions

[Decision reuse settings](ai-brain-discussion-decision-reuse.md) excludes validated manual-only records from automatic `inputs`. When present, `manual_only` contains only id/path/fixed status, capped at 20 entries and 2 KiB serialized total; `manual_only_truncated: true` reports further entries. Neither field exists when there are no manual-only records, preserving the exact pre-policy JSON/generation for historical proposal retry. No stopped text, citation or user-provided title is included in the summary. This intentional policy is not an invalid-source omission. Inspect fetches the current exact citation separately for an explicit selection.
