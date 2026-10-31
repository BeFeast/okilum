# Live Preview L1, PR 1: inline content inside containers (#868)

## Classification

Paragraphs and ATX headings are accepted at top level and inside supported
containers: unordered and ordered list items, task items and blockquotes, nested
and mixed. Their inline styles, links and wikilinks project with exact source
coordinates. Container syntax (bullets, ordered and task markers, `>` prefixes,
lazy continuations) stays visible Source on its lines; quote conceal is PR 2.
Tasklist parsing is on, so `[ ]` is container syntax, not rejected text.

Other blocks inside containers (code, tables, setext headings, HTML) and other
containers are not traversed. Their bytes remain exact raw Source while siblings
project. A wrapped link label whose continuation carries a quote prefix stays raw
instead of styling container syntax.

## References (#766)

Resolved `[label][ref]`, `[label][]` and `[label]` project like inline links,
with the definition's URL as target. Inline link labels may wrap; the concealed
`[` and `](dest)` stay on one line. Unresolved or ambiguous references stay raw.
Definition rows are consumed by the parser; uncovered top-level rows of the form
`[label]: destination` keep their height and get the quiet `Definition` style.

## Local structural edits

Accepted blocks inside a container need that container as parse context.
Classification records every top-level list and quote. Retained remap carries
these contexts through inline edits; typing at a container's end extends it.

A local reparse widens its dirty run to every container it touches or borders
across whitespace, so a container that could absorb an indented or lazy line is
in the fragment. Between accepted blocks only whitespace and `>` may remain; an
unsupported block refuses.

Before parsing:
- Outside known containers, lines starting with a fence, `<` or `[` refuse.
- Inside them, a definition-like line (`[` after container, list and task
  prefixes) refuses. Fences and HTML wait for the parse.
- The line above the run is outside the fragment. Unless it is blank or an ATX
  heading it could take a setext underline or block a list, so the run refuses.
- A fragment may begin with a BOM only at the document start.

After parsing:
- Indented (4+), tabbed, `>`, fence and HTML lines must lie inside a container
  of the local parse. Old container ranges are not trusted for this: an edit can
  move a fence to column 0, where it would run to the end of the document.
- A container that reaches the run's end refuses when the next line is not
  blank: it could continue lazily outside the fragment.

Recorded contexts may under-approximate after inline edits (for example an
indented paragraph absorbed into a list). That is safe: widening also takes any
container bordering the run across whitespace, and the parse decides the rest.

Definitions change every use, so definition edits always need a full parse. A
local reparse resolves only labels the full parse already resolved, with the
same URL and title, through comrak's broken-link callback. A new label stays raw
until the async full parse is adopted; it is never guessed.

## Evidence

`okilum-core`: container display corpus (three depths, ordered/task markers,
lazy quotes, BOM/CRLF, tabs, ru/he/niqqud/combining/emoji, unsupported siblings),
reference forms and quiet definitions, local reparse equal to a fresh parse for
edits in containers, and refusals for definitions, unsupported content,
outside-container indentation, escaping fences/HTML, the paragraph above a run
and lazy absorption. Each guard was disabled once to confirm its test fails.

Native Reader acceptance for integrated L1 (Wayland, fcitx5, latency budget)
is tracked on #868 and is not claimed by this PR.

# PR 2: quote markers conceal per element

The paint seam already replaces a quote delimiter's glyph with a bar in the same
advance, so source bytes, wrap breaks, rows and hit-testing do not change.

- Each `>` marker's reveal scope is its own `> ` prefix. A caret inside quote
  text reveals no delimiter; a caret at or next to one reveals only that one.
  Reveal follows the shared immutable reveal snapshot, as for inline syntax.
- The first line's delimiter comes from the AST, so quotes after list markers
  and inside nested items are covered. On a later line the quote's delimiter is
  the depth-th `>` after only spaces and parent delimiters, within three columns
  of the first line's; otherwise the line is a lazy continuation with no marker.
- A quote whose delimiter is joined to a combining mark stays raw alone. Other
  quotes, bullets and rules keep their decorations.

Bars on soft-wrapped continuation rows need a hanging indent: without one a
wrapped row starts at x = 0, under the bar. See PR 3 and the hanging gutter
below (#1641).

# PR 3: paint-only line decorations

- **Code blocks.** Top-level fenced and indented code blocks get a quiet row
  background (vendor patch 0049). It is a `CodeBlock` marker that replaces no
  glyphs: the same prepaint pin and validated source as glyph markers select its
  rows, the block must map unconcealed, and a stale source paints nothing. It is
  painted in the glyph-background pass, before selection and text, so selection
  stays visible. There is no caret reveal, so typing inside the block keeps the
  background steady. Contents stay literal; nothing inside is classified. Code
  inside containers gets no background yet.
- **Rules (#727).** Thematic breaks already paint a rule at the existing row
  height; setext underlines, front matter and fenced text stay raw.
- **Quote bars.** One bar per `>` on every line that carries a delimiter (PR 2),
  and on every soft-wrapped row of such a line, inside its hanging gutter
  (#1641). A bar is never drawn where it would cover glyphs.

Switching decorations on or off changes no source bytes, advances, wrap breaks,
rows or hit targets.

# Hanging gutter for block markers (#1641)

The heading gutter (S6b) hangs a revealed `##` left of the text. Lists, tasks
and quotes get the matching hanging indent: the bytes before an item's text
(indentation, `>` delimiters, `-`/`+`/`*`/`1.`/`1)` markers and a task box after
a list marker) form a gutter, and soft-wrapped rows of the line start under the
first character of its text instead of under the marker.

- The provider lists one `LineIndent { line, prefix }` per projected line that
  starts with such a prefix (vendor patch 0103). Only each line's first bytes
  are read; the newline scan is a byte search, with no parse on the UI thread.
- Container syntax is never concealed, and markers are replaced by bullets,
  boxes and bars in the same advance, so the prefix bytes, their styles and the
  gutter width are identical with the caret on the line and off it: entering
  or leaving a list, task or quote line moves neither its text nor its wrapped
  rows (0 px; the acceptance bound is 1 px).
- The wrapper shapes the rest of a line that already wraps at the width left
  of the gutter, so each continuation row fits; caret movement, clicks and
  paint use the same `wrap_indent`. A gutter wider than half the text width,
  a right-to-left line and a hanging heading keep the previous wrapping.
- Every continuation row of a quoted line repeats its bars inside the gutter.
- Headings inside a list keep `- ` as their gutter; a revealed `##` after it
  stays inline as before.

Evidence: `okilum-shell` source presentation tests cover the prefix grammar and
caret reveal on every byte of list, ordered, task, nested and quote lines with a
positive control that markers really revealed; the vendor GPUI test checks wrap
indent, row widths, hit-testing, vertical movement and repeated quote bars,
against a no-indent control. Native Live Preview acceptance on macOS and Linux
is not claimed by this change.
