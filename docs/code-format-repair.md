# Code Format, validation and explicit Repair (#2771)

File Edit keeps the pinned 48 px viewer header. One primary **Format** split
button runs Format; its caret opens **Format code**, **Validate code** and
**Repair code…**. Ordinary Markdown fenced-code bodies retain their existing
three-button operations block. The existing Markdown style
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

### XML Format (#2912)

Format code on XML/XSD/XSL files and `xml` fences pretty-prints with the
editor's indent unit (its default TabSize, two spaces, as JSON Format writes).
A streaming two-pass formatter in `okilum-core` builds no tree and expands no
entity. It changes only whitespace between tokens and copies everything else
byte for byte: start and end tags (attribute order, quotes and spacing), text,
comments, CDATA, processing instructions, the XML declaration and DOCTYPE.
One blank line before a child survives; BOM, CRLF/LF and the final newline
follow the source, and mixed line endings are refused as for JSON.

An element's content is re-indented, one child per line, only when that
cannot change its text:

- its content is child elements, comments or processing instructions and
  nothing else — no non-whitespace text, CDATA or reference;
- `xml:space` does not apply, after resolving the value (character
  references, whitespace): anything other than `default` freezes the subtree,
  an unresolvable value counts as `preserve`, and `default` inside a frozen
  subtree keeps it frozen;
- in a prose vocabulary — XHTML, DocBook, TEI, SVG text elements, and in no
  namespace the HTML, DocBook and DITA names that carry prose (`p`, `b`,
  `li`, `para`, `emphasis`, `ph`, …) — whitespace between children may be
  text, so the element is re-indented only when every boundary between its
  tags and children already holds whitespace. `<p><b>a</b><i>b</i></p>`
  reads «ab» and stays byte for byte.

Data vocabularies (a Maven POM, Atom, configuration files) are element-only,
so a one-line file is indented. Every other element is copied verbatim.

The formatter's well-formedness check is the authority for Format and
Validate; the tree-sitter grammar only highlights. It checks matched tags,
one root, quoted and unique attributes, XML characters, character references
to XML characters, entity references that are predefined or declared in the
internal subset (an external subset or parameter entity reference allows
others), comments without `--`, no text outside the root, and at most 10 000
nesting levels (deeper input is refused as a budget, not as invalid), at any
size up to 64 MiB. Format refuses exactly what Validate reports as a syntax
error, with the same line and column and Go to error; nothing changes. The
64 MiB input and output limits, the worker, Cancel and the single Undo step
are JSON's. In Read, Format does what it does for JSON. XML declaring a
non-UTF-8 encoding stays read-only, so Format never reaches it.

Explicit file Edit admission retains the viewer's 4 MiB UI cap. An existing
editing session can grow beyond that cap and keeps the 64 MiB JSON operations
budget. The read-only viewer, pagination, Tree, Find, shaping and save pipeline
retain their existing limits and behavior.
Plain-file edit entry and mutation do not schedule the Markdown draft-outline
parser. Entering a plain file retires the previous note's draft-outline worker
and cache; ordinary Markdown editing retains its existing outline updates.
Large plain-file Source also releases its hidden read preview instead of
retaining a second complete data tree beside the native editor. Explicit Read
recreates that preview through the existing file-open path.

Button dispatch pins an immutable native source revision. Target flattening,
newline detection, validation, formatting and repair construction all run on the
background executor. For files, a running label and **Cancel** share the single bottom line;
notes keep the existing control row. Typing remains available. Cancellation is checked every 16 KiB in
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

File Repair opens a sheet over the lower editor; the editor viewport does not
move or shrink. Original and proposal sit side by side, with Cancel and Apply
repair. Cancel never changes text, and Apply is one Undo step without an
explicit Save. Repair from an invalid file's read bottom line enters Edit first.

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

## File-only toolbar and feedback (#2811)

Raw stays selected in Edit. The other views and Fold/Unfold remain visible but
disabled, with live shortcut hints explaining that Read restores them. At 700 px
the switch becomes one menu; Read, dirty Save, Format, Find and Document actions
stay in the header. Copy, Open with and folds yield to overflow. Language is in
the Format menu for a known extension; an unknown/absent extension gets its own
button when space permits. The toolkit's DropdownButton and the app's 28 px hit
areas/theme tokens take precedence over static mock button internals.

File Find uses the shared header field and revision-bound EditorTarget worker:
60 ms debounce, immutable Rope snapshots, literal matching off the UI thread,
stale-result rejection and selection-relative stepping. The toolkit search strip
and Find in note controls remain available only in their note host.

For `.json`, one strict verdict follows the last key after a 120 ms debounce on
a worker. Only one parse is in flight; intervening edits cancel it and coalesce
into the latest revision. Badge, bottom line and inline diagnostic consume the
same immutable snapshot and verdict. Current and last-valid snapshots retain a
shared Rope, not another value tree or flattened source. This is the handoff for
future Contents work; no Contents parsing is added here. Other languages keep
explicit Validate and the not-validated-since-change state.

Refusals, syntax/validation results, running state, failed Save and conflicts
have one bottom line. Successful Format/Repair, already-formatted/already-valid,
cancellation and stale-result feedback are transient toasts. Conflict actions
remain available from Resolve and Close, keeping draft in that line. Format,
Validate and Repair refuse dispatch while busy, after failed Save and during a
conflict; the menu explains the disabling reason.

Live Format/Language/badge views occupy measured sibling overlays of the cached
Header, like Find and Save. Typing, Find and validation do not render cached slots
or notify SidePanels. Focus changes still cause GPUI's ordinary render. Returning
to Read carries a small presentation snapshot: view, sparse Tree fold/selection
state, part, query/case and text selection/scroll. The newly loaded viewer clamps
the handoff to the current document and never remembers internal Raw editing as
an explicit read-view choice.
