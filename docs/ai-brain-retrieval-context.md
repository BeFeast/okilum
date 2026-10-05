# AI Brain indexed retrieval and reviewed context

Parent [#113](https://git.oklabs.uk/BeFeast/tessera/issues/113), contract
[#114](https://git.oklabs.uk/BeFeast/tessera/issues/114). This is an additive
milestone after the accepted alpha, not a change to the v0 read-only contract.
Development and acceptance remain externally controlled in T3.

## Observable outcome

A user describes a problem, finds relevant original Markdown passages, reviews
which passages will be sent, edits task guidance and prepares a bounded packet.
The existing T3 stage consumes that reviewed packet. The same packet can produce
an AI context export containing summary, decisions, constraints, open questions,
next step and source provenance. No manual copying between these surfaces is
required. Exact Markdown-and-attachment archive export remains a separate action.

## Authority and derived storage

Canonical Markdown and adjacent media remain authoritative. The lexical/chunk/
embedding index is disposable and rebuildable. Its deletion must not remove goal
records, frozen context, dispatch identities, acknowledgements or other durable
operational state. The index follows create, modify, rename and delete events;
read/open events must not cause indexing. Idle is idle: no recurring full-brain
scan or cache rewrites when canonical files do not change.

An index generation describes an observed corpus, not a promise that the disk
cannot change. Search results report freshness and indexing state. Before a
reviewed packet is consumed, the backend validates the exact selected revisions.
Changed, deleted, inaccessible or newly excluded sources cause an explicit stale
review error; the user rebuilds/reselects and reviews the replacement. No source
is silently omitted, substituted or refreshed inside an already reviewed packet.

## Retrieval boundary

The application API adds `brain_index_status`, `brain_index_rebuild` and
`brain_search`. Search accepts the query, mode (`lexical`, `semantic`, `hybrid`),
explicit scope (`scope.mode=project|goal`, default `project`) with its target
goal and optional path/include/exclude restrictions. Project means this brain's
saved knowledge, optionally restricted to a directory. Include and
exclude apply before selection; exclusion wins. The final API field shapes are
recorded with implementation in [the application API](ai-brain-application-api.md).

Search returns bounded original excerpts with a stable citation identifier,
brain-relative path, exact source revision, line/heading locator, method and rank.
Scores remain method-specific: a keyword score is never represented as vector
similarity. Original excerpts preserve source text. Generated paraphrases are not
returned as source quotations. Source kind, status, date and verification metadata
remain available when present, especially for generated results and decisions.
Unknown metadata stays unknown; indexing does not verify a source's claims.

Frontmatter delimiters must begin in column zero; indented Markdown dividers
inside YAML literals remain metadata content. Retrieval and reviewed-packet reads
share this rule, including BOM openings, CRLF and trailing delimiter whitespace.
Malformed canonical record ownership fails closed. Derived index schema v2 rejects
earlier generations and rebuilds from sources, so previously misclassified raw
context chunks and their vectors cannot survive a cache reload.

A search requires a selected target goal and an explicit knowledge scope. Goal
scope includes shared ordinary Markdown and the current goal's saved knowledge.
Brain/project scope may retrieve other goals' saved outcomes and decisions as
referenced knowledge, preserving owner, status, verification and date. A project
scope can be a visible relative directory restriction; it need not invent a new
project identity model. Raw internal stage, session, dispatch and operation
records are excluded from retrieval. The UI exposes scope and never silently
widens it. The default should make project knowledge reusable.

Referenced knowledge does not transfer operational ownership. Every reviewed
packet remains bound to its target goal; inspecting or including another goal's
saved outcome cannot mutate that goal, its task, criteria or stage. Backend
validation enforces the packet's declared scope even for explicitly supplied
citation objects. Goal-only selection cannot smuggle another goal's records in
by supplying their paths.

`semantic` requires actual embeddings. Provider/model identity, immutable model
digest, dimensions, query/document preprocessing and chunking version identify
compatible cache entries. An unavailable provider is explicit: semantic search
is unavailable, and any hybrid lexical-only fallback is visibly degraded. Do not
silently reuse vectors from a different model or label a heuristic semantic.

The initial candidate is Ollama `qwen3-embedding:0.6b`, subject to the independent
corpus gate, not accepted merely because it returns vectors. Provider configuration
is an operator concern; users do not type model digests into their task flow.

## Lexical readiness before semantic enrichment

[#300](https://git.oklabs.uk/BeFeast/tessera/issues/300) publishes two immutable
phases on the existing index worker. After complete source inventory, source
revision revalidation and lexical/incoming-graph construction, main index status
is `ready`. Missing document embeddings report `semantic_status=indexing` while
lexical search and incoming references answer from that generation. The separate
incoming-graph budget may explicitly make backlinks unavailable while ordinary
Search remains healthy; truncated backlinks never appear as complete results.
Hybrid uses its existing explicit lexical fallback; semantic-only search returns
`semantic_unavailable` until all configured vectors are usable.

The same worker enriches missing vectors serially using the existing configured
model, digest, dimensions and timeouts. Completion publishes a new generation;
provider failure publishes semantic `unavailable` with its warning while lexical
and backlinks remain available. An incoming cursor from the first phase refuses
after the second phase; use explicit Refresh. Each phase owns its own lexical
files and cache directory. Source files and operational journals are untouched.

Relevant watcher events and explicit rebuild increment an invalidation epoch
under the publication lock. Both publications revalidate source inventory and
configuration, then commit the disk pointer and visible generation under that
same lock. Enrichment checks epoch, configuration and phase ownership between
batches, so obsolete work cannot publish or continue the old batch sequence.
An in-flight HTTP request may finish under its existing timeout; a source change
marks the index stale immediately and the queued refresh may wait for that call.
There is no additional worker, polling loop, provider retry or transport
cancellation mechanism. Operator configuration changes are detected during
active enrichment; an idle configuration change still needs the existing
explicit rebuild/restart path.

Same-schema exact source/model vector reuse is preserved. Partial valid vectors
remain reusable after failure and restart, but missing vectors never satisfy the
same-document fast path or advertise semantic readiness. Cache v5 is unchanged;
older schemas remain incompatible and rebuild from canonical source.

## Reviewed packet

The native flow is search → inspect original → select/remove/pin → edit guidance
→ review/freeze → prepare or export. A pinned source is an explicit selection;
it does not bypass scope, source access, revision validation or packet budgets.

A packet has an immutable identity/revision and goal ownership. It records the
query/method and chosen citation IDs, exact source paths/revisions/locators and
excerpts, plus editable task guidance and the applicable goal/result context.
Excerpts and guidance are separate: editing instructions never changes the
original source quotation. The UI shows the actual bounded content that the
consumer receives, including any truncation. Oversize selections require an
explicit correction rather than silently dropping the tail.

The T3 path retains existing prepare/start/revise/discard guards, stage ownership,
previous-result history and explicit criteria verification. A terminal engine
outcome does not verify a goal. An earlier client may continue using the existing
full selected-source `stage_prepare` path only when the target goal has no saved
reviewed-context packet. The new flow cannot silently append unreviewed full
documents or prior transcripts behind a bounded packet, or omit its saved packet
reference. Guidance and excerpts occur once in the actual dispatch payload.
Source changes before preparation or Start require explicit renewed review;
changes after an accepted dispatch do not block its reconciliation or outcome.

A guidance-only packet is valid for T3 after explicit review, visibly stating that
no source evidence was selected. T3 preparation does not require an artificial
task binding. AI export requires at least one real selected citation.

## AI context export

AI export consumes the same frozen, reviewed packet; it does not perform a second
hidden retrieval. Reuse the configured conversation provider and existing
cancellation/error behavior. Preserve the packet ID/revision and original-source
provenance in the export. The AI archive includes only selected exact excerpts,
with original path/revision/line provenance and excerpt hashes, plus reviewed
guidance and generated Markdown. Unselected note content is omitted. Exact full
Markdown-and-attachment archive export remains separate and does not depend on
an LLM. Export jobs persist across desktop closure; backend restart interrupts
unfinished jobs without automatic provider replay. Cancelled or failed jobs do
not become downloadable partial packages or overwrite earlier completed jobs.

The generated document contains summary, decisions, constraints, open questions
and next step. It distinguishes source-backed statements, interpretation and
unknowns. References must resolve to citations actually present in the packet;
reject invented/invalid references rather than publishing an apparently cited export. A valid citation proves
provenance, not the truth of the generated statement. Source content is untrusted
data, not permission to change scope or instructions to the application.

Historical PARTIAL/unverified AI results remain historical. A later user decision
may supersede one without pretending the older source already said it. Preserve
both origins/status/date where selected. Planning acceptance does not establish
unperformed build, install, physical-device or health integration checks.

## Acceptance and evidence

[#115](https://git.oklabs.uk/BeFeast/tessera/issues/115) owns backend indexing and
retrieval; [#116](https://git.oklabs.uk/BeFeast/tessera/issues/116) owns native review
and AI export; [#117](https://git.oklabs.uk/BeFeast/tessera/issues/117) joins evidence
and delivery. Reviews are bounded to two substantive rounds; unrelated findings
become follow-ups.

The independently declared corpus contains 24 synthetic Markdown notes, close
engineering/UI distractors and English/Russian paraphrases. Fourteen predeclared
queries compare the same corpus and scope across lexical, semantic and hybrid.
Count each relevant document once even if multiple chunks appear. Measure Hit@3
and MRR@10. Semantic and hybrid each require at least 12/14 Hit@3; semantic must
recover at least two queries missed by lexical Hit@3, and at least three of the
four cross-language queries. These are focused usefulness gates, not a claim of
general search quality. Preserve the initial result and all later comparisons;
do not tune on hidden answers and then describe the result as blind evaluation.

The private oracle, expected results and scoring receipts remain outside indexed
roots and outside engine/provider context. Corpus bytes and oracle are pinned
before the first measurement. A separate old-PARTIAL/later-clarification case
checks faithful provenance without requiring an automatic truth engine.

All of the following are separate mandatory correctness checks:

- Positive control proves search and event instrumentation observe actual changes.
- Explicit include/exclude, knowledge scope and target-goal ownership are enforced by the backend.
- Source change invalidates a reviewed packet; deletion removes results.
- Rebuild preserves retrieval meaning and all canonical/operational state.
- Provider absence or incompatible model cache produces truthful degradation.
- Excerpts match the named original revision and locator byte-for-byte.
- Read-only activity does not trigger index writes or repeated rebuilds.
- Native review, bounded T3 payload and AI export consume the same packet.
- Export preserves the historical/current distinction and unperformed checks.

Evidence reports implemented, tested, built, packaged and native-verified states
separately. Deliver the artifact as soon as it exists and continue verification.
linux-test-host is the preferred isolated GUI rig. macOS testing remains user-owned;
Windows is deferred by the current user instruction.
