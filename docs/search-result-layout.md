# Search contents result layout (#772)

Search results use the full list width, including the selected/hover background.
Rows size to their contents: a title, a muted folder breadcrumb, and a single-line
matching snippet. A hidden link destination retains its additional explanation
from #758. Empty snippets do not reserve a line. Secondary text remains 12 px.

The list is bounded by the existing 100-result query limit. A content-sized
scroll container replaces the uniform-height list, whose fixed 88 px allocation
left blank space even when a result had no snippet. Keyboard navigation scrolls
the actual selected child into view. Click, Enter and jump-to-match use the same
result and original snippet as before.

Plain-query title and folder words receive Unicode-safe literal highlights.
Tantivy remains authoritative for body snippets and search ranking; label
highlighting does not reinterpret advanced query syntax. One-line snippets retain
up to 29 characters of leading context before the first highlight, so a repeated
heading cannot push the matching word beyond the visible line. The original
snippet and its match offsets are retained for opening the note.

## Reproduction and limits

Baseline: #771 head `197ddabad045edecd5841874e1d7124af53ea9e5`.
Native Linux on CT141, Xvfb, isolated HOME/XDG, public synthetic Markdown;
vendored gpui-kit verified against the complete patch stack before building.
The body, alias and hidden-target fixtures reproduce the narrow selection and
88 px row gaps. Snippets are visible at both 1x (96 DPI) and 2x
(`GPUI_X11_SCALE_FACTOR=2`, 192 DPI), so the missing-snippet report from Mac beta
8326 has not been reproduced by changing Linux scale. This is not evidence that
the Mac defect is absent. Verify the reported query and corpus on Mac after
publication.

Evidence is kept in `~/.cache/tessera-qa/772/`; before/after light and dark captures
use the same synthetic vault and machine. Do not publish private vault notes.

## Acceptance checks

- Body and alias hits: one visible snippet, with the searched word highlighted.
- A match in a hidden target: highlight the alias and retain the muted reason.
- A filename-only hit: highlight the displayed title and omit an empty snippet.
- A `path:`-only hit: highlight the matching breadcrumb component.
- Down/Up past the viewport: keep the selected row visible; Enter opens it.
- At 2x scale: retain full-width selection and visible matching text.
- macOS after publication: repeat Oleg's `kara` query in the reported vault,
  in both themes, and check the previously missing context against the actual
  matching source. Linux synthetic evidence does not close that platform check.

Local automated validation: all 10 `quick_open::tests::` tests pass, including the
5000-note/superseded-query acceptance test, mouse/keyboard opening with
jump-to-match, Unicode label marks, one-line preview offset preservation, and
rendered row width/height assertions. Shell `clippy --tests -- -D warnings` and
shell `fmt --check` pass. The vendor patch stack is verified independently;
formatting is scoped to the shell package rather than rewriting vendored code.

| Theme | Before | After |
|---|---|---|
| Light | [Baseline](https://oklb.uk/proud-sparrow-6798) | [Compact rows](https://oklb.uk/silly-otter) |
| Dark | [Baseline](https://oklb.uk/calm-lynx) | [Compact rows](https://oklb.uk/jolly-mole) |

Native title-only and `path:`-only controls show highlighted labels without an
empty body snippet. The 15-result keyboard control reaches the last item and
Enter opens that exact note at the highlighted match. The final build also
retains full-width selection and visible snippets at 2x scale. UI rules 1–10:
no additional window/frame, short muted breadcrumbs, secondary text at 12 px,
compact rows, platform shortcuts, and unchanged match navigation.


A display snippet omits an exact leading repeat of the displayed title, at a
word boundary. Body highlight offsets are remapped, and hidden-link explanations
survive even when the repeated title was the whole visible snippet. The original
search hit remains intact for jump-to-match. Frontmatter parsing/filtering belongs
to #746; this shell-only projection deliberately leaves `title: Kara roadmap…`
unchanged and has a regression control for that boundary.
