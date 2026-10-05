# Raw Markdown classifier (#214)

`tessera_core::source_classifier` is a pure child of [managed Live Preview](managed-live-preview.md), built on the reviewed [mapping foundation](source-projection.md). It does not implement a native editor or complete Live Preview. The shell calls it through the application-owned `brain::source_projection::CachedProvider` on a background executor; backend and persistence remain independent.

`classify(&Snapshot)` parses the complete authored source directly with comrak 0.47.0. It returns a `Classification` holding that exact snapshot, a projection `Plan`, semantic `StyleSpan` values in canonical UTF-8 byte coordinates, and bounded diagnostic reasons. `styles_for(current)` rejects document, generation or exact-byte mismatch. Projection independently rejects a stale plan. Styles may nest; approved conceal spans must remain disjoint. Diagnostics are metadata for callers, not document text.

The classifier never calls Reader preprocessing, link rewriting, rendering or `source_preview`. It never reconstructs Markdown from AST literals. Source gaps and unsupported top-level blocks remain exact authored bytes. Unsupported or ambiguous inline syntax makes its whole containing paragraph/heading Source and removes its candidate styles. A failed final mapping safety check makes the whole document Source. Copy and edits still belong to the canonical snapshot, never the display label.

## Conservative initial syntax

- Top-level paragraphs and ATX headings (levels 1–6). Heading hash markers and verified syntactic separators may be concealed; indentation and remaining whitespace stay authored. The reveal block includes indentation and BOM so a caret before the heading reveals its markers.
- Bold and italic with asterisk/underscore delimiters, including validated nested combinations; double-tilde strike. Single-tilde strike stays Source.
- Single-line inline code, concealing only the exact matching backtick runs. Interior spaces, escapes and entities remain authored; this intentionally does not reproduce CommonMark code-space normalization.
- Single-line inline ordinary links with a nonempty plain label and simple destination, plus `[[target]]` and `[[target|label]]`. The label keeps authored escapes/entities. Styles do not resolve or navigate the destination.

Reference/autolinks, link titles, complex/escaped destination delimiters, multiline code/labels, ambiguous or empty wiki fields, embeds/images, frontmatter, fences, indented code, tables, lists, blockquotes, setext, HTML and other unknown nodes remain Source. YAML `---` and TOML `+++` frontmatter are recognized only to preserve them; an unclosed opening delimiter makes the entire document Source. Unclassified unescaped markup-looking text, including incomplete emphasis/link/code and highlight/math markers, conservatively makes the containing block Source. Ordinary punctuation-heavy prose can therefore remain Source. Supporting these cases later requires explicit raw-range evidence.

## Byte coordinates and preservation

Comrak's ordinary source positions use 1-based byte columns and inclusive ends. The classifier maps them through an authored line-start table and validates arithmetic, bounds, UTF-8 slices, raw delimiters, child extents and grapheme boundaries. A final all-concealed projection checks the complete marker set, including grapheme joins created by concealment. Unsupported containers are not traversed and do not need trustworthy source positions.

The source-position probe and regression fixtures establish several exceptions that prevent naive reuse of rendered text:

- Text AST literals decode escapes and entities; raw slices do not.
- Code AST literals trim/normalize spaces and replace line breaks; raw slices do not.
- CRLF `SoftBreak` can identify only the CR byte. No break node is used as a conceal span; the raw CRLF remains one mapping boundary.
- A first-line BOM contributes three byte columns. It remains authored and is never concealed.
- NUL is internally replaced by U+FFFD and shifts later positions. NUL input returns whole-document Source before parsing.
- Lone CR uses parser line splitting unlike an LF-based table. This child returns whole-document Source before parsing for lone CR; LF and CRLF are supported.

BOM, Unicode, raw escapes/entities, newline choice, absent final newline, spacing and unsupported text are preserved. An active selection/IME range reveals every touched block through the existing mapping foundation. These pure tests do not establish native caret, IME, clipboard or undo behavior.

## Resource boundary

Classifier input is limited to **64 KiB**, stricter than the mapping foundation's 256 KiB. Larger input returns exact Source without parsing. The classifier admits at most 4,096 AST nodes, depth 32 below the document root, 4,096 total block/marker ranges, and 4,096 total emitted styles/diagnostic reasons. Traversal and candidate scans are bounded by those limits and the input cap; no recursive classification starts before depth validation.

Comrak has no cooperative cancellation/fuel API. Node/depth caps are checked **after parsing** and do not constitute a parser time or memory bound. No hard parse deadline or native input latency is claimed. A future adapter must schedule classification off the input/paint path, retain the complete canonical buffer, and discard stale results. Input/paint measurement, native single-buffer caret/IME/undo proof, and managed recovery/CAS integration remain separate acceptance gates for #211.

## Verification

Tests include every admitted syntax family, nested canonical styles, independent expected source/display text, generated delimiter geometries, exact copy/replacement, selection/composition reveal, indentation carets, stale styles/plans, Unicode/grapheme joins, BOM/CRLF/NUL/lone-CR cases, malformed/unsupported blocks alongside a valid positive neighbor, and resource caps. Core all-target tests and Clippy accompany the frozen implementation receipt. Standalone parser timings are supporting measurements on identified synthetic corpora, not a native performance gate.
