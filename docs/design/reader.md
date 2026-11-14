# Reader design spec — direction A «Calm»

Tracking: [#348](https://git.oklabs.uk/BeFeast/okilum/issues/348). Approved by Oleg on
2026-10-04 from the interactive mockup (direction A):
[html.me.uk/t/okilum-reader-348/reader.html](https://html.me.uk/t/okilum-reader-348/reader.html?dir=A).
The mockup is the visual reference; this file is the contract. Where they
disagree, this file wins.

The reference mood is T3 Code: neutral palette, thin dividers, icon controls,
breadcrumbs, a folder tree, well-set Markdown in a readable column.

## Scope and delivery

| Release | Contents |
| --- | --- |
| R1 — shell | Icon toolbar, no idle status text, find bar only on ⌘F, panel headers, Reader tokens, Light/Dark/System, document typography and column width |
| R2 — sidebar | Folder tree (domains → PARA → notes) from #335, compact Search in the header |
| R3 — right panel | Table of contents above «Linked from» (#337) |

User-selectable themes beyond Light/Dark/System and a theme picker are
[#349](https://git.oklabs.uk/BeFeast/okilum/issues/349); see §Themes.

UI strings stay in English. The mockup's Russian strings show content, not
localisation.

## Layout

Oleg's toolbar organization decision of **2026-10-08 (#767)** supersedes the
original combined titlebar and document controls:

- **App titlebar (46 px).** Sidebar toggle, the project switcher, Recent ▾,
  Pinned ▾ and one vault Search at the left (#1508, see «Title-bar navigation»);
  loading feedback, app More and right-panel toggle at the right. Native window
  controls and dragging remain unchanged. Appearance and Settings are in app
  More rather than separate toolbar buttons.
- **Document header (48 px, pinned).** Back/Forward and Reveal in sidebar precede folder breadcrumbs
  and the note title (#1756). Clicking a folder breadcrumb reveals and expands
  that folder in the tree; Reveal opens a collapsed sidebar and selects the note.
  The Reveal button is disabled for a document outside the current tree. At narrow
  widths it yields to the title; Reveal remains in note More. Clicking the title
  renames inline. Read/Edit use book/pencil;
  Edit also has Live Preview/Source (eye/code). Find, width, Maximize/Restore
  and note More follow (#1909). Maximize sits beside width in this document
  header; its shortcut and reversible panel state remain unchanged. Save
  appears only while dirty; save/conflict feedback keeps its existing behavior.
- **File headers (Variant A, #2795 / #2806).** The same pinned 48 px mount
  has seven erased slots: navigation, name and state, Find, view switch,
  viewer actions and Copy, file actions (Edit/Read, dirty Save, Open with),
  window actions (width, Maximize, Document actions). The view-switch slot stays empty
  until its viewer opts in. Viewers provide read-only `ViewerChrome` values
  and entity callbacks; shared builders do not match on viewer types.
  Every header control is at least 28 px high. Thin token dividers separate
  groups. Copy holds path, vault path, wikilink and, for text viewers only,
  file content. Content comes from exact UTF-8 disk text read on a worker
  (64 MiB maximum, including a file growing during the read), or the unsaved
  editor buffer. The toast names its size; no formatted preview is copied.
  Open with holds the default app, installed editors under Open in, the
  platform file manager, Open in terminal (the file's folder, #2837), and
  Quick Look on macOS only.
  Labels are opt-in from about 1,400 px of document width. The pure overflow
  policy reads the viewer's `overflow_order` table plus its action descriptors,
  and drops labels, Reveal in sidebar, width and Maximize, viewer actions,
  Open with and Copy (as submenus), then narrows Find to 128 px and an icon
  (large files keep a field), then collapses the view switch. It protects
  120 px of file name. Edit/Read, dirty Save and Document actions never leave
  the row. Notes retain their existing layout and pinned Maximize.
  PDF supplies zoom out, fit to width (width glyph), zoom in to slot 5;
  their tooltips use live bindings and their overflow items remain functional.
- **Code/data header (#2809).** CodePreview supplies Table / Tree / Formatted /
  Raw through the shared segmented control (28 px hit areas, token border and
  semibold active label). The sidebar Tree / Now uses the same component.
  Code view labels collapse to icons plus the active label before Fold/Unfold
  overflow. The view switch and Document actions stay in the row. One-view
  files omit the switch. ZIP combines its breadcrumb and one-button view menu
  with the same compact action builders; it has no second code toolbar.
  Read-only and invalid badges sit by the name; one bottom line owns diagnostics,
  large-file reasons and paged position/slider. Code/data read views use the
  shared header Find. With «Show labels on toolbar buttons» on, Fold all,
  Unfold all, Copy, Edit and Open with carry labels from about 1,400 px; the
  overflow function drops those labels before anything else leaves the row.
- **Tables (#2812).** CSV, TSV and the Table view of a JSON array have no rows
  of their own: Find in loaded rows, Table / Raw (four views for an array),
  First row is header and Reset sort sit in the header; Copy cells is in Copy
  and Open in DuckDB in Open with; counts, notices and Load more rows share the
  bottom line. Raw text loads on first use; a hidden table runs no projection.
- **YAML, TOML and XML (#2813).** Tree / Raw at any size with semantic rows,
  Find in Tree, and per-format signals: `serde_yaml` decides «Not valid YAML»;
  TOML and XML only report «Syntax problem» and keep Tree.
  Focus changes re-render the GPUI cached header; source edits,
  query typing and page changes do not invalidate its slot key.
- **File Edit header (#2811).** Edit keeps this header and its shared Find.
  Raw stays active; other views and folds are disabled with live Read hints.
  One toolkit Format split button owns Format / Validate / Repair, with Language
  in its menu for known extensions. The split reads as one control (#2884):
  both halves share the header button height and top, one outline with rounded
  outer corners, a straight seam with a 1 px divider, hover/press per half. Unknown/absent extensions can show Language
  separately. Read, dirty Save, Format, the field and Document actions stay at
  700 px; the view switch becomes one menu. Live sibling overlays own changing
  button/badge state without rebuilding cached slots on typing. One bottom line
  owns results/refusals/conflicts; successes and stale outcomes use toasts. A
  lower Repair sheet preserves the editor viewport, with Cancel and one-Undo
  Apply. A strict JSON worker after 120 ms drives badge, line and diagnostics.
  Read restores the previous view/folds/part/query from a small handoff.
- **One Find component (#2807).** Notes retain their floating bar, selectors,
  match count, case toggle, stepping and two-press Esc behavior. A file opts in
  with a target that owns its query, count/case/step capabilities and clear/focus
  callbacks. Its bordered header field is 200 px, narrows to 128 px without the
  key hint, then becomes an icon; large-file targets retain the field. Focusing
  expands it left toward 272 px over the title, clamped to the measured space
  after Back/Forward. The field remains inside the header; neighboring controls
  and content do not move. The cached header reserves the slot while the live
  Find controls mount as a measured sibling overlay, like Save. Query, count,
  case changes do not invalidate the cached header slots or SidePanels. GPUI focus
  changes still re-render the header; text typing does not. ZIP is the first file
  target: Find in entries,
  no count/case/steps, Enter returns to the list retaining the filter, and Esc
  clears and returns in one press. With entry-pane focus Find belongs to that
  pane. The entry's 48 px compact bar contains its archive/entry crumb and PDF
  zoom or plain-text Open action; it has no history controls.
- **Code/data Find and Tree (#2810).** Tree, Formatted and Raw share the same
  header field. Tree and long-line Raw search the whole immutable document on a
  worker, including hits across parts. Ordinary text uses a revision-bound
  editor adapter: query/source changes coalesce for 60 ms, then Rope flattening
  and literal Unicode matching run on a worker. No source edit scans a document
  on the UI thread. Both targets remember Match case and unfold a hit. Enter and
  Shift+Enter step; Esc clears marks and retains the selected visible hit. Tree
  folds back Find-only branches outside that hit's ancestor path. The placeholder
  is “Find in whole file” for paged text or a file over 4 MiB, “Find” otherwise, and
  “Find in loaded rows” while a table (CSV, TSV, JSON array) is active (#2812).
  Bare/native and ZIP hosts reserve an icon
  at compact widths and open the same field over their row. No legacy whole-file
  row or toolkit search strip remains in code/data read mode.
  Tree hides its synthetic document root and starts with top-level keys at
  depth zero. Semantic rows show a key/ordinal plus scalar preview or key/item
  count. Empty containers use `{ }` / `[ ]`. Chevron buttons, 32 px rows and a
  fixed 60 px two-action slot follow the app's component sizes. Keys use regular
  code text, scalar values the active syntax theme and counts the faint token.
  Before the first Tree paint, a bounded printable ASCII plus generated ellipsis/middle-dot glyphs set is prepared at
  GPUI's finite subpixel phases under an empty content mask, in every foreground,
  count and syntax-value colour used by Tree. This covers macOS colour-dependent
  dilation as well as numeric punctuation and varied ASCII keys. The cache is
  per window/font/size/scale/colour set; row sizes and shaping stay unchanged. Native scroll probes
  count bitmap rasterizations separately from raster-bounds queries.
  Copy node and
  Show in raw text appear on hover/selection; a row context menu supplies those
  actions and Fold/Unfold with live shortcuts. Up/Down move; Right expands or
  enters a child; Left collapses or goes to the parent; Enter shows the node in
  Raw at its exact part. Ctrl+Shift+C copies that node, including on Mac; notes
  retain Copy as Markdown in their own context. Other matches use the highlight
  token; current matches use warning plus the accent bar. Scrollbar marks use
  sorted node lookup, capped at 1,000 evenly sampled hits. Their dedicated
  `findMark` / `findMarkCurrent` tokens have at least 3:1 contrast against both
  the `findMarkTrack` raised track and the canvas in every theme; text-highlight
  backgrounds are not used for scrollbar ticks. Overlapping opaque other-hit
  ticks are painted as their exact union, preserving gaps and the 1,000-hit
  sample while avoiding repeated quad submissions on each scroll.
- **HAR captures (#2836).** Requests and Source are header views; Source is the
  JSON viewer with Requests added. The header Find filters the request list;
  its summary and reading progress are in the bottom line. Details sit under
  the list behind the shared split handle (docs/data-file-viewer.md).
- **Simple file viewers (#2807).** ZIP extraction lives in viewer actions, with
  a filled, pinned Extract all button. Its summary is in the bottom line.
  Canvas uses a 28 px read-only lock badge, header Fit/zoom and percentage;
  percentage sits between Zoom out and Zoom in and yields first; pan/zoom hints
  live in the bottom line.
  A standalone image's Copy menu offers the same checked Copy image command as
  an embedded image. Video retains its card and Open with default app button.
  Drawing files contribute Expand/details, Open in Excalidraw and Copy drawing
  details through their loaded drawing handle. Drawing details opens the full
  diagnostics in a dialog, including from overflow; embedded rows stay intact.
- **Logs (#2808).** Find in records is the same header field, with no count,
  case toggle or stepping; Enter retains the filter and focuses records, Esc
  clears it and returns. The former Filter log command remains an alias for
  existing user bindings. Level, Time range and Follow are labelled state
  controls, filled while active. Query, Saved views, Columns, Export rows and
  Files are icons. Files, Export, Columns and Saved views overflow first, then
  Follow/Time/Level keep their icons and carets; Open with and Maximize yield
  next. Find, the three applicable state controls, Query and More stay.
  A single 40 px secondary row appears for an opened Query, nonempty query,
  chips (including disabled chips), regex editor/error or investigation. It
  contains query/error, Recent queries, Regex, chips and one Clear all; regex
  editing occupies that same row. Multi-line regex diagnostics show their cause
  on one line and keep the full diagnostic in a tooltip. Investigation Back, breadcrumb and loaded
  count stay in it at 700 px too. Narrow chips use one counter popover.
  The message-field chooser lives in Columns. Logs have no Copy header button:
  Document actions > Copy contains raw line (with a reason while disabled),
  file content and the three path forms, each with a toast. Match count, source,
  window scope, rotation/reset notices and save-view feedback use the bottom bar.
  Standalone Log hosts use the same descriptions and erased action slot in their
  own compact header. Embedded `.okilog` headers plan overflow, labels and chip
  folding from the allocated pane width, including with both side panels docked.
  Follow has distinct start/stop hints. Regex help remains available beside the
  pattern and explains named groups plus reopening the file and stopping Follow
  when applying a pattern. During a background refilter the bottom bar retains
  the source total and shows a separate “Filtering…” label.
  Histogram, row/column menus and details retain their behavior.
- **Viewer bottom status line.** Empty descriptions render nothing. Otherwise
  it is 28 px high with a top `border-subtle`, a leading icon, a bold label,
  12/16 muted text, at most two actions and an optional trailing control.
  Truncated text has a full tooltip. Warning uses `warning-tint` with `text`
  on it (AA, AAA for High contrast); neutral uses `surface`. Logs first reuse
  the component without changing their existing text or controls.
- **Labels.** Glyphs are the default. Settings → Appearance → “Show labels on
  toolbar buttons” persists globally; mode labels appear only when the measured
  title and controls fit. Find yields to More before the title truncates, labels
  yield to glyphs, and Back/Forward and Read/Edit remain available.
- **Note More.** Reveal in sidebar, Find, mode, Rename, Move, history, platform
  Reveal, Copy path, Open in new window, Move to Trash and Close. New window uses
  the existing vault lifecycle; split view is outside this change.
- **App More.** Settings, keyboard shortcuts, creation/open/recovery actions,
  sidebar preferences, Appearance and application commands.
- **Panels** are separated from the document by one 1 px `border-subtle` line. The
  left panel names its vault. The ordinary right panel has no panel header;
  its sections retain their own headings (#1910). Closing
  uses the title-bar toggle or panel shortcut; overlay panels also close on
  an outside click. Esc only dismisses local transient UI (#483). Panel headers have no close button.
- **Panel defaults.** Each panel's open/closed state is remembered next to its width
  (`reader-layout.json`). Until R2/R3 ship useful panel content, R1 keeps the
  current defaults (both closed). From R2 the sidebar defaults to open on docked
  widths; from R3 the right panel does too.

### Compact window (< 1000 px document area, reference 900 px)

The accepted #321 rules stay: below the dock threshold panels overlay the
document (shadow `shadow-lg`, 48 px of document stays exposed), at most one
panel at a time; an outside click closes it and returns focus to the document. Both panels start
closed. When resizing or desktop tiling crosses into compact mode, panels hide
automatically so the document reflows without being covered (#677). Their wide
visibility and preferred widths remain intact and return on expansion. A panel
opened explicitly in compact mode still uses the overlay. Selecting a note from
the sidebar dismisses that overlay and focuses the document, retaining the wide
sidebar preference; expanding a folder keeps the overlay open. App and note
controls retain the same grouping at compact widths, with note actions
overflowing as described above.

## Find in note

The main toolbar search glyph opens vault-wide **content search** (#624), with
Ctrl+Shift+F / ⇧⌘F in its tooltip. **Find in note** lives in the document's **…**
menu and in its header as a distinct `TextSearch` glyph, with Ctrl+F / ⌘F.
Quick open (Ctrl+K / ⌘K) is the palette described under «⌘K palette». The same Find action works in preview
and Edit source. Both match literal text without case by default, including
Unicode letters. The optional Aa / Match case switch starts off; its explicit
choice is remembered globally in the application UI store outside the vault (#682).

The following floating-bar rules apply to Markdown notes; opted-in file viewers
use their header field as described above.

- Hidden by default. ⌘F / Ctrl+F opens it; ✕ closes it and clears marks.
  Opening it keeps docked panels; only a compact overlay closes.
- Every search field (find, quick open and full-text palette) has a clear
  button (×) inside while it has text; clicking it clears and keeps focus.
  Esc in a non-empty field clears it; Esc in an empty field closes find or
  the palette (#378).
- It floats at the top-right of the document area, 10 px from the header and
  18 px from the right edge, over the document. The document does not reflow
  when it opens. The bar uses the `popover` surface, `border`, radius 8 and `shadow-sm`.
- Contents: search icon, input (220 px), «2 of 5» count (`text-muted`, tabular
  figures; «No matches» when empty), Previous (⇧⏎), Next (⏎), Close (Esc).
- Marks: every match `find-match`, the current one `find-current` with a 1.5 px
  `find-current-ring` outline. The marks are painted inside the vendored text
  view (`scripts/patches/0008-text-view-search-highlights.diff`), which still uses a fixed amber; moving it onto
  these tokens is a small follow-up patch, not part of R1. Every theme already
  defines `findMatch` and `findCurrent` for it.

## Status text

Idle status is never shown. «Ready» and «Document not yet available» labels
disappear. While loading is active, the header shows a 14 px spinner and the
phase text (`text-muted`, 12 px) after the breadcrumbs, plus Cancel. A failed
load shows the phase text in `danger` plus Retry. Operation feedback uses the bottom notification overlay without reflow (#576):
four seconds for ordinary feedback, eight seconds with Undo, ×/Esc to close.
Ordinary error toasts expire after eight seconds of unhovered reading time;
hovering or focusing their notification stack pauses the timer. Actionable editor recovery
notices and ambiguous-link choices persist until dismissed with ×/Esc. File-access
failures use plain language and keep the draft; original errors go to the diagnostic
log. Rename previews list updated notes separately from the collapsed “Links not
updated” details and use singular wording for one note. Recovery
offers use a four-second toast only when a newer draft exists; entering source mode
can still restore it later. An unclean launch with no unsaved draft is silent. No
notification adds a full-width row. History actions use a compact floating toolbar.

The unreadable-items list uses note names without Markdown extensions and folder
breadcrumbs to distinguish duplicates. Each row explains the unavailable item
in plain language. Full paths, original errors and the diagnostic log location
are behind a technical-details disclosure and included in Copy. Details, Copy
and Retry use compact glyphs with tooltips; Retry closes the list and refreshes.

iCloud placeholders are a distinct pending-availability state, never unreadable
items or attention rows (#1837). The header shows a muted “N files still in
iCloud” count while any remain. On macOS an explicit “Download all” action submits
the known placeholders to iCloud on a background worker. Its tooltip and completion
notice explain Finder → “Keep Downloaded”. The action is disabled during request
submission; submission does not claim that files have arrived. Failed requests
report a count and can be retried. Switching vaults or closing the window stops
submission of the remaining requests. Indexing inspects metadata and never requests a
download; filesystem materialisation notifications reconcile the source and clear
the count without Retry or reopening. A displayed document keeps its last readable
content (and any editor draft) while its source is pending. Ordinary read errors
on other files retain their normal attention UI; damaged encoding uses the inline
Repair flow below. Existing partial-inventory and
link-ambiguity safeguards also apply while cloud sources are pending.

## Typography

Fonts stay the brand pair: Noto Sans (UI and text) and Cascadia Code (code).

| Role | Size / line height | Weight | Notes |
| --- | --- | --- | --- |
| Chrome (buttons, tree, panels) | 13 / 18 | 400, 500 for current item | was 14 |
| Panel section label | 11.5 / 16 | 600, `text-faint` | sentence case |
| Body | 15.5 / 25 (toolkit default 1.618) | 400 | was 15 |
| H1 | 30 / 36 | 600 | tracking −0.012 em, margin-bottom 0.5 em |
| H2 | 21 / 27 | 600 | margin-top 1.9 em, bottom 0.55 em |
| H3 | 17 / 23 | 600 | margin-top 1.5 em, bottom 0.4 em |
| H4–H6 | 15.5 | 600 | |
| Inline code | 0.86 em | 400 | `code-bg`, 1 px `code-border`, radius 4 |
| Code block | 13 / 21 | 400 | `code-bg`, 1 px `code-border`, radius 8, padding 14×16 |
| Table | 0.92 em | header 600 `text-muted` | row rule `border-subtle` |

The document column has a maximum width of 740 px including its horizontal padding:
a left gutter of 48 px and 40 px on the right, so lines are at most 652 px, about 75
characters. The gutter is the same in Reader, Source and Live Preview, so text keeps
its x when the mode changes; a revealed heading marker (`##`) hangs in it in Live
Preview (#1034). On narrow windows it shrinks linearly to 24 px at the 600 px minimum
window width. The column is centred in the available document area. The
padding is 44 px at the top. The `document-end-space` token is
`max(120px, 0.30 × window viewport height)` (#419), inside the scrollable
content after the last block. Reader and source editing use it; the editor
rounds up to a whole text row. It updates when the window resizes, allowing
the last line to scroll to a comfortable reading position.

## Spacing and shape

4 px base grid. Common steps: 4, 6, 8, 10, 12, 16, 24, 40.

| Element | Value |
| --- | --- |
| Header / panel header height | 46 |
| Tree / list row height | 28, radius 6, indent 14 per level |
| Icon button | 28 × 28, icon 16, radius 6 |
| Sidebar / right panel default width | 280 / 280 (`reader_layout::DEFAULT_PANEL_WIDTH`; resizable, persisted, min 200) |
| Toolbar controls | 28 high, icon 16, radius 6; optional mode labels yield to glyphs |
| Callout | padding 12×16, radius 8, 1 px border |
| Popover / palette | radius 8 / 12 |

## Color tokens

**Rule:** components read colors only through semantic tokens:
`brand::Palette` for interface surfaces and `brand::ReaderPalette` for
Reader-specific roles. A literal color (`rgb(…)`, `hsla(…)`, `0x…`) in a Reader
component is a review failure. New roles become new tokens first.

Interface tokens (existing `interface-tokens.json` 2.0.0, unchanged):

| Token | Light | Dark | Use |
| --- | --- | --- | --- |
| `canvas` / `surface` | `#ffffff` | `#202226` | document, header, panels |
| `surface-raised` | `#f7f7f8` | `#292c31` | secondary fills |
| `sidebar` | `#f1f2f3` | `#292b30` | left sidebar |
| `text` | `#24262b` | `#eceef1` | primary text |
| `text-muted` | `#656b74` | `#a4a8b0` | secondary text, crumbs |
| `border` | `#c4c7cc` | `#60646e` | inputs, popovers |
| `border-subtle` | `#e3e4e7` | `#383b42` | dividers, panel edges |
| `selected` | `#edf0f3` | `#353a43` | current row, hover |
| `accent` | `#0969e8` | `#176bdb` | focus, current-section bar, checkboxes |
| `link` | `#0969e8` | `#71acff` | resolved wikilinks |

Reader tokens (new `reader-tokens.json`, loaded the same way):

| Token | Light | Dark | Use |
| --- | --- | --- | --- |
| `text-faint` | `#8d929a` | `#7c818a` | section labels, counts, kbd hints |
| `link-underline` | `#0969e8` @ 28 % | `#71acff` @ 30 % | resting underline |
| `missing-link` | `#9a5b00` | `#e2b25c` | unresolved wikilink text, dashed underline @ 50 % |
| `code-bg` | `#f6f7f8` | `#26292e` | inline code, code blocks |
| `code-border` | `#eceef0` | `#30333a` | |
| `callout-bg` | `#fffaeb` | `#2a2720` | warning callout fill |
| `callout-border` | `#f3e3b5` | `#4a3f2a` | |
| `warning-tint` | `#fbf5e6` | `#3a3220` | warning bottom line; theme-derived warning over surface |
| `find-match` | `#fde68a` | `#5c4f1c` | text-match background |
| `find-current` | `#fbbf24` | `#8a7420` | current text-match background |
| `find-mark` | `#8a5a00` | `#f2c14e` | other-hit scrollbar tick |
| `find-mark-current` | `#0b57d0` | `#8ab4f8` | current-hit scrollbar tick |
| `find-mark-track` | `#eff0f2` | `#30333a` | scrollbar tick track |
| `find-current-ring` | `#d97706` | `#facc15` | |
| `hover` | `#eff0f2` | `#2a2d33` | row and icon-button hover |
| `selection` | `accent` @ 22 % | `accent` @ 35 % | text selection |
| `callout-important` | `#9333ea` | `#c084fc` | «important» callout accent |
| `highlight` | `#ffd000` @ 40 % | `#ffd000` @ 40 % | `<mark>` background |
| `scrim` | black @ 18 % | black @ 45 % | dims the document under the table overlay |
| `on-status` | `#ffffff` | `#202226` | text on a filled status color |

Status colors (`success`, `warning`, `danger`, `info`) come from the brand
tokens, unchanged.

## Appearance

The Reader ships **System** (default, follows macOS), **Light** and **Dark**. The
existing `AppearancePreference` already models this; R1 exposes it in the More
menu and persists the choice in app config (not in notes). Theme flips are live:
every Reader color is read from the palette on render, so nothing needs
restarting.

## Themes

A theme (#349) is a pure token set: it names every interface and Reader role
above for both a light and a dark variant, and nothing else. Mode
(System/Light/Dark) stays a separate choice, so every theme follows the system
too. Status colors may fall back to the brand tokens; no other role inherits.

| Theme | Key | Character |
| --- | --- | --- |
| Okilum (default) | `okilum` | `interface-tokens.json` + `reader-tokens.json`, the tables above |
| Graphite | `graphite` | neutral greys, slate accent, no hue in surfaces |
| Paper | `paper` | warm off-white, ink-blue links, rust accent; sepia dark |
| High contrast | `high-contrast` | black/white surfaces, AAA (7:1) reading text |
| Nord | `nord` | cool arctic blues after the Nord palette |

Token files live in `crates/okilum-shell/assets/themes/<key>.json`
(`okilum-theme/v1`). A unit test checks every theme in both variants: `text`
and `text-muted` on `surface`, `canvas` and `sidebar`; `link` and `missing-link`
on `surface` and `canvas`; `text` on `code-bg`, `surface-raised`, `selected` and
`hover`; status colors on `surface` and `on-status` on them — WCAG AA (4.5:1),
AAA (7:1) for High contrast. `on-accent` on `accent` is 4.5:1 and `text-faint`
3:1 (4.5:1 for High contrast). A new theme that fails is fixed, not exempted.

**Theme picker** (`theme_picker.rs`): a self-contained row of compact swatch
cards (120 px). Each card previews the theme's sidebar, surface, text, muted
text, link and accent in the variant currently shown. The selected card has a
2 px `focus` ring and a check glyph; ring and glyph space are always laid out,
so selection never moves anything. Cards are keyboard stops (Enter or Space
selects). The choice applies live and is stored once for the whole app
(`reader-ui.json`, key `theme`), never per vault or in notes. Settings →
Appearance hosts the picker below the independent mode and reading controls.
Legacy `appearance.json` is migration input only.

The brand mark keeps its brand colors under every theme. The `highlight` color
is resolved when a block is parsed, so an open note picks up a new theme's
highlight on its next re-parse. The syntax-highlight theme for code blocks stays
the toolkit's light/dark pair.

**Vault colour** (#774, Oleg 2026-10-08): vaults are told apart like Chrome
profiles while the theme stays app-wide. A vault may have one of eight presets
(Red, Orange, Amber, Green, Teal, Blue, Purple, Pink); the default is none, so
a single-vault user sees no change. The colour shows only as an 8 px dot before
the vault name in the sidebar header and a 2 px line along the window's top
edge, drawn over the title bar so nothing moves. Each preset has a light and a
dark value, tested at 3:1 against `canvas`, `surface` and `sidebar` of every
theme. Set from the vault name menu (Vault colour) or Settings → Vault; stored
in `reader-ui.json` (`vault_colors`) by canonical vault root, never in the
vault. Every window of the vault repaints at once. No per-vault theme and no
tinted rows.

**Maximize note** (#1821): the title-bar glyph, View → Maximize Note and
Option+Command+F on macOS / Ctrl+Alt+F on Linux / Ctrl+Shift+Enter on Windows toggle both side panels off
and restore their previous open/closed choices. Preferred widths and the
separate full/normal reading-width preference remain unchanged. The same
action works while editing and retains the editor, draft and selection.
The toolbar stays visible and its tooltip changes to “Restore panels”.
Explicitly opening a side panel exits this temporary view; resizing into a
compact window retains the existing no-automatic-overlay rule. Saved layout
preferences contain the normal panel choices, so closing a maximized note
does not leave a later window with permanently hidden panels.

## Icons

Code view segments use the embedded `view-tree.svg` and `view-table.svg` outline
glyphs; Raw uses `code-xml.svg` and Formatted uses `list.svg`. Segment glyphs keep
the sidebar Tree / Now component’s 14 px size inside 28 px hit areas. Fold all /
Unfold all use `chevrons-down-up.svg` / `chevrons-up-down.svg`, distinct from the
adjacent reading-width toggle’s `fold-horizontal.svg` / `unfold-horizontal.svg`.

Lucide outline icons at 16 px, stroke 1.75, `text-muted` at rest and `text` on
hover. Toolkit names come from `gpui-component`'s `IconName`; the four missing
glyphs are added as Okilum assets under `icons/` and loaded with `Icon::path`.

| Action | Icon | Shortcut / tooltip |
| --- | --- | --- |
| Toggle sidebar | `PanelLeft` | «Notes ⌘\\» |
| Back / Forward | `ArrowLeft` / `ArrowRight` | «Back ⌥←», «Forward ⌥→», disabled at the ends |
| Find in note | `TextSearch` | «Find in note ⌘F» |
| Toggle right panel | `PanelRight` | «On this page ⌥⌘\\» |
| Maximize / restore note | `Maximize` | «Maximize note ⌥⌘F» / «Restore panels ⌥⌘F» |
| More | `Ellipsis` | menu |
| Close find | `Close` | «Close Esc» |
| Find previous / next | `ChevronUp` / `ChevronDown` | «⇧⏎» / «⏎» |
| Log level / time / Follow | `list-filter.svg` / `Clock` / `ArrowDown`, with caret | Live state/action hint |
| Log query / saved / columns / export / files | `SquareTerminal` / `Star` / `LayoutDashboard` / `download.svg` / `Folder` | Live action hint |
| Case sensitive | `CaseSensitive` | toggle |
| Folder / open folder | `Folder` / `FolderOpen` | tree |
| Note | `FileText` | tree, quick open |
| Tree disclosure | `ChevronRight` / `ChevronDown` | |
| Callout warning | `TriangleAlert` | also ambiguous backlink |
| Contents | `list` (asset) | right-panel label |
| Linked from | `link` (asset) | right-panel label |
| Recent | `clock` (asset) | quick open |
| Command | `command` (asset) | reserved |

Every icon-only button has a tooltip naming the action and shortcut.

## Sidebar (R2)

**Recognized agent files (2026-11-01).** Tree, search Agent files and Now share
the exact, case-sensitive filename classifier: `AGENTS.md`, `CLAUDE.md`,
`GEMINI.md`, `SKILL.md`, `CONTEXT.md`, `ANTIGRAVITY.md`, at any depth. These
instruction files remain in inventory and can always be opened explicitly;
Tree lists them with Show hidden files, or temporarily on explicit reveal.
Ordinary `agent.md`, `agents.md`, `my-AGENTS.md` and backup suffixes do not match.
Search Agent files also lists the vault-root `@AgentContract.md` and the default
search views leave it out (#2940); Tree keeps it under the @ rule.
Only @ and dot path segments are hidden; underscore paths are ordinary (#2847).
Templates follow the same rule, while @ index rows lead their visible folder.
This shared visibility switch does not rename or delete files.
Scanner-excluded generated service directories remain separate from browsable
hidden files and are not added back by this switch.

**Folder agent rules (#1587).** A folder containing an exact direct-child
`AGENTS.md` Markdown file has a compact `@` badge, including while collapsed.
The tree header supplies the badge for its root (vault or project scope).
Clicking opens that file in Notes without activating the folder. The badge
remains available with Show hidden files off. This uses the existing single
View → Show hidden files preference, which also reveals `.claude`, `.agents`
and `.codex` with their contents. Agent dot-folders also stay hidden inside a
configured templates branch. Service-directory exclusions remain in force.

The existing exact-name visibility rule is a superset of shim hiding: real
`CLAUDE.md` / `GEMINI.md` rules retain that existing hidden-by-default behavior.
Only files whose entire trimmed content is `@AGENTS.md` or `@./AGENTS.md`
(allowing BOM, whitespace and CRLF) receive the import-shim tooltip annotation.
This classification is performed by one coalescing background worker per reader,
with a 4 KiB small-file limit and a 4097-byte read ceiling even if a file grows.
Rendering and badge discovery read only the published inventory, never disk.


**Tree scroll context (Oleg, 2026-11-01).** The permanent Tree header opens
the real index as `Vault Index`, with the existing Contents icon and a left-aligned
title. New note, New folder, Collapse all and Show/Hide hidden remain compact
icon-only actions in that row; the index path, Reveal and Pin remain available.
One overlaid breadcrumb row follows the first visible tree item without changing
the list viewport. Its prefix `↑ 4 · Health › Areas › Current` counts only earlier
root-folder rows fully scrolled above the viewport in the current visible tree
projection. It excludes the current root ancestor, nested folders and filtered
items. Tooltip and menu list those earlier folders; selecting one uses the same
scroll-only navigation as an ancestor click and preserves expansion. The prefix
disappears at zero. At a root-folder boundary its name remains beside the prefix;
no additional sticky row or stacked folder headers are introduced.


The panel header gives the vault name width priority and a full-name tooltip.
Clicking it opens vault actions. New note, New folder and Collapse all folders
follow. Measured text determines overflow: Collapse all, then New folder, then
New note move into one More menu before the name truncates. There is no total
note count or second search button. The app search retains keyboard focus and
Enter/Space activation; the quick-open shortcut remains unchanged.
Neither panel header has a close (×) button (#442).
Title-bar toggles and ⌘\ / ⌥⌘\ (Ctrl outside macOS) control panel visibility.
Esc in preview or source leaves panel visibility unchanged (#483).
In compact overlay mode, a click outside closes the panel; the outside
click is consumed before reaching the document. While an overlay panel is open,
scrolling over the exposed document is blocked so the background stays in place.
The sidebar has no tree-filter
mode or query state. Beneath the header is the tree of real folders and notes
(#335 rules: real directories only, root-relative identity, no inferred
grouping). Top-level folders are labelled in weight 600 without a folder icon;
deeper folders have `Folder`/`FolderOpen`; notes have `FileText`. The current note
is highlighted with `selected` and weight 500 wherever its row is listed. Opening
a note does not move the tree by default: its row is selected only if it is
already listed, and nothing expands or scrolls; Reveal in sidebar does that on
request. Settings → Appearance → «Show the open note in the file tree» (#1762,
off by default) makes the tree follow, as Auto-reveal does in other editors:
opening a note expands its folders, selects it and scrolls only when the row is
off screen. It never takes keyboard focus or opens a closed sidebar, waits while
the tree is being wheel-scrolled (until it has been still for 1.2 s), and does
not open a closed folder with more than 200 entries — that folder is selected
instead. Folders the user already opened stay as they are.
Markdown tree rows use the file stem by default (#1834). Settings → Appearance →
File tree labels can select Title (first H1 → frontmatter title → file stem);
the row label and its sort key change together, live in every Reader window.
Non-Markdown files retain their full name. The folder's `@index.md` leads
its folder, but its label follows the same setting. Reveal and rename continue
to use the path identity; note headings, Quick Open, search and Linked from keep
their titles in either mode.
There is no flat note-list mode in the sidebar and no item cap (#369). Folders whose name starts with `@` or `.` are
not shown; their notes stay reachable through links and search. Archive folders
(`Archive`, `4 Archive`, `Архив`) are muted and open only on request or to reveal
the current note. Navigating to a hidden note shows its branch muted until the next note;
hidden rows use subdued styling without a text badge, and the current row keeps
its normal selection contrast. The tree never highlights a neighbour instead. «Show hidden files» (eye button on the Folders header, … menu, ⇧⌘. — bound as `cmd->`, the way macOS reports it) lists
them all and is remembered per vault (#395). Recent, Pinned and Inbox follow the
same rule and update as soon as it changes; quick open and search still find
hidden notes (#635). The sidebar header shows four actions in fixed
slots — New note, New folder, «Collapse all» and the eye (#1756) — idle and hovered alike; hover only
raises their contrast and never adds, hides or moves an icon (#1512). A narrow
sidebar moves only Show hidden into ⋯; New note, New folder and Collapse all
stay visible. Reveal belongs beside the document breadcrumb, including when
the sidebar is closed. ⌥-click on a folder, ⌥→/⌥← on the selected folder and the
folder context menu («Expand/Collapse all subfolders») act on the folder and
everything below it, as in Finder. There is no global «Expand all» (#410).
F2/Enter starts inline rename for notes and folders (#578); Right/Left expands/
collapses, and Left on a child selects its parent. A folder-label click selects;
double-click, its disclosure glyph, or ⌘↓ (Ctrl↓ on Linux) toggles it.


### Title-bar navigation (#1508: #1517, #1519)

Variant A, decided 2026-10-22. Left of the title bar, after the sidebar
toggle:

- **«<project> ▾» switcher (#1519).** Outside a project it reads «<vault> ▾».
  It opens the shared Projects picker (#1520) as a popover left-aligned under
  it: search, kind filter, recent projects first, then vault domains. Choosing
  a project enters it through the same revalidating pipeline as the Projects
  section. Inside a project the popover ends with **Leave project**; the old
  «<name> · Leave project» text button is gone. Esc, a second press on the
  switcher or an outside press closes it.
- **Recent ▾ and Pinned ▾ (#1517).** Popover lists: title, the parent folder's
  name under it, and the age for Recent. Recent shows six and «Show all»; its
  footer names the Ctrl+Tab cycle, which is unchanged. Hidden notes follow
  Show hidden files (#635). ↑ ↓ ↵ work inside; Esc closes. Below 960 px the
  two buttons shrink to their clock and pin glyphs.
- **Inside a project** both lists show the project's notes, with a
  This project / All vault segmented switch at the top. The choice is
  remembered per project with the sidebar state (`nav_all_vault`); the
  stores themselves stay vault-wide (Recent keeps 40), and the project is a
  folder-prefix filter.

### Pinned files row (#2789)

Decided by Oleg 2026-11-13 (option b). **Pin** / **Unpin** sits in each
file's menus: the note and file ⋯ Document actions (last before Move to
Trash), the tree row menu and the document title's context menu. Pins are the
Pinned ▾ store, so the tree's pin glyph, the Pinned menu and this row agree.

A row of pinned files sits above the document header whenever one is pinned
and stays while other notes and files open, like pinned tabs. A click opens
the file through ordinary navigation, so unsaved edits follow the usual save
and draft rules; pinning never saves or discards anything. The open file's
pin is highlighted; × or the pin's menu unpins. Pinned folders stay in the
Pinned menu only. Many pins keep one fixed-height row that scrolls
sideways; long names are cut.

Pins are saved with the per-vault sidebar state and survive a relaunch. A
move or rename inside Okilum carries the pin. A pinned file that a complete
inventory no longer has (moved, renamed or deleted elsewhere) is not dropped:
the row shows it struck through as missing until removed, and the Pinned menu,
palette and Now leave it out. A single file opened without its vault has no
pins, so neither the row nor the menu item appears.

### ⌘K palette (#1518)

Quick open (Ctrl+K / ⌘K) is one palette with the sections **Pinned**,
**Recent**, **Projects** and **Files**, in that order; one query filters all
four. Pinned and Recent are the title-bar lists, so inside a project they
follow its This project / All vault switch; Recent leaves out the note on
screen. Projects shows up to five rows from the shared picker, then «All
projects…», which opens the switcher with the same text. Files waits for text
(«Type to search files in <project or vault>») and, inside a project, ranks
the project's files first; it leaves out what Pinned and Recent already show.
↑ ↓ move across all rows, Tab / ⇧Tab jump between sections, ⌘1…⌘4 (Ctrl
elsewhere) to a given one, ↵ opens, Esc clears then closes. Opening it closes
an open title-bar menu or the switcher. Content search (⇧⌘F) stays one flat
list of matches.

### Tree only (#1508, design session 2026-10-22)

The left sidebar is the folder tree and nothing else. Recent and Pinned moved
to the title bar (Recent ▾, Pinned ▾, #1517); Projects, Inbox and Logs are app
sections in the ⋯ menu (#1474). In a vault the Folders header stays, with its
actions; clicking it folds the tree. Inside a project there is no Folders
header at all: the project's tree starts right under the panel header and
cannot be folded away. Scrolling never changes the sidebar's geometry.

The retired controls are gone with the sections: «Collapse/Expand all
sections», «Folders only / Restore sections», ⌥-click on a header and their
shortcuts. Recent, Pinned and the tree's fold are app state per root in the
Reader state directory; nothing is written into notes. Older state files load
unchanged: pins and recents survive, and the old per-section fold and
Folders-only snapshots are ignored.

The earlier variant A «Sections» (#369, #434) — Recent, Pinned and Inbox as
collapsible sidebar sections above Folders — is superseded.

## Right panel (R3)

No panel header; sections have their own headings (#1910). Properties, Contents
and Linked from remain independent sections with their existing scrolling:

- **Properties** — the note's frontmatter, when present, under its own collapsible heading.

- **Contents** — the outline of the current document. Each item is indented 12 px per level. The current
  section has a 2 px `accent` bar on the left and `text` weight 500, and the rest are
  `text-muted`. Rows retain their natural text height; long outlines scroll
  within the section's height limit instead of compressing their rows (#417).
- **Linked from · N notes · M places** (#394, variant A «Source and quotes»,
  chosen by Oleg 2026-10-04; [mockup](https://html.me.uk/t/okilum-reader-348/linked-from.html?v=A)).
  The card header is the *source* note: file icon, bold title, its folder
  right beside it (two notes with one name stay distinguishable), relation
  field pill for frontmatter links, place count, ↗ on hover; clicking it
  opens the note («Open note»). Below it, indented under a 2 px rule, are the
  *places* inside that note: muted text, this note's name bold on a light
  `accent` mark — never link-blue; each opens the source at that line («Open
  at this place»). More than 3 places collapse behind «Show N more». An
  ambiguous link gets `TriangleAlert` and an «ambiguous» pill, never a
  silent pick.
- **Empty sections (#646):** counts appear only when above zero. An empty
  section keeps its header and shows a muted «—» under Contents and a bare
  «Linked from», including while inventory is pending. Hidden-only or empty
  properties also use «—», never «0 properties» or internal property counts.
  Never «0 notes · 0 places».

## Properties (#386, approved by Oleg 2026-10-04)

Mockup: [properties.html](https://html.me.uk/t/okilum-reader-348/properties.html).
Read-only view of the note's leading YAML frontmatter; nothing is edited or
normalized.

- **Right panel open:** a «Properties» section above Contents, collapsible
  (state remembered); collapsed it shows the one-line summary.
- **Right panel closed or compact:** one line above the document —
  «Note · Active · updated 3 Oct · 3 relations · #a #b» — that expands the
  properties in place.
- **Values:** wikilinks open through the same resolver as body links
  (ambiguous → choices, missing → `missing-link` colour and a notice); URLs
  open in the browser with ↗; dates read «3 Oct 2026, 18:40 · yesterday»;
  tags, type and lists are chips, status is an accent chip; `_` keys are
  hidden behind «Show system properties». Invalid YAML says so.
- **Relations:** this note's frontmatter wikilinks count in the summary. A
  note that links here from its frontmatter appears in «Linked from» with
  the field as a pill and the row «field: Target».

## Link destinations (#428)

Internal note, heading and attachment links use the accent colour without an
external marker. Existing missing/ambiguous presentation remains authoritative.
External HTTP(S), mail, telephone and FTP links use the same accent plus a small
trailing ↗. Hovering their text or marker shows the destination domain and URL
(or the full URI for mail/telephone). The marker is presentation only: it must
not enter copied document text or shift source/search byte ranges.

The same distinction applies in tables, Properties and Linked from snippets.
A snippet's highlighted destination retains its existing muted mark; other
external links have their own action/tooltip, without opening the source note.

## Wide tables (#368)

A table wider than the reading column scrolls horizontally and has persistent
cues, including when the clipped cells are empty (#418):

- A 28 px inner shadow at each edge with hidden content (foreground, maximum
  18% opacity). The right shadow disappears at the end; the left appears after
  scrolling. The outer border and corner radius are omitted at clipped edges.
- A quiet **⤢ N columns** Expand badge at the top right, just above the header
  so it never obscures cell content. Always visible, with a stronger hover
  background; N is the total column count.
- A 3 px horizontal track below the table, separated by 4 px. Foreground at
  8% opacity for the track and 28% for its thumb; thumb width and position show
  the visible fraction and scroll position. It remains visible without hover.

The badge opens the table in an overlay over the document at window width with
24 px margins: the same renderer, links and inline code; columns at natural
width, long cells wrap, horizontal scroll only when it still does not fit.
Esc, ✕ or a click outside close it; the document underneath does not move.
Following a link from the overlay closes it. Tables that fit get none of these
cues. There is no separate Expand button below the table.

## List indentation (#2239)

Reader bullet and numbered lists start one rem inside the paragraph edge, using
the existing nested-list spacing. Nesting adds its normal step; wrapped lines
stay aligned with the item's text. This changes layout, never Markdown whitespace
or the editor's Tab/Shift+Tab actions (#2239).

## Onboarding / empty

No root selected: centred card, 440 px wide. It holds the Okilum mark (52 px), the title «Open your
notes», one sentence saying files are read-only, a primary **Open folder…** and a
secondary **Open file…** button, and recent folders below a divider.

## Code block Copy (#429)

Fenced and indented code blocks have an always-visible Copy glyph in the top-right
corner in Reader and unfocused Live Preview (#1985). Its tooltip is **Copy code**.
Tab reaches the action; Enter/Space activate it. After activation a check glyph
and **Copied** tooltip confirm the copy for two seconds without changing the
control's size. Repeated copies renew that interval; changing the block clears
feedback.

Copy writes code content only: no fences or info string, no Markdown container
indentation, while code indentation, blank lines and original line endings
(including a final LF/CRLF/CR when present) are preserved. Inline code has no
Copy action. This does not change the document's selection-copy format.


## Code block language (#430)

Code blocks reserve a 24 px header above the code; a quiet language label sits
next to Copy. Explicit fence languages win, with aliases normalised (cs/c# →
csharp, sh/shell →
bash, yml → yaml, js → javascript, ts → typescript, py → python, rs → rust).
Unknown explicit languages retain their label and the highlighter's plain fallback.

Unlabelled fences use conservative signatures: supported shebangs, nonempty valid
JSON objects/arrays, typed multi-key YAML mappings, a narrow TOML table shape,
distinctive Rust/Python/JavaScript combinations, or multiple known shell prompts.
Rule confidence must reach 95/100 and have a unique winner; these scores describe
signature strength, not calibrated probabilities. Ambiguous text has no label and
no syntax highlighting. Indented blocks are never inferred. Detection skips blocks
larger than 8 KiB or 128 lines and caches both matches and abstentions per
block/resolver. Ordinary views resolve on entering the viewport. The staged
startup document resolves its fences in the background before publication, so
labels and syntax colours accompany the first text frame, including a restored
mid-note position. Label and highlighting share the result.
No source bytes, explicit fence language, selection export or Copy payload change.

Existing highlighter assessment: keep the vendored tree-sitter adapter and its
language/highlight caches. A small bounded heuristic set is preferable here to a
broad classifier dependency: predictable abstention matters more than coverage.

### Linked-note hover preview (#446)

Hovering a resolved internal note link, a Linked from entry, or a quick-open
result opens a bounded preview after 350 ms. Holding ⌘ (Ctrl outside macOS)
opens it immediately, including when the modifier is pressed while stationary.
External, missing, ambiguous, unsupported and unverified links do not open a
preview. Ordinary click navigation is unchanged.

The preview uses the Reader Markdown renderer, including local images, tables,
code, callouts and supported diagrams. It is at most 520 × 420 px, stays inside
the window, and scrolls independently. A heading link lands at its heading;
a missing or ambiguous heading displays an unavailable message. The header's
↗ button opens that exact note and heading. Hover leaves the current document,
reading history and keyboard focus untouched.

The cursor can move from the link into the preview (220 ms dismissal grace).
Escape, an outside click, background scroll, navigation or a changed quick-open
query dismisses it. Preview links can be opened, but do not spawn nested previews.
Target preparation runs off the UI thread; stale results are discarded.

### About Okilum (#464)

All desktop platforms share the same About dialog: app icon, name, tagline,
short product introduction, four capabilities, version/build/update channel,
and repository, release notes, MIT License and third-party notices links.
macOS opens it from Okilum → About Okilum, replacing the standard system panel;
All platforms also expose About in the app More menu (#484). Check for Updates appears only when
Sparkle is available. Linux identifies system-managed updates; Windows identifies
its diagnostic channel and read-only scope. The repository URL has one source of
truth in `about.rs`. Local builds report development metadata explicitly.

### Document actions and empty vault (#474)

The 48 px document header contains breadcrumbs on the left and source/preview
and document More glyphs on the right. Per Oleg’s decision on 2026-10-08, it
stays pinned outside the scrollable Reader and Source viewport, like Obsidian.
Wheel and keyboard scrolling move only the document; fresh and restored sessions
use the same 48 px header, with no collapse or restore-only exception.
Source mode adds a Save glyph (⌘S / Ctrl+S tooltip)
and a quiet unsaved dot; conflict recovery actions remain explicit glyphs with
tooltips. Attachment headers use Copy and Open with menus alongside
Document actions, following the file-slot and overflow rules above. Quick Look
is a macOS-only item in Open with.

Reveal uses the platform's name for the file manager: «Reveal in Finder» on
macOS, «Show in Explorer» on Windows and «Show in File Manager» on Linux, where
it calls `org.freedesktop.FileManager1.ShowItems` and falls back to `xdg-open`
on the containing folder. Shortcut hints use ⌘/⌥/⇧ glyphs only on macOS and
spell out Ctrl/Alt/Shift elsewhere; both come from `platform::labels` (#612).

Document More contains Reveal in sidebar (the document breadcrumb crosshair action),
Edit/Preview, Rename/move, Note history, Reveal, Open in terminal,
Copy path, Open in new window, Move to Trash and Close note.

Open in terminal (#2837) follows Reveal in sidebar row menus, breadcrumb and
title menus, the file header's Open with and note Document actions, and is in
the palette and shortcut catalog (Option+Command+T / Alt+Super+T). It opens
the system terminal in the item's folder; a file opens its containing folder.
Entries inside a ZIP are not files on disk and have no file menu, so the
action is absent there; the archive file itself opens its own folder.
Launcher selection, argument safety and AppImage environment are described
in docs/project-terminal.md.
The app More menu contains New note, Open file/folder, recovery, appearance,
hidden files, About and Quit. Editing actions are absent in Windows diagnostic.

⌘W (Ctrl+W elsewhere) closes the selection to an empty vault screen with Recent
notes and search/new-note hints, retaining the sidebar. Back returns to the
closed document. The empty selection survives restart; another Close closes the
window. Conflicted or failed saves prevent closing and retain the source text.

## Settings (#479)

A separate Settings window opens from the application/More menu or ⌘, / Ctrl+,.
A left list selects Appearance, Files, Updates and Inbox. Changes apply immediately
through the existing user appearance, per-vault sidebar and Sparkle preferences.
Files identifies the originating vault; opening Settings from another Reader
retargets that section. Without a vault it explains that a vault must be opened.
Updates shows the installed version/build and selected update channel. macOS and
Windows use a compact joined Stable/Beta control with shield/flask glyphs and
accent fill on the selected segment; changes persist immediately. Beside it,
the refresh icon button checks through the installed updater. Actions have
tooltips and accessible names; unselected controls have no grey outline. All controls use
the same standard Settings button height, radius and padding; the current
version/build/channel sits directly above. Linux identifies system-package
management and offers compact repository-setup and release-notes glyph buttons;
it does not pretend to switch pacman repositories. Unpackaged builds explain
updater availability.
Inbox shows “Not connected”; no connection controls are enabled in this slice.
The deferred local Excalidraw editor needs no editor-URL setting.

Files also selects a templates folder per vault (default `@Schema/Templates`; no `_Assets` fallback).
“New note from template…” uses a standard popup menu, without a modal or raw
folder path. Escape dismisses without creating anything; choosing a template
starts the existing inline name field. The menu lists Markdown files directly in
that folder, substitutes `{{title}}` and local `{{date}}`, and creates a new note
without modifying the template or overwriting an existing destination. Folder
preferences and template contents are read only for these explicit user actions.

### Note history (#475)

Note More → Note history replaces the right panel with a timeline for the current
note. Compact rows (minimum 36px) show relative age, a quiet byte/line delta badge
(`B` / `L`) against the reviewed current file, and a star for protected versions.
The row tooltip preserves the exact UTC timestamp, save reason and protected
status. Loading and read errors stay in the panel. An arrow button with the
“On this page” tooltip returns to Contents and Linked from.

Selecting a version previews it in the document area without replacing the live
Reader or editor buffer. A compact bottom overlay identifies the version and offers Restore,
Show changes, Source/Preview, Back to current, and Save as recovered note. Link-move
versions also offer Recover whole link move. These actions use the same 28px
ghost glyph buttons and tooltips as the document header (#548); active comparison
and source modes remain visibly selected. Up/Down selects adjacent versions;
Escape or Back to current returns to the live document without closing the panel.
Source is read-only; Show changes compares the replaced line span to the reviewed
current file. Switching notes clears the historical preview.

Restore has no confirmation dialog. It preserves the replaced current bytes in
history and offers Undo in an eight-second toast. Both restore and undo retain the
existing dirty-editor and exact-current-byte guards; a racing edit is never silently
overwritten. Restore failures appear in a dismissible toast. Recovery of unsaved drafts
continues to use its existing dialog.

### Damaged text encoding (#1934)

A Markdown note with invalid UTF-8 opens with replacement characters (U+FFFD).
A quiet inline notice keeps it read-only and offers Repair. Merely opening,
searching, previewing, or trying Edit never changes the original file.

Repair explicitly replaces invalid byte sequences with U+FFFD through the same
revision-bound safe-save path as source editing. The original bytes are durable
in Note history before publication. On success, the notice clears and Edit is
available. A changed source refuses the transaction; a failed transaction restores
its original bytes and removes only verified staging files.

History previews damaged versions with replacement characters, with a clear
notice that Restore and Save as recovered note preserve the exact original bytes.
Restoring the original version to the same note makes it read-only again. Raw
records share the existing History retention limits; interrupted recovery is
protected. They live in application state, outside the vault. Existing text
records are unchanged, and older builds skip the additive raw record kind.

### macOS file previews (#477, step 1)

Opening a non-Markdown file on macOS requests a Quick Look thumbnail in the
existing document view. PDFs show a first-page preview; Office documents, still
images and video posters depend on the installed system provider. The document
header keeps its glyph actions. Space opens full Quick Look. This step does not
add PDF page navigation, text selection or search.

The bitmap fits the available column, with quiet file metadata and a Space hint
below it. Loading and unavailable states retain the header actions. SVG and
animated image formats keep the existing renderer. Linux/Windows retain their
existing file view.

Thumbnail requests use 1024 points at 2x, at most 2048 physical pixels per side,
a 32 MiB encoded-byte limit and a ten-second wait. Each selected file owns one
request; navigation cancels it and releases the bitmap. Requests and decoding are
asynchronous. The file revision is checked before publishing the result; a changed
or missing source yields the unavailable state. Reopening always requests a fresh
preview. Thumbnails are memory-only derived data; they never write to the vault.
The preview is a GPUI image, so palettes and menus retain normal overlay ordering.

“Reveal in sidebar” opens the Notes panel if hidden, expands Folders and focuses
the current note or file using the same action as the document breadcrumb crosshair.

### UI state and new windows (#592)

Okilum restores the last reading environment without an inheritance setting.
Appearance (System/Light/Dark), reading text size (12–24, default 15.5) and reading
width (560–1200 logical pixels, default 740) are global. Their model and persistence
live in `reader_ui_state`; Settings presentation is owned by the separate #623
redesign. #592 does not add or change Settings controls.
Source text scales with the reading size. Windows in the running application
share these values live; new application instances restore the latest persisted
preferences. Independently edited fields merge without replacing other preferences.

Each canonical vault remembers independent panel visibility and preferred widths,
collapsed sidebar sections, expanded tree folders, tree cursor/scroll, Properties
presentation, source/preview mode, the current note (including the empty vault),
reading scroll and Back/Forward entries with their offsets. Window geometry,
display, maximized and fullscreen state are vault-specific and still clamped to
an available display. Native window managers/compositors retain control of placement
(e.g. Wayland tiling and X11 automatic placement). Linux requests native maximize
after mapping the window; a tiling compositor may retain the geometry without
acknowledging the maximized flag (observed in Hyprland). Okilum records the
compositor's actual state rather than claiming a rejected request succeeded.
Source restoration keeps the document hidden until its source viewport is ready;
the preview and the source's initial top position must not flash on startup.
The published document's source viewport must become usable independently of
background search validation; “Checking search data” cannot keep it blank.
Reader restoration installs the saved block/offset before its first layout, so
the top of the note never paints before the restored position. Breadcrumbs remain
visible on restoration, including mid-note, and stay pinned during later scrolling.
Reader and Source use the same header rule for fresh and restored sessions (Oleg,
2026-10-08).
Initial source syntax preparation starts before mounting, without the typing
debounce. The restored editor is revealed once its scroll and initial highlighting
are ready; subsequent edits stay visible while syntax updates asynchronously.
Neither mode waits for search validation.
The saved appearance is applied before native window construction, rather than
waiting for the window's appearance observer. Startup must not expose a light
palette when Dark was saved.
A previously unseen vault inherits the last active window's
presentation and geometry, without copying its note paths or navigation history.
Transient menus, hover previews, notifications and dialogs do not reopen.

Missing presentation state lives in the versioned OS state store
`reader-ui.json`, outside vaults and rebuildable caches. Writes are atomic and
debounced; normal quit, update relaunch and the existing graceful SIGTERM path
flush pending state. Invalid/future stores are preserved rather than overwritten.
Legacy appearance, panel-width and window-frame files remain migration inputs;
existing pinned/recent/Inbox metadata and source/draft recovery keep their
respective storage contracts. Source contents never enter the UI state file.
On Linux the default store is `~/.local/state/okilum/reader-ui.json` (or
`$XDG_STATE_HOME/okilum/reader-ui.json`), beside `reader-diagnostic.log`.
### About in Settings (#750)

About opens the shared Settings window at About from both the app menu and the
Reader menu. Repeated invocations reuse that window and leave the document
uncovered by a modal. Product information, release links and update action remain
available. Linux says “Updates come from your package manager” without a redundant
technical channel label. Windows describes the full reader/editor client.

### Updates visual QA on Linux

The non-publishing Linux branch-dispatch artifact includes the explicit
`settings-ui-harness` feature. Run its extracted binary with
`OKILUM_DEBUG_UPDATER_UI=sparkle` (or `velopack`) and open Settings → Updates.
Both platform modes use the same production control tree: Stable/Beta segments
and the refresh glyph. Channel selection stays in memory; refresh does not
contact an updater. Normal builds omit this feature and ignore the variable.
This verifies layout and interaction only, not native update delivery.

### Settings presentation (#623)

Settings uses a left-aligned navigation list with section glyphs. Pages have
a title and unboxed rows: a label with a short muted description on the left,
a compact control on the right. Appearance offers a glyph segmented mode
selector and independent text-size and reading-width controls backed by the
global UI state store. Reading controls live in a separate component; the
theme picker offers its swatch cards below these controls.

Files shows the vault name and shortened location, with the full path in a
tooltip and an explicit platform Reveal action. Hidden-file visibility is a
switch. Templates has an inline current value and folder action. No setting
action stretches into a full-width text button.

### Toolbar actions (#680, revised by #767)

New note and New folder create in the selected tree folder, beside a selected
note/file, or in the open note's folder when the tree has no selection. Explicit
context-menu destinations take precedence; template folders remain protected
(#719). These actions retain their menus when hidden by overflow. Rename uses
the note title, note More or its existing shortcut, without a separate pencil
button competing with Edit. Inline errors and Escape cancellation are unchanged.
Folders section actions also overflow into More at narrow widths, so the hidden
files eye and Collapse all never clip at the sidebar edge. Contents preserves
authored heading text verbatim.

### Move destination popover (#684)

Move to… from the note menu or a tree row opens the same lightweight popover at
that menu position. It contains folder search and a scrollable destination list,
with recent destinations first for the current vault and window session. There is
no dimmed backdrop or title bar. Typing filters; Up/Down selects; Enter moves;
Escape or a click outside dismisses. Self, descendants, and the current parent
are excluded. Errors stay inside the popover. The existing revision-aware move,
exceptional link-impact confirmation, and result toast with Undo are unchanged.

### Drawing diagnostics

Missing drawing embeds show “Drawing not found” and a muted filename without the drawing extension. Ambiguous embeds show “Several drawings match this name”; the original target and candidate paths remain in the details disclosure and Copy details. Resolution never chooses an arbitrary match (QA follow-up, 2026-10-08).

A failed drawing preview shows a short message. Parser and rasterizer details
remain available through an Info tooltip and a Copy details glyph, rather than
technical text in the document. Copy uses the standard transient toast. Rendering,
partial-render warnings and missing or ambiguous drawing identity are unchanged.

### Reader keyboard scrolling (#705)

When rendered note text has focus, Up/Down scroll by two text lines and Page
Up/Page Down scroll by 90% of the viewport. This includes focus transferred from
a dismissed compact sidebar. These bindings belong only to Reader TextView;
source inputs and the folder tree retain their own navigation keys.

### Derived Projects (#370, option B approved 2026-10-07)

Projects sits between Inbox and Folders. It contains every readable folder
`@index.md` (case-insensitive) with a scalar `type: project` (case-insensitive), outside PARA archive
folders. It is independent of the folder tree's hidden-item setting. Rows open
the canonical index note; no Markdown, folders or type values are rewritten.

Active projects precede planned projects; unknown/paused/missing statuses remain
visible afterward with the authored status on hover. Done/closed/completed and
cancelled projects are initially behind “Show N done”. Within status groups,
newest accepted source activity in the project subtree comes first; title/path
break ties deterministically. Domain is a muted secondary label, not a raw path.
Open checkbox counts cover the folder subtree, excluding archives and hidden
subfolders/templates, and disappear at zero. Empty Projects has no zero-count
message. Its collapse preference is app-owned and does not alter older sidebar
state fields.

The list is rebuilt from accepted Reader snapshots and refreshed with changed or
removed sources in the existing incremental worker. Render performs no source IO.
The folder tree is unchanged. The future Views slot is absent while no saved view
exists; this slice adds neither service views nor a placeholder heading. Linux
light/dark before/after are captured by the QA sub-session after merge, per the
owner's delivery instruction for this slice.

### Bulk sidebar folding (#712)

The sidebar header's Folders-only glyph hides the four upper sections and opens
Folders; a second click restores their previous state. The restore snapshot is
per-vault and survives restart. Manual section changes invalidate the snapshot.
Alt-click (Option-click on macOS) on a section header applies its next toggle to
all five left sections; Properties in the right panel stays independent.

Primary+Shift+Left always selects Folders-only; Primary+Shift+Right expands all
left sections. Both appear in More. Reveal in sidebar keeps its glyph/menu and
no longer uses Primary+Shift+Left. Collapse all folders is always visible in the
Folders header and collapses tree nodes without collapsing the section itself.
No note files change. The executor landing #663 adds the two shortcut catalog entries afterwards;
#712 has no dependency on the shortcut sheet. Local Linux X11 light/dark
before/after accompanies the PR; the QA sub-session checks the released build
and strict UX on muninn after merge.

### Canonical source for typed views (#636)

Markdown preparation retains the exact primary-file source, including frontmatter,
BOM and line endings, alongside the rendered projection. Reader publishes these
representations together after its path/generation guards pass. Reconciliation
renders its accepted inventory snapshot and compares full source bytes, so a
frontmatter-only change refreshes properties and future native-view selection.
Newer reconciliation supersedes older work for the same note. Closing the note
clears canonical source; HTML mode carries no canonical Markdown snapshot.
This is presentation evidence, not a write capability: native task edits still
require the displayed Tasks index revision and the guarded FileEditor API.


### Network vault availability (#816)

A known network vault with no scan progress for five to six seconds shows “Vault location
is not responding — showing last loaded content”. This is a bounded UI notice,
not proof of disconnection: a filesystem call may still be waiting inside the OS.
The last document and drafts remain available. Retry coalesces with the in-flight
scan rather than creating another blocked worker; once a failed scan has ended,
Retry starts a new one. Successful reconciliation shows a transient “Back online,
rescanned” confirmation and retains the normal network Rescan control. A root read
failure must not replace the last usable inventory with an empty success.
Unreadable counts cover inventory entries only; watcher limitations and preparation
warnings are separate. Native SMB acceptance uses only the disposable #516 fixture.

### Tree file preview (#704)

With tree focus, Space previews the selected non-Markdown file and retains tree
focus, including an explicitly opened compact sidebar. Up/Down follows attachment
selection while that preview session is open. Space again or Escape ends the
session; moving onto a note or folder ends file-preview following. Space on notes
and folders retains ordinary tree activation. Enter/click navigation keeps its
existing behavior, including compact-sidebar dismissal.

macOS uses a Reader-owned native Quick Look panel; Linux/Windows use the existing
main-panel file preview. This does not change supported thumbnail/image formats
or open external applications on selection. Normal note navigation and closing
the owning Reader end the native session. Native Mac QA is required for actual
panel focus and multi-window behavior.


### Note header title priority (#767, Oleg 2026-10-08)

The document title receives space before optional header controls. At narrow widths, Find, Save and Live Preview/Source remain available in the document menu; their toolbar controls and optional labels yield. Back/Forward, Read/Edit, the document menu and the unsaved indicator remain visible. Parent breadcrumbs collapse into a parent-folder menu before the title is truncated; its tooltip always exposes the full title. This applies to Reader and dirty Edit with labels enabled or disabled.


### Edit pencil (#871, Oleg 2026-10-09)

The note-header pencil toggles Edit/Read and is a primary action: optional controls overflow first; the pencil stays visible alongside Read. Its label and accessible name are Edit. Edit restores the last Live Preview/Source choice. Escape dismisses local transient UI first, then returns to Reader through the existing safe save/conflict path. Rename remains title click, F2 and the document menu.

### Entering a rendered Live Preview block (#1642)

A rendered table, image, callout or code block shows its source while the caret
or selection touches it. Besides arrows and a click:

- A click lands on the source of what was clicked: a table cell's text, an
  image's embed. A callout's body still lands on the source line under the
  pointer.
- A double-click opens the source with the whole block selected.
- F2 opens the rendered block next to the caret (nothing but blank space between
  them) with the whole block selected. With no such block F2 renames the note.
- Escape with the selection inside a block's source draws it rendered again
  before it leaves Edit. The caret returns to where F2 or Enter found it, else to
  the line after the block, and the block keeps a focus ring while the caret
  stays there. Enter or F2 then opens it again. Once the caret moves or the text
  changes the ring is gone and Enter is a new line again.

### Image file actions (#1897)

Reader and Live Preview image blocks offer a context menu for the image itself:
Show in Finder / Show in Explorer / Show in folder, Open in default app, Show in
sidebar, Copy path and Copy image. The source editor offers the same actions when
its caret is inside a Markdown image or image wikilink embed. Cmd-click on Mac
and Ctrl-click elsewhere opens the image in the system default app. Ordinary
Live Preview presses still return to source editing.

The menu uses the rendered image's captured resolver identity. Missing or ambiguous
images have disabled actions and an explanation. A cloud placeholder can be revealed
and explicitly downloaded; Open and Copy image remain disabled until download.
Remote HTTP(S) images offer Open in browser and Copy image only. Copy runs in the
background through bounded local reads or the existing image downloader, and never
creates a vault attachment. Commands captured for a note cannot act after changing
that note or its publication generation. Source-menu construction reads an observed
snapshot rather than re-entering EditorState. Brain previews retain their own image
authority. Native file-manager/default-app acceptance is recorded per OS and build.

Local image actions require a path inside the captured vault, including authored
`file://` images. Outside paths and decoded traversal have disabled actions;
execution rechecks canonical containment, including placeholder Reveal/Download,
so a replaced symlink cannot redirect the command. Source image lookup is limited
to 64 KiB before resolver construction or parsing, for menus and modified clicks.


### Bounded watcher batches and remote FUSE (#1945, 31 October 2026)

Approved policy: retain the 300 ms quiet window after each relevant event,
including repeated writes to an already-pending note. Ignored service paths,
attachments and out-of-root events do not postpone pending note delivery.
Release a nonempty batch after at most two seconds from its first relevant
event, even if the event queue remains busy. Existing bulk thresholds and caller
coalescing remain authoritative; this adds no scan or periodic timer.

On Linux, known remote FUSE mounts (`fuse.sshfs`, `fuse.rclone`) retain their
native watcher for local writes. Mount classification uses decoded mountinfo
paths and the longest component prefix, so nested local mounts retain local
behavior. The existing inline header control says “Auto-refresh limited” and
offers Rescan for other clients' changes; it adds no modal or banner and exposes
no filesystem paths. Local/unknown FUSE keeps its existing watcher behavior.
NFS/SMB handling is unchanged. Other remote filesystem types and periodic scans
are outside this change.

Additive `@` vault conventions and the synthetic native acceptance matrix are
defined in [special-path compatibility](../vault-at-conventions.md). Both legacy
`_` behavior and the configured templates branch exception remain supported.
