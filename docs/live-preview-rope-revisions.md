# Live Preview Rope revisions and edit receipts

Issue #2403 is the first presentation/input slice of #1621. Its acceptance bound
is source/display byte inspection proportional to the edited logical line plus a
constant, independent of the note's total size (owner decision 44461). Incremental
grapheme segmentation or wrapping inside a logical line is outside this slice.

The native editor owns canonical Rope revisions. Source snapshots share those
revisions; replacing source bytes publishes a receipt naming the predecessor,
old byte range and inserted byte length. Up to 32 receipts are coalesced without
retaining old notes. Missing or foreign provenance cannot authorize guessed edits.
History and IME still mutate the canonical editor buffer through its existing
transactions; presentation state adds no Undo step and is never copy/save authority.

The core receives the exact native revision and receipt. Retained presentation,
grapheme/newline indexes and display Ropes reuse unchanged nodes. Source/display
mapping and S1/S2 scroll anchors consume the same receipt instead of discovering
prefixes and suffixes through note-wide byte comparisons. Legacy contiguous
providers retain a compatibility path, which also supplies positive controls.

Canonical workers prepare container indents, rendered-block identities, heading
reveal scopes, inline formula candidates and punctuation lexical summaries.
Remapping carries unchanged metadata forward and inspects changed logical lines.
Punctuation keeps paragraph-wide quote pairing and language selection from cached
line events, including quotes crossing CRLF lines. Protected-marker validation
reuses only an exact predecessor's proof for byte ranges outside the edit.

The regression gate covers 25 KB, 130 KB and 1 MiB notes with ordinary logical
lines, a multi-line prose paragraph, and a prose line of a few KB. Counters observe
source/display slices, explicit flattening, cold display construction and display
replacement bytes. Actual native renderer tests separately observe projected
frames, bounded dirty rows, retained scrolling, IME, Undo/Redo and authored Copy.
Cold work and instrumented origin/main runs are required positive controls;
zero counters without those controls are not evidence.

A note consisting of one logical line remains a report-only edge case. The
preserved core diagnostic at 25,600 / 133,120 / 1,048,576 bytes reports 140,805 /
732,165 / 5,767,173 inspected source/display bytes respectively, with no flattening
or cold display builds during the edit. Those measurements expose whole-line
segmentation and do not establish compositor latency. The test continues to
report this case; it does not apply the ordinary-line scan budget to it.

This change does not change draft persistence/recovery or own-save publication.
Those are #2404 and #2405. gpui core remains unpatched; native changes are carried
by the existing gpui-component vendor patch stack and verified by its script.
