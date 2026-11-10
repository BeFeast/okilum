# Code Format, validation and explicit Repair (#2771)

The desktop source editor exposes separate **Format**, **Validate** and
**Repair…** actions for a code/data file and for the ordinary fenced-code body
containing the caret in a Markdown source editor. The existing Markdown style
menu remains independent. Its **Format code / Validate code / Repair code…**
entries also target the current code body. Reader/MCP operations stay read-only.

The language selector is explicit and initially follows the file extension or
authored fence language. JSONC, JSX and SCSS retain their dialect identities even
where highlighting shares another grammar. Changing this selector changes the
interpretation for these operations; it does not rewrite the file extension or
fence marker, infer a dialect from invalid input or save a syntax correction.

Invalid does not disable Format. The operation either returns safe formatting
or leaves the source exactly unchanged and explains why. Validation runs again
after Format, including a refusal; a newer source revision invalidates old
diagnostics. Read-only JSON Pretty also remains callable for invalid input and
keeps the raw source when strict formatting cannot safely recover its structure.

## Current capability inventory

Highlighting support is not a claim of a safe formatter. No generic whitespace
normalizer or another language's formatter is substituted for missing support.
Strict JSON operations share the read-only viewer's **64 MiB input/output**
budget and **128 container levels**. Grammar syntax checks retain their existing
256 KiB / 250 ms budget: their trees can amplify input, unlike the strict JSON
validator, which never builds a value tree. Unsupported formatters/repairs remain
explicitly unavailable. A size/depth/output refusal is one message containing the
limit and source size, never a syntax verdict.

Button dispatch pins an immutable native source revision. Target flattening,
newline detection, validation, formatting and repair construction all run on the
background executor. A visible operation label and **Cancel** share the existing
control row; typing remains available. Cancellation is checked every 16 KiB in
scans and output construction and before/after the strict parser's bounded call.
Cancel/navigation/source mutation retires the result: no partial edit is applied.
The non-interruptible strict parser phase is bounded to 64 MiB. A cancelled job
keeps admission until it drains; there is at most one admitted operation per App,
so repeated Cancel/retry or multiple Reader windows cannot queue source copies.

JSON worker scratch has a conservative **512 MiB envelope**, separate from the
already open editor, its Undo history and rendering caches: a shared native Rope,
one flattened target (at most twice its 64 MiB length in String capacity), bounded
64 MiB proposal/output buffers and bounded parser/string/conversion scratch.
The formatter preallocates its conservative output bound; Repair appends once
instead of repeatedly inserting quotes into the remainder. Repair output growth
is checked before every append, including quoted keys and EOF closers; CRLF/BOM
expansion is checked before conversion. Native controls exercise a near-64-MiB
input with all complete round-trip versions alive and record process peak RSS.

Repair retains the complete revision-bound original and proposal, but each
read-only comparison editor receives at most **64 KiB**, clipped at a UTF-8
boundary. The explanation explicitly says when comparison is shortened; Apply
uses the complete proposal. Preview construction cannot mount several multi-MB
editors on the UI thread. The first source mutation retires those comparison
editors. Apply uses the native stamp/range/language guard, without flattening or
revalidating the source on the UI thread; proposed validation is prepared by the
worker. Format and Apply remain one native Undo transaction and use the normal
atomic, revision-checked save path.

Read-only Pretty opens JSON inputs up to 8 MiB automatically in Pretty and keeps
its separate 64 MiB display-output budget. No tree, Find, sidebar Contents or
control-layout behavior is changed by these source-operation limits.

| Explicit language | Validation | Format: valid / invalid / incomplete | Automatic Repair |
| --- | --- | --- | --- |
| JSON | Strict JSON parser | Token-preserving indentation / unchanged with reason / unchanged with reason | Preview bare identifier-key quoting and balanced EOF container completion; ambiguous/mismatched/unterminated input refused |
| YAML | Installed YAML grammar: syntax check, without expanding aliases or claiming semantic validity | Unchanged with language-specific unavailable reason in every case | Unavailable; unchanged with reason |
| astro, bash, c, cmake, cpp, csharp, css, diff, ejs, elixir, erb, go, graphql, html, java, javascript, jsdoc, kotlin, lua, make, markdown, markdown_inline, php, proto, python, ruby, rust, scala, sql, svelte, swift, toml, tsx, typescript, xml, zig | Installed selected grammar: syntax diagnostics only; grammar absence/budget exhaustion is explicit | Unchanged with language-specific unavailable reason in every case | Unavailable; unchanged with reason |
| JSONC, JSX, SCSS, text or an explicit unknown name | Unavailable; no different dialect substituted | Unchanged with language-specific unavailable reason in every case | Unavailable; unchanged with reason |

For strict JSON `{ a: 1 }`, Format preserves the invalid original. A separate
Repair can propose `{ "a": 1 }`. The proposal shows **Original (unchanged)**
and **Proposed repair**, and only **Apply repair** changes the source.
Missing values, comments, mismatched delimiters and ambiguous strings are not
discarded or invented to produce an apparently valid document.

## Source and revision safety

Formatting preserves key order, duplicate keys, number spellings and string
escapes. It retains BOM, uniform CRLF/LF and the authored final-newline choice;
mixed line endings are refused. Whitespace-sensitive unsupported languages,
comments and string content remain byte-for-byte unchanged.
Single-line fenced bodies inherit the owning Markdown document's newline
convention. Caret movement refreshes controls on code-target transitions;
language menu items recheck their original target and revision before acting.
Fence ranges and newline context are collected on a debounced background task
from an immutable native revision. Toolbar paint and caret movement query only
the matching metadata; they never flatten or rescan the document. A changed
revision hides stale controls until current metadata is ready, and superseded
workers cannot publish targets for a newer source.

A Repair preview pins the exact native source revision, target body and chosen
language. Preview and Cancel do not mutate source or create source Undo entries.
An intervening edit, Undo/Redo, selection into another body, language change,
reload or source conflict refuses stale Apply. Format and accepted Repair use
the existing exact-source replacement path as one Undo step and keep normal
draft protection, save, external-revision conflicts and recovery behavior.

This contract does not grant editing to read-only formats or paged/large-file
previews. Container-prefixed Markdown fences that the existing source snippet
surface cannot isolate safely are not rewritten as flat code.
