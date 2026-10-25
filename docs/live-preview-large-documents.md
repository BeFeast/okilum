# Progressive ordinary-note Live Preview (#1640)

The ordinary editor no longer disables its entire Live Preview provider when
the document crosses 64 KiB. Authored text, selection, IME, undo and persistence
remain in the existing source editor. Classification and projection metadata
remain derived, revision-bound presentation data.

## Classification

`source_classifier::segmented` first indexes top-level CommonMark/GFM blocks
with the already locked `markdown-rs` parser. Attention, inline code, links,
images, entities, inline HTML and the other inline constructs are disabled in
this pass. It records authored UTF-8 byte ranges, preserving containers, fences,
frontmatter and document-wide reference definitions. It does not derive edit
coordinates from rendered text.

Comrak remains the semantic classifier for each block. Its existing 64 KiB,
4,096-node, depth-32 and 4,096-range limits apply **per block**. An oversized or
otherwise limited block stays exact Source without disabling accepted neighboring
blocks. Unclosed frontmatter retains the existing conservative behavior; an
unclosed fence remains one block through EOF. Conflicting duplicate reference
definitions cannot redirect a locally classified container to a later target.

An editor-scoped cache keys each block by its exact authored bytes, complete
reference context and whether frontmatter is allowed at that position. Hash
collisions still require exact equality. A cache hit rebinds both classification
and projection-plan identity to the requested document/generation. Entries and
retained source/context bytes have separate limits. A document change or an edit
covering more than 20% of the previous source clears the cache before rebuilding.

## Publication and editing

The first background task classifies blocks intersecting the native source
viewport. Before the first layout it uses a bounded source range around the
caret. Cache hits outside the viewport are retained before publication, so a
small edit does not erase known offscreen formatting or block heights. Pending
blocks have no inferred inline styles; the continuation classifies them in the
background and publishes the complete result.

Both publications require the current editor entity, document/generation and
exact source bytes. A stale first result cannot authorize its continuation.
Disabling Live Preview cancels adoption; subsequent enabling can resume an
incomplete revision. Session/cache locks are taken only in background work,
never from input, compose or render.

The existing retained-presentation edit mapping, bounded local reparse and
`remap_raw_tail` remain in use between source mutation and fresh adoption.
Projection validates canonical grapheme boundaries, sorted/disjoint ranges,
marker safety and post-conceal grapheme joins as before. Its document allowance
is now 2 MiB, 65,536 ranges and 16 MiB deterministic work units, independently
of the unchanged per-block classifier limits. This includes the 1 MiB acceptance
note and the 1.2M-character performance fixture; these finite bounds are not a
native frame-time claim.

`PendingBlocks` is explicit diagnostic metadata. A partial publication cannot
produce a successful full-Live-Preview latency sample. Source/Live Preview
switching and progressive publication do not replace authored text or create
undo transactions. Saved-target heading-navigation limits remain a separate
contract.

## Validation

Core regressions compare segmented and whole-document output, semantic styles,
decorations and source/display mappings on supported syntax, Unicode/BOM/CRLF,
containers, references, frontmatter, HTML and fences. Cache tests cover identity
rebinding, changed definitions, eviction and the 20% invalidation rule. Native
provider regressions cover partial-to-complete publication and retained edits
in a document above 1 MiB, including Enter in an unclosed fence.

The editor perf report remains supplementary under #1639's manager disposition.
The three new LP scenarios are measured/report-only in CI; their initial
baseline values must not be invented. For #1640 acceptance, the introducing PR
must report at least five measured input/scroll repetitions per size on the same
binary (130 KB, 1 MiB and 1.2 MB). A size may leave the Source fallback only when
its median editing-frame measurement is <=33 ms on Linux. A size that misses the
threshold remains explicitly unsupported and is tracked for follow-up; a
supplementary CI status does not waive this acceptance threshold. Exact-head
native/required CI and review also remain mandatory. M4 <=16 ms and weak Linux
<=33 ms remain product targets; Xvfb software post-frame results do not prove
GPU completion or physical presentation. Same-runner A/B activation belongs to
#1662 after this work.
