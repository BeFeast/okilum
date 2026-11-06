# Native math in Read and Live Preview (#1645, #2596)

Point 14 of research #1537, accepted 2026-10-24. Live Preview draws inline
`$…$` formulas instead of leaving their TeX raw.

## Behaviour

- **Caret away:** the formula is drawn in its place on the line: Greek,
  operators, relations, super- and subscripts, simple fractions, roots,
  accents and `\mathbb`/`\mathcal` letters, with letters italic. The `$`
  delimiters are concealed like other Markdown syntax.
- **Caret in or at the edge of a formula:** its exact source shows in the line
  (`$x^2$`), and a card under the formula draws it in two dimensions
  (stacked fractions, raised and lowered scripts, limits on sums, a root
  with its bar). The card is an overlay: no text moves. It follows the caret
  and disappears when the caret leaves the formula or the editor loses focus.
- **TeX Okilum cannot draw** (an unknown command, an unclosed brace, a double
  superscript, `&` alignment): the line shows the exact source, delimiters
  included, and the card states the reason in the danger color instead of
  showing an empty box.
- Read uses the same linear inline rendering, including italic/bold runs,
  paragraph wrapping, selection and search. Unsupported inline TeX shows its
  exact source with delimiters. Literal code and money stay unchanged.
- Display math (`$$…$$`, standalone on one or multiple lines) uses the same
  native two-dimensional layout as the caret card in Read and Live Preview.
  In Live Preview, a caret, selection, IME composition or replacement touching
  that block reveals its exact source. Leaving it draws the formula again.
  Source mode and canonical Markdown bytes remain unchanged. Display math in
  a rendered callout uses that callout's existing source reveal boundary.
- Unsupported display TeX retains its source and a visible error reason;
  it never becomes an empty formula. The supported TeX subset is unchanged.

## Conservative detection

The classifier uses comrak's dollar-math parse (opening `$` not followed by
whitespace, closing `$` not preceded by whitespace nor followed by a digit, one
line, `\$` escaped), so TeX is never read as emphasis (`$a_i b_j$`). On top of
it, `okilum_core::tex::is_inline_formula` refuses a bare amount: a signed
number with separators and at most one `k`/`M`/`B`/`%` suffix. So
`~$1.5k/mo and $2$`, `$5 and $10`, `costs $5,$10` and `pay $2$ now` stay text,
and their paragraphs keep rendering their other formatting. A `$` left as text
no longer forces its paragraph to raw source.

The Reader's internal inline-math projection applies the same amount rule, and its closing
`$` must be the next unescaped one, so `$1.5k/mo and $2$` is no longer read as
the formula `1.5k/mo and $2`.

## Rendering choice

There is no third-party TeX engine: `okilum_core::tex` parses a bounded TeX
subset (4 KiB per formula, nesting depth 24) into a small layout tree. The
in-place rendering is that tree written on one line in Unicode, so it uses the
editor's own text layout, wrapping, selection and hit-testing; the card lays
the same tree out with GPUI flex elements. No raster, no background job, no
new dependency. A small gpui-kit inline-parser extension replaces tagged
Reader math images with ordinary styled paragraph text; no gpui core changes are needed.
The tags carry exact-source fallback labels and are presentation data only.

## Cost on the editing path

- Classification (off the UI thread) gains comrak's dollar scan.
- Each composition looks up formulas outside the caret in a per-classification
  cache (one parse per distinct formula) and, only when the note has a drawn
  formula, copies the display text once to splice them in. That copy is
  bounded by Live Preview's 64 KiB presentation cap.
- The card looks only at the caret's line and parses only the formula under
  the caret, once per change of its TeX.

## Not verified here

Native GUI rendering (glyph coverage of the note font for symbols such as `∑`
and `ℝ`, card placement near window edges, IME inside a formula) was checked
only through headless GPUI tests, not on a real display.

## #2596 verification

The required hosted gate covers the conservative dollar rules and projection
source mapping. The `math-native` workflow executes Read selection/search/source
copy, stacked display fractions, unsupported-TeX fallback, and Live Preview
render/reveal with CRLF source on macOS and Windows. A Windows mutation removes
the Read inline/display renderer registrations, requires both compiled tests to
fail at runtime, then restores the exact bytes and reruns the complete math tests.
These native-runner GPUI tests do not claim installed tester acceptance.

Math uses an internal image tag rather than a nested Markdown link, so formulas
inside link labels retain the surrounding link. The inline parser consumes that
tag before image resolution; no image or network request renders the formula.
