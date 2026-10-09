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

- **App titlebar (46 px).** Sidebar toggle and one vault Search at the left;
  loading feedback, app More and right-panel toggle at the right. Native window
  controls and dragging remain unchanged. Appearance and Settings are in app
  More rather than separate toolbar buttons.
- **Document header (48 px, pinned).** Back/Forward precede folder breadcrumbs
  and the note title. Clicking the title renames inline. Read/Edit use book/pencil;
  Edit also has Live Preview/Source (eye/code). Find and note More follow. Save
  appears only while dirty; save/conflict feedback keeps its existing behavior.
- **Labels.** Glyphs are the default. Settings → Appearance → “Show labels on
  toolbar buttons” persists globally; mode labels appear only when the measured
  title and controls fit. Find yields to More before the title truncates, labels
  yield to glyphs, and Back/Forward and Read/Edit remain available.
- **Note More.** Reveal in sidebar, Find, mode, Rename, Move, history, platform
  Reveal, Copy path, Open in new window, Move to Trash and Close. New window uses
  the existing vault lifecycle; split view is outside this change.
- **App More.** Settings, keyboard shortcuts, creation/open/recovery actions,
  sidebar preferences, Appearance and application commands.
- **Panels** are separated from the document by one 1 px `border-subtle` line. Panel
  headers are titles (vault name, «On this page»), never «Close panel». Closing
  uses the title-bar toggle or panel shortcut; overlay panels also close on
  an outside click. Esc only dismisses local transient UI (#483). Neither panel header has a close button.
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
Quick open (Ctrl+K / ⌘K) is unchanged. The same Find action works in preview
and Edit source. Both match literal text without case by default, including
Unicode letters. The optional Aa / Match case switch starts off; its explicit
choice is remembered globally in the application UI store outside the vault (#682).

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

The document column has a maximum width of 740 px including 40 px of horizontal padding, so lines are at most 660 px, about 75
characters. The column is centred in the available document area. The
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
| Sidebar / right panel default width | 264 / 256 (resizable, persisted, min 200) |
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
| `find-match` | `#fde68a` | `#5c4f1c` | find marks |
| `find-current` | `#fbbf24` | `#8a7420` | current find mark |
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
(`tessera-theme/v1`). A unit test checks every theme in both variants: `text`
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
theme. Set from the vault name menu (Vault colour) or Settings → Files; stored
in `reader-ui.json` (`vault_colors`) by canonical vault root, never in the
vault. Every window of the vault repaints at once. No per-vault theme and no
tinted rows.

## Icons

Lucide outline icons at 16 px, stroke 1.75, `text-muted` at rest and `text` on
hover. Toolkit names come from `gpui-component`'s `IconName`; the four missing
glyphs are added as Okilum assets under `icons/` and loaded with `Icon::path`.

| Action | Icon | Shortcut / tooltip |
| --- | --- | --- |
| Toggle sidebar | `PanelLeft` | «Notes ⌘\\» |
| Back / Forward | `ArrowLeft` / `ArrowRight` | «Back ⌥←», «Forward ⌥→», disabled at the ends |
| Find in note | `TextSearch` | «Find in note ⌘F» |
| Toggle right panel | `PanelRight` | «On this page ⌥⌘\\» |
| More | `Ellipsis` | menu |
| Close find | `Close` | «Close Esc» |
| Find previous / next | `ChevronUp` / `ChevronDown` | «⇧⏎» / «⏎» |
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
is highlighted with `selected` and weight 500, and its ancestors are expanded.
There is no flat note-list mode in the sidebar and no item cap (#369). Folders whose name starts with `_` or `.` are
not shown; their notes stay reachable through links and search. Archive folders
(`Archive`, `4 Archive`, `Архив`) are muted and open only on request or to reveal
the current note. Navigating to a hidden note shows its branch muted until the next note;
hidden rows use subdued styling without a text badge, and the current row keeps
its normal selection contrast. The tree never highlights a neighbour instead. «Show hidden files» (eye button on the Folders header, … menu, ⇧⌘. — bound as `cmd->`, the way macOS reports it) lists
them all and is remembered per vault (#395). Recent, Pinned and Inbox follow the
same rule and update as soon as it changes; quick open and search still find
hidden notes (#635). Hovering the Folders header
shows «Collapse all» and «Focus current» (only the path to the open note stays
expanded; ⇧⌘← while browsing). ⌥-click on a folder, ⌥→/⌥← on the selected folder and the
folder context menu («Expand/Collapse all subfolders») act on the folder and
everything below it, as in Finder. There is no global «Expand all» (#410).
F2/Enter starts inline rename for notes and folders (#578); Right/Left expands/
collapses, and Left on a child selects its parent. A folder-label click selects;
double-click, its disclosure glyph, or ⌘↓ (Ctrl↓ on Linux) toggles it.


### Sections (#369, variant A «Sections», chosen by Oleg 2026-10-04)

Mockup: [sidebar.html](https://html.me.uk/t/okilum-reader-348/sidebar.html?v=A).
Under the panel header, top to bottom, each section collapsible (state remembered):

- **Recent** — the last opened notes, 5 shown, «N more» up to 10, with a
  relative age.
- **Pinned** — notes and folders pinned with the pin that appears on hover
  (filled `accent` when pinned). A pinned folder reveals itself in the tree.
- **Inbox** — computed, never configured or stored: notes created in the last
  14 days (platform birth time; fallback: first seen by the app after its
  baseline) that are not yet built into the structure — directly in the vault
  or a domain root, or without incoming links. Archived and `_` notes never
  qualify. A note leaves on its own once it is moved into a PARA folder and
  linked. Each row shows the reason («at root», «in Work», «no links») and age;
  the header shows the count.
- **Folders** — the tree above.

All four section headers remain visible outside the scrolling bodies (#434).
Scrolling down in Folders folds Recent/Pinned/Inbox to counted header rows;
returning to the top restores the user's expanded sections. Clicking a folded
header expands it in place without resetting the tree position. Automatic folding
is transient and never overwrites the saved collapse preference. Large upper
sections have bounded body viewports, sharing the space above a reserved 140px
minimum tree viewport; their headers do not scroll with their contents.

Recent, Pinned and collapsed sections are app state per root in the Reader
state directory; nothing is written into notes. Searching in the palette leaves
the sidebar sections and tree expansion unchanged.

## Right panel (R3)

Header «On this page», without a close button. Until R3 the right panel keeps its current
backlinks content under the title «Backlinks» with a quiet «N notes · M links» count. Two sections with independent scrolling:

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

## Onboarding / empty

No root selected: centred card, 440 px wide. It holds the Okilum mark (52 px), the title «Open your
notes», one sentence saying files are read-only, a primary **Open folder…** and a
secondary **Open file…** button, and recent folders below a divider.

## Code block Copy (#429)

Fenced and indented code blocks have a small **Copy** action in the top-right
corner, visible on block hover and on keyboard focus. Tab reaches the action;
Enter/Space activate it. After activation the label reads **Copied** for two
seconds, including after the pointer leaves the block. Repeated copies renew
that interval; changing the block clears the feedback.

Copy writes code content only: no fences or info string, no Markdown container
indentation, while code indentation, blank lines and original line endings
(including a final LF/CRLF/CR when present) are preserved. Inline code has no
Copy action. This does not change the document's selection-copy format.


## Code block language (#430)

Code blocks reserve a 24 px header above the code; a quiet language label sits
next to Copy. Explicit fence languages win, with aliases normalised (sh/shell →
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
tooltips. Attachment headers use the same language: Quick Look (macOS), Open,
Reveal and Copy path glyphs alongside More.

Reveal uses the platform's name for the file manager: «Reveal in Finder» on
macOS, «Show in Explorer» on Windows and «Show in File Manager» on Linux, where
it calls `org.freedesktop.FileManager1.ShowItems` and falls back to `xdg-open`
on the containing folder. Shortcut hints use ⌘/⌥/⇧ glyphs only on macOS and
spell out Ctrl/Alt/Shift elsewhere; both come from `platform::labels` (#612).

Document More contains Reveal in sidebar (the sidebar crosshair action),
Edit/Preview, Rename/move, Note history, Reveal,
Copy path, Open in new window, Move to Trash and Close note.
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

Files also selects a templates folder per vault (default `_Assets/Templates`).
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
the current note or file using the same action as the sidebar crosshair.

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
`_index.md` with a scalar `type: project` (case-insensitive), outside PARA archive
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
