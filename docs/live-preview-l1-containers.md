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

`tessera-core`: container display corpus (three depths, ordered/task markers,
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

Bars on wrapped continuation rows need reserved indentation and are PR 3.
