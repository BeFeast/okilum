# Markdown incoming references (#316)

Reader already includes ordinary Markdown note links. Complete managed incoming
references with the shared document parser and resolver, retaining the existing
wiki graph semantics, accepted source inventory, scope, line ordering, deduplication,
self-link exclusion, budgets, revisions and cursors.

Use backend metadata for Markdown identity, as managed navigation does. Add edges
only for accepted indexed target snapshots; discovering an identity never enrolls
its contents. Attachments, external links, code and malformed syntax create no
note edges. Heading-qualified links identify their containing note. Ambiguous
note destinations retain candidate edges marked ambiguous.

Version the derived cache and bind reuse to resolver inventory so newly occupied
paths cannot reuse a stale fallback destination. Do not touch canonical Markdown,
live Brain data or services. No controls or visual layout change; existing panels
render the additional references. UI rules reviewed; Linux light/dark evidence
and native actions remain with the manager's exclusive QA sub-session.

Validation: shared Reader/managed fixture for exact destinations and exclusions;
managed scope/dedupe/budget tests; stale/missing source and cache generation tests;
fmt, affected-crate clippy, one GLM review, CI and Linux beta publication.
