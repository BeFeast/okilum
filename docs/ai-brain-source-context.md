# Use a saved Source in Context

**Source → Use in Context** adds the complete saved note to the selected goal's
local Context draft and opens Context. Existing selected citations, pins, guidance,
query and scope remain. The action does not pin the new citation. Choose **Build**
and then review the Context explicitly before using it.

The note must be nonempty UTF-8 Markdown outside operational records, with no
`record_type` and either no `type` or case-insensitive `type: Note`. Ordinary
user-authored and assistant-derived notes are supported, including unowned project
notes when the retained scope allows them. A different goal's note is refused.
Assistant attribution and unverified status remain exact.

[Saved Maestro observations](ai-brain-maestro-observation-context.md) are the
explicit operational-record exception, with same-goal historical ownership checks.

The entire source, including frontmatter, must fit one 8,192-byte citation. BOM,
CRLF, Unicode, unknown metadata and a missing final newline are preserved.
Malformed or ambiguous frontmatter, unsupported records and oversized sources
are refused visibly; use ordinary Context search to select an eligible passage.
No text is truncated or split automatically.

## Exact read and local merge

Save or discard Source edits first. The action independently reads the current
saved source through guarded `source_read`. It captures the endpoint, full
workspace, goal, path, revision, bytes and native editor input stamp. If the disk
source changed, Reload and inspect it before a fresh explicit action. A late
reply after changes to either draft or its owner leaves both drafts unchanged.
Pending editor writes, conflicts, recovery, close, note/decision/criteria reviews
and Context proposal adoption must be resolved first.

If Context has not been loaded, guarded `context_get` hydrates its saved packet
before adding. Transient local guidance, query, chosen citations and pins survive
hydration, including when the local packet is null. Conflicting saved/local
folders require inspecting Context first. Existing include/exclude restrictions
remain in the scope and in the subsequent Build request.

An already selected path and revision is a no-op with **Already included in
Context**, even when the existing citation is only a passage. An unchanged
reviewed packet stays reviewed. If Context contains an older revision of the path,
remove it explicitly through normal Context controls before adding the current
Source. Adding cannot exceed twenty citations or 64 KiB of guidance plus excerpts.
Refusal preserves both drafts.

A successful addition marks the local selection changed while preserving the
previous saved packet. Staging makes only read calls: it creates no canonical
file, packet, receipt, provider task, index or recovery journal. Existing
Build/Review validation remains authoritative and rejects stale source revisions.
There is no new promise to recover an unsaved Context draft after a crash.

## Validation boundary

The shell constructs the existing Citation wire shape with deterministic ID,
exact full excerpt, line bounds and current metadata projection. A backend test
compiles this same production helper in test-only scope, serializes its result,
and passes it through real `retrieval::validate_citation` and
`context::selected_citations`. There is no production shell-to-backend dependency
or change to record admission or API schemas.

Focused GPUI checks cover hydration and retained draft state, duplicate no-op,
late goal/workspace/source/Context changes and changed disk reads with recorded
read-only RPCs. Native and CI evidence are tracked separately in
[issue265](https://git.oklabs.uk/BeFeast/okilum/issues/265).

## Use selected lines from a longer saved note

In Source or Live Preview, select text and choose **Use selected lines in Context**.
Inspect the preview, then choose **Add to Context**. Context citations contain
complete source lines: the preview explicitly shows when it expands a partial
first or last line. A selection ending at the start of the next line excludes
that next line. Selecting a final newline does not add an empty line. Unicode,
CRLF and missing final newlines retain their original bytes; reversed selection
produces the same passage. The source selection and draft are unchanged.

The complete saved note can be at most 1 MiB, matching the existing Context
source-read boundary. The complete-line excerpt can be at most 8,192 UTF-8 bytes.
Larger notes or excerpts are refused before staging; nothing is truncated. Only
ordinary eligible notes use this action; operational records keep their existing
specialized Context actions. Attribution and verification come from the complete
saved source even when its frontmatter is outside the passage.

Save or discard edits and finish IME composition first. Preparation reads the
saved source and, when needed, the saved Context packet into a private preview.
Add reads them again. Changed disk bytes, selection, goal, workspace or Context
require a fresh preview; Cancel invalidates pending replies and changes neither
draft. Existing query, guidance, scope, citations and pins survive a successful
Add. The new passage is unpinned and still requires explicit Build/Review.

An already selected path and revision remains a no-op, including another passage
from that note. Remove it explicitly in Context before replacing it. An older
selected revision is refused rather than replaced. Preview and Add make only
read calls and create no source, packet or recovery record. The existing whole-note
**Use in Context** action retains its 8,192-byte full-note limit.
