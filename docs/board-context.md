# Project-note attention and next-stage context (#1769)

The Board reads explicitly linked project notes for attention, decisions, blockers
and things to try. The original Markdown keeps the arguments, history and full
scenario. The Board shows a short reason/consequence and opens that note in the
existing center. **Discuss** appends references to the existing Manager draft;
it never sends, approves, assigns, dispatches or changes a stage.

Oleg's 2026-10-30 decision preserves #1795: the current stage title, outcome,
scope and Stage note action remain absent from the Board, including its menu.
Current context stays in linked notes. Only **Next stage** has an explicit,
initially collapsed section. No Current scope section or binding is introduced.

## Explicit binding

An optional owner-authored `ops/board-context.toml` sits beside the Board manifest:

```toml
version = 1
notes = ["decisions/choice.md", "scenarios/try.md"]
next_stage = "planning/next.md" # optional; never included as an attention item
```

An absent file quietly disables this projection; `notes = []` is valid. This
separate file preserves compatibility with older strict Board/delivery readers.
No migration or discovery scan of `board.md`, arbitrary prose or remote issues
runs. The reader never creates or edits these files. Unknown keys are errors.
Paths must be unique, project-relative Markdown notes, including through symlinks.
The reader bounds the list to 64 notes, each read to 256 KiB, and a refresh to
2 MiB. File work uses the Board's existing off-thread, visible-only refresh and
owner/generation guard.

## Attention and decision notes

Each linked note declares YAML frontmatter, for example:

```yaml
---
board_context:
  kind: decision
  title: Choose the safe next action
  reason: Two alternatives need owner input
  consequence: The next action waits for this decision
  issue: 42 # optional; an explicit link, never automatic stage enrollment
  resolution: decisions/choice-answer.md # optional, explicit closure evidence
---
# Alternatives
The original **Markdown**, rationale and links remain here.
```

Kinds are `attention`, `decision`, `blocker`, `try`, `next_stage`. Title, reason
and consequence are required. `next_stage` belongs only in the explicit
`next_stage` slot. Only attention/decision/blocker may reference a resolution.
An issue outside the Board's selected set stays visibly outside it; it is never
added to kanban or promoted by these notes. Existing from-tester priority remains
unchanged. Four attention cards appear initially; more expand explicitly.

A missing, renamed, unreadable or malformed note stays visible with its path,
last successful snapshot and source error/age. Config, notes and resolution
notes have independent freshness. A healthy source refresh cannot freshen a
failed note or conceal it behind a successful build.

## Explicit resolution of an exact question

A resolution is another ordinary linked note:

```yaml
---
board_resolution:
  decision: decisions/choice.md
  revision: '<SHA256 shown in the question tooltip or Discuss draft>'
  by: Oleg
  at: '2026-10-30T18:00:00Z'
  outcome: Choose the first explicit alternative
---
The actual decision, rationale and supporting evidence.
```

The revision is SHA256 of a JSON-serialized sorted map containing the question's
kind, title, reason, consequence, issue, platform/channel display labels and
Markdown body (CRLF normalized to LF). It excludes the resolution link, avoiding
self-reference. Use the full digest shown by the Board. Any change to those
question fields/body invalidates older evidence; changing a link alone does not.
The resolution must name this exact note and revision and include nonempty
identity/time/outcome. These fields are owner-authored evidence, not authenticated
approval. A matching fresh resolution removes that question from attention;
missing/stale/mismatched evidence keeps it visible. Merge, successful runs and
acceptance of an older build never resolve a decision.

## What to try

A try note additionally specifies one issue, OS and channel:

```yaml
---
board_context:
  kind: try
  title: Try the visible result
  reason: Check this behavior in the exact delivered build
  consequence: Report what you observe
  issue: 42
  platform: linux
  channel: sprint
# delivery: the real, exact build-specific records defined in board-delivery.md
---
The full manual scenario, expected behavior and observations.
```

Availability reuses [Board delivery](board-delivery.md): the issue must be in the
selected Board set with explicit PR references, proven source inclusion, available
artifact and current matching scenario evidence for that OS/channel/build/source.
The **same try note** must contain that exact `delivery` record and also be linked
by `board-delivery.toml`'s `evidence` list. No receipt is fabricated or inferred
from a note title. Without that evidence, the note remains openable and says
“Try unavailable · build/scenario evidence unconfirmed”. A confirmed row shows
platform/channel/build/source and scenario, plus a Build link. Publication,
installation and human acceptance remain separate.

## Next stage and navigation

The explicit next-stage note uses `kind: next_stage` and the same short
metadata. Its content appears only after clicking **Next stage** and opens as
ordinary Markdown. It cannot alter current issue membership. There is no
current-stage placeholder or hidden replacement header.

Open note / Board return retain the executor filter, selected card, Board scroll
and existing Manager conversation/draft. Discuss retains that draft and its
ordinary Undo; sending remains the user's action. The implementation adds no
network source, execution control, timer or canonical-file write.
