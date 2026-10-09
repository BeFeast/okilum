# Obsidian syntax in the Reader

Issue #651. The Reader shows Obsidian-specific Markdown the way Obsidian does,
without writing to the note. Core (`okilum_core::obsidian`) rewrites the
Reader's derived source into Markdown the stock renderer understands; the
shell (`reader_obsidian.rs`) gives the tagged blocks their look. The MCP
`read_note` source is unchanged: an agent still reads the note as written.

Code spans and blocks are always excluded (`prose.rs` draws that boundary), so
`` `%%x%%` ``, `` `$x$` ``, `` `[^1]` `` and `` `^id` `` document syntax instead
of using it. The corpus note is `fixtures/reader/obsidian-syntax.md`.

| Syntax | Reader behaviour |
|---|---|
| `> [!type]` callouts | Typed accent and glyph (unchanged, #46). `[!type]-` starts closed, `[!type]+` open; the title row toggles with a chevron glyph. Fold state is session-only, never written. |
| `==highlight==` | Unchanged (#47). |
| `%%comment%%` | Hidden, inline or across lines. A comment that fills its lines removes them, so it cannot split the paragraph around it. |
| `[^id]` footnotes, `^[inline]` | Numbered by first reference, shown as a superscript link. Definitions move to the end of the note after a rule, one block per footnote, each with a back-to-reference glyph. |
| `$…$`, `$$…$$` math | No math renderer: inline math is shown as a code span, display math as its TeX source centred in the monospace face on the code tint. |
| `^id` block IDs | Hidden. `[[note#^id]]` and `[text](note.md#^id)` open the note and land on the block; `![[note#^id]]` embeds just that block (a paragraph, a list item with its children, or the block before an ID alone on its line). |
| `![[note#Heading]]` | Unchanged (#49): the heading's section. |

## Decisions

- **An unpaired `%%` is text.** Obsidian hides everything after a stray marker;
  Okilum does not, because a `100%%` typo would hide the rest of a note and
  the reader could not tell anything was there.
- **Unreferenced footnote definitions are not shown**, as in Obsidian and GFM.
  A reference whose label nothing defines stays literal (`[^x]`), which is
  visible and therefore not a silent guess.
- **Math falls back to its source.** Rendering TeX needs a renderer that passes
  the rendering bar; until one does, the formula's source is the honest
  fallback, and inline math is never mistaken for prose. A currency pair such
  as `$5 and $10` is not math (Pandoc's rule: no space inside the delimiters,
  no digit after the closing `$`).
- **Block IDs follow the heading contract.** A missing ID refuses with "No block
  with that ID exists" and keeps the current document; a duplicated ID is
  ambiguous and never picks a winner. The managed Source editor reports block
  navigation as unsupported rather than landing at the note top.
- Footnote and block landing use the same top-level block index as headings, so
  a footnote at the very end of a note lands in view where the list cannot
  scroll it to the top.
