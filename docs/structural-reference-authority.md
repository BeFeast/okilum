# Canonical structural/reference authority (#1640)

Core-only groundwork; Reader, the full-parent fragment-cache key and every
production size/range/work cap remain unchanged. There is no local reparse,
fragment fuel, replay, composed display join or large-note admission here.

`source_classifier::structural::BlockIndex` is built with one canonical root
parse. Its AST supplies top-level blocks/whole containers and its optional
reference audit supplies **all** accepted definitions, effective first-definition
indices, lookup order and the same root fuel. Classification consumes that same
AST through the classifier's extracted `classify_parsed` helper. No hidden
definition parser or second fragment parse reconstructs semantic context.

The admitted context is closed: the exact shared Live Preview options constructor,
document-start frontmatter policy and no broken-link callback. The immutable index
and reference authority bind the exact document/generation/source bytes and this
configuration. Unknown options, even pointer-equal/field-similar options or a
stateful callback, receive a fresh observation-only parse with the original
options/callback. They cannot produce an authority or completed adoption ticket.
No hash or closure pointer substitutes for semantic identity; callbacks retain
their normal exactly-once behavior.

Canonical top-level AST positions and definition **paragraph context** positions
are converted to authored byte ranges and UTF16 ranges at actual grapheme
boundaries. Invalid line/column, overflow, invalid UTF8 byte, surrogate interior,
cluster interior and reversed ranges refuse authority. Whole-line fragment
ranges include original indentation/newlines; exact raw gaps cover the rest.
These are source partitions, not independently parsed/projected pieces. They do
not certify safe projected joins. Unterminated frontmatter refuses authority and
keeps the existing canonical whole-root raw policy.

Definition offsets/raw text and lookup offsets inside prefix-stripped parser
content remain explicitly labeled observations. They are **not** root editor
ranges and cannot authorize source edits or navigation. Converting those relative
positions, if a later feature needs it, requires separate proof; no private
container-prefix algorithm is copied here.

The core session models pending/completed results without adding a runtime or
scheduler. Complete results can adopt only for the current exact request sequence,
source and closed context. Partial results never adopt; stale completed results
cannot replace newer input. Held old authorities remain valid only for their old
snapshot. On a change of **more than 20%** of the previous source bytes (maximum
changed middle length after common prefix/suffix), retained structural authority
is dropped before work; exactly 20% retains the old immutable value, which still
cannot authorize the changed current source. Every new ticket computes a fresh
canonical root. This is invalidation/adoption policy, not cross-edit work reuse.

Root fuel is observed, never reset or replayed. Failed adoption/readback performs
no parser or lookup work and cannot charge again. Equivalence of future local
assembly/replay is **not proven**; canonical full-root recompute remains the
fallback. The fragment cache continues to require its whole-parent key.

Targeted core regressions cover complete unused/duplicate/forward definitions,
canonical fresh classification/ranges, direct fresh AST positions, source partition
coverage, frontmatter/container context, Unicode/CRLF/UTF16 boundaries, unknown
options/stateful callbacks, held old roots/two unpainted revisions/shifted ranges,
the exact 20% threshold, stale partial/completed requests and no charge on failed
adoption. Counters at the actual canonical parse site provide cold/no-extra-work
controls; there is no cross-edit hit or performance claim.

Receipt clarification: the earlier parser API's 21-case XML/HTML/CommonMark
upstream comparison did not enable XML `render.sourcepos`; it proves those exact
outputs, not an independent upstream source-position comparison. This slice
checks source positions directly against a fresh canonical AST and checks their
editor-coordinate conversions explicitly. The existing 21-case audited/default
API test now also compares every descendant's Sourcepos directly.

Remaining integration obligations: safely bounded current-root assembly and
fuel/outcome equivalence, projected joins/global mapping, viewport and publication
ownership in the real Reader, PresentationProof and source/features/binary
reconciliation before any separately authorized size admission. No measurement
binary, release/diagnostic build, GUI/native run or hosted dispatch is authorized
by this code/test contract.
