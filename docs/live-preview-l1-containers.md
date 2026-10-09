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
- Outside containers, lines starting with a fence, `<` or `[` refuse.
- Inside containers, fences and HTML end with the container and are allowed. A
  definition-like line (`[` after container, list and task prefixes) refuses.

After parsing:
- Indented (4+), tabbed or `>` lines must lie inside a container of the local
  parse or of the known contexts.
- A container that reaches the run's end refuses when the next line is not
  blank: it could continue lazily outside the fragment.

Definitions change every use, so definition edits always need a full parse. A
local reparse resolves only labels the full parse already resolved, with the
same URL and title, through comrak's broken-link callback. A new label stays raw
until the async full parse is adopted; it is never guessed.

## Evidence

`tessera-core`: container display corpus (three depths, ordered/task markers,
lazy quotes, BOM/CRLF, tabs, ru/he/niqqud/combining/emoji, unsupported siblings),
reference forms and quiet definitions, local reparse equal to a fresh parse for
edits in containers, and refusals for definitions, unsupported content,
outside-container indentation and lazy absorption. The lazy and indentation
guards were each disabled once to confirm their refusal tests fail.

Native Reader acceptance for integrated L1 (Wayland, fcitx5, latency budget)
is tracked on #868 and is not claimed by this PR.
