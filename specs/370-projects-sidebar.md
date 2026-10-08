# Derived Projects sidebar (#370)

Approved option B from docs/research/370-knowledge-organisation.md. Deliver the
Projects section between Inbox and Folders from canonical project `_index.md`
frontmatter. Active → planned → other; completed projects expand via Show N done.
Muted domain labels, status glyphs and nonzero task counts. Keep the real tree and
canonical files unchanged. No Views heading until a saved view exists; service
Views and annotations are later phases.

Read-only inventory found 11 project index notes in the owner's vault: ten active,
one closed (positive controls in AI/Projects and Business/Projects). Accept scalar
Project/project; preserve unknown statuses rather than guessing active. Exclude
archive folders using the existing tree rule. Projects refresh in the accepted
snapshot/incremental worker; no extra note reads on UI thread. Do not change
incremental index schema or watcher logic.

Core tests: status ordering, domain identities, archive/task boundaries, malformed
properties, incremental replacement/removal. Native test: actual rendered row,
done disclosure, canonical navigation, unchanged source bytes and incremental
status/count update. fmt and affected core/shell clippy before push; one PR-Agent
review. QA owns Linux light/dark before/after after merge (explicit user override).

UI rules: inline navigation and disclosure (1–2); no notification strip (3);
status glyphs and existing section controls (4–5); no new borders (6); human
labels and no zero counts (7); 12px secondary captions (8); platform-neutral (9);
existing compact Reader sidebar styling (10).
