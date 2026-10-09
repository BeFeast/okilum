# Research: Live Preview / smart editor, and what to take from Zed (#359)

> **Historical document.** Written before Tessera was renamed Okilum (2026-10-09). Names, paths and links are kept as they were then.

Status: research only. No product code changes. This document informs a decision
and a slice plan for the Obsidian-style Live Preview editor requested in #359.

Request: "research how hard it is to implement a smart editor; steal (hommage) the
tastiest bits from the Zed editor". The target is Obsidian-style Live Preview:
Markdown syntax hidden outside the cursor line or selection; wikilinks, embeds,
images, tables, tasks and headings rendered inline; and the editor qualities Zed
has. Owner update during the research: relicensing Tessera from MIT to GPL-3.0 is
acceptable if it materially helps, so both a "stay MIT" and a "switch to GPL" path
are evaluated.

Evidence base:

- Zed: `zed-industries/zed` at `72d073d6423b0bf7e04aa87567a308d19617b7f2`
  (2026-10-06). The licence table comes from each crate's `Cargo.toml`
  `license` field and its `LICENSE-*` symlink.
- Tessera `main` at `9b9b669`, with the vendored gpui-kit
  `928c3eb776a3d733d9b771f7dea27a6a79242ced` set up and verified by
  `scripts/vendor-setup.sh --verify`.
- A scratch benchmark outside the repo (source in the appendix). All timings come
  from one host in one session: a 4-vCPU Intel Xeon @ 2.80 GHz Linux container,
  release profile.

## TL;DR

- **Tessera already has the first slice. It is just not on ordinary notes.**
  - Managed Brain notes have a single-buffer Source/Live Preview editor that hides
    heading, emphasis, strike, inline-code, link and wikilink syntax outside the
    active block. It is described in `docs/managed-live-preview.md` (#211–#220).
  - It runs on vendor patches 0010/0011/0012/0014/0015/0017/0023 (a
    `ProjectionProvider` seam in gpui-kit's `EditorState`), plus
    `tessera_core::source_projection` and `tessera_core::source_classifier`.
  - The Reader's source mode (Cmd+E / Ctrl+E) uses the same `EditorState`, but
    installs `ExactSource`, a provider that always returns `None`
    (`crates/tessera-shell/src/reader_editor.rs:19-24`).
  - The cheapest useful step is to wire the existing classifier into the Reader
    editor, not to build anything new.
- **Licences: only the GPUI family, `sum_tree`, `collections` and `util` in Zed are
  Apache-2.0.** `editor`, `multi_buffer`, `text`, `rope`, `language`, `theme`, `ui`,
  `markdown` and `vim` are GPL-3.0-or-later.
  - "Adopt Apache-licensed `text`/`rope`" is not an option: both are GPL.
  - Of the 249 workspace crates, 34 are Apache-2.0, 207 are GPL-3.0-or-later and 8
    have no `license` field (benchmarks and `publish = false` crates, which ship a
    GPL licence file).
- **Using Zed's `editor` under GPL does not mean using an editor widget. It means
  importing most of Zed.**
  - `editor` reaches 99 workspace crates transitively, including `workspace`,
    `project`, `client`, `rpc`, `db`, `lsp`, `dap`, `terminal`, `git` and
    `remote`.
  - It builds against Zed's in-tree `gpui`, not the `gpui-pre 0.3.x` that Tessera
    and gpui-component pin.
  - Zed's display map also assumes uniform line height, and blocks are whole
    numbers of rows. Obsidian-style larger headings would need deep changes there
    too.
- **tree-sitter-markdown is the wrong engine for live classification. It measured
  roughly 40× slower than comrak** and gets little from incremental reparsing.
  - At 256 KiB: comrak parses in 6.8 ms. tree-sitter-md takes 290 ms for a full
    block+inline parse, and 260 ms to reparse after a one-character edit.
  - Positive control: the same harness shows tree-sitter-rust re-parsing a 256 KiB
    file in 2.4 ms after an edit (65 ms full). The instrument does detect reuse;
    the Markdown grammar simply does not benefit from it.
- **What to take from Zed is mostly ideas, and those ideas are legal under MIT.**
  - Layered display transforms over a `SumTree`, each with input and output
    summaries.
  - Edits treated as invalidation ranges that flow up the layers.
  - Anchors.
  - `BlockPlacement::Replace` for rendered blocks.
  - Transaction grouping for undo.
  - Background parsing that tolerates stale results.
  - The one crate worth reusing directly, `sum_tree`, is Apache-2.0 and is already
    in Tessera's dependency graph as `gpui-pre-sum-tree`.
- **Recommendation.**
  - **Stay MIT** and evolve the existing editor (option a), in the slices below.
    The first slice for ordinary notes is about 3–5 engineer-days plus native
    acceptance.
  - **Switching to GPL buys little for this feature.** It would let us vendor
    `text`/`rope` (anchors, CRDT undo) and port display-map code. That pays off
    only if Tessera commits to a from-scratch editor element (about 4–6
    engineer-months). Relicensing is not justified by the editor alone.

## 1. Licences, crate by crate

Zed's README: "Zed source code is licensed primarily under GPL-3.0-or-later, with
Apache-2.0 components where marked." Each crate carries its own `license` field and
a symlink to the matching root licence file (`LICENSE-GPL` or `LICENSE-APACHE`).
There is no AGPL file at this commit.

| Zed crate | Licence (`Cargo.toml`) | Licence file | Relevance |
|---|---|---|---|
| `gpui` | Apache-2.0 | `LICENSE-APACHE` | UI framework (Tessera uses the crates.io republish `gpui-pre`, also Apache-2.0) |
| `gpui_platform`, `gpui_macros`, `gpui_*` (apple/linux/macos/windows/wgpu/web/util/shared_string/tokio) | Apache-2.0 | `LICENSE-APACHE` | Platform layer, including IME and AccessKit plumbing |
| `sum_tree` | Apache-2.0 | `LICENSE-APACHE` | B+-tree with monoid summaries: the backbone of rope, buffer and display map |
| `collections`, `util`, `refineable`, `scheduler`, `zlog`, `ztracing` | Apache-2.0 | `LICENSE-APACHE` | Small utilities |
| `rope` | **GPL-3.0-or-later** | `LICENSE-GPL` | Chunked rope with UTF-8/UTF-16/point summaries |
| `text` | **GPL-3.0-or-later** | `LICENSE-GPL` | CRDT buffer, anchors, transactions, undo map, `Patch`/`Edit` |
| `clock` | **GPL-3.0-or-later** | `LICENSE-GPL` | Lamport/vector clocks used by `text` |
| `multi_buffer` | **GPL-3.0-or-later** | `LICENSE-GPL` | Excerpt aggregation over buffers |
| `editor` (includes `display_map/*`, `element.rs`, `movement.rs`, `selections_collection.rs`, `input.rs`) | **GPL-3.0-or-later** | `LICENSE-GPL` | The editor, display map, IME handler and multi-cursor |
| `language`, `languages`, `grammars`, `language_core` | **GPL-3.0-or-later** (`grammars`/`language_core`: no field, `publish = false`, GPL file) | `LICENSE-GPL` | Syntax map, tree-sitter injections, highlight queries |
| `buffer_diff`, `lsp`, `project`, `settings`, `theme`, `ui`, `workspace` | **GPL-3.0-or-later** | `LICENSE-GPL` | Pulled in by `editor` |
| `markdown`, `markdown_preview` | **GPL-3.0-or-later** | `LICENSE-GPL` | Zed's pulldown-cmark renderer for hovers and the preview pane |
| `vim` | **GPL-3.0-or-later** | `LICENSE-GPL` | Vim/Helix modes (34.6k lines) |

Third-party crates that would be relevant (from the crates.io registry, versions
locked in Tessera's `Cargo.lock`):

| Crate | Licence | Already in Tessera? |
|---|---|---|
| `gpui-pre` 0.3.3 and all 23 `gpui-pre-*` crates, including `gpui-pre-sum-tree` | Apache-2.0 | yes |
| gpui-kit (`gpui-component`, `gpui-base`) | Apache-2.0 | yes (vendored) |
| `ropey` 2.0.0-beta.1 | MIT OR Apache-2.0 | yes (through gpui-base) |
| `tree-sitter` 0.26.13, `tree-sitter-md` 0.5.3 | MIT | yes (through gpui-component's highlighter) |
| `comrak` 0.47.0 | BSD-2-Clause | yes (`tessera-core`) |
| `markdown` (markdown-rs) 1.0.0 | MIT | yes (gpui-base TextView) |

Consequences for MIT Tessera:

- Apache-2.0 crates can be linked or vendored with notices. `THIRD_PARTY_NOTICES.md`
  already covers `gpui-pre-*`.
- GPL crates cannot be linked or copied, including translated or "lightly adapted"
  code. Only ideas, algorithms and public documentation of behaviour may be used,
  written fresh. When an MIT implementation is ported from a GPL file, write it
  from the design described here, not side by side with the GPL source.

## 2. Tessera today

There are two editors on the same widget, and one renderer that cannot map back to
source.

### 2.1 Reader source mode (ordinary notes)

- **Widget.** `crates/tessera-shell/src/reader_editor.rs:251-264` builds
  `gpui_component::input::EditorState` with `.language("markdown")`, soft wrap,
  no line numbers, folding, search or replace, and `ExactSource` as the projection
  provider. That provider returns `None` (`:19-24`), so nothing is concealed. It
  is painted at `:762` in Cascadia Code 13 px.
- **Toggle.** `Reader.editing: Option<reader_editor::Editing>` (`main.rs:1091`).
  The actions are `ToggleSource`/`SaveSource` (`main.rs:147-148`, bound to
  `secondary-e`/`secondary-s`). `render_main` chooses `render_source()` when
  editing (`main.rs:3930`).
- **Lossless, revision-aware save.**
  - `tessera_core::file_editor::FileEditor` (`file_editor.rs:69`) does a
    byte-compare-and-swap against the opened base, an atomic `renameat`
    `EXCHANGE`, and preimages in `source_history`.
  - Every `SourceMutation` queues a superseding `DraftWrite`.
  - `set_value` (which resets undo history) is called only on load, reload and
    refresh.
- **Platform.** Unix only. On Windows, `reader_editor_windows.rs` makes `Editing`
  uninhabited.

### 2.2 Managed Brain editor (Live Preview exists here)

- **Facade.** `brain/source_input.rs:21` defines
  `SourceInput::{Legacy(TextareaState), Managed(EditorState)}`.
- **Toggle.** `brain/source_projection_ui.rs` (`SourceProjectionUi { live }`,
  `toggle_source_projection` `:141`, `schedule_source_projection` `:82`). The
  button is in `brain.rs:3331-3343`.
- **Adapter.** `brain/source_projection.rs`. `CachedProvider` (`:15`) classifies
  on a background executor with one latest pending request. It implements
  `ProjectionProvider::compose` (`:71`) by returning a `MappedProjection`
  (`:122-153`).
- **Core.** `tessera_core::source_projection` (`Snapshot`, `Region::{Source,
  Conceal}`, `Plan`, `Active`, `Bias`, `project`) maps between source and display
  with explicit bias at hidden boundaries. `tessera_core::source_classifier::classify`
  (comrak with real byte positions) produces the `Plan`, `StyleSpan` and
  `NoteLink` values.
- **Vendor seam.** Patch 0010 (2,001 lines) adds
  `gpui_base::input::projection`: the `SourceProjection` and `ProjectionProvider`
  traits, `SourceStamp`/`LayoutStamp`, `ConcealBias`, `WrapAffinity` and
  `SourceMutation`. Patches 0011/0012/0014/0015/0017/0023 add review fixes, IME
  geometry, vertical movement, attribution, projected grapheme boundaries and
  source click.

Hard limits that matter for "very large notes":

- Classifier: 64 KiB, 4,096 AST nodes, depth 32.
- Projection: 256 KiB, 4,096 ranges.
- Above these limits the note falls back to exact Source. That is safe, but no
  Live Preview is shown.
- The benchmark below shows that a dense synthetic note of about 60 KiB already
  hits the node cap (`StructureLimit`) and falls back entirely.

Current limits of the projection seam, found by reading
`vendor/.../crates/base/src/input/projection.rs`:

- **Conceal only.** A projection can remove source bytes from the display. It
  cannot insert a rendered element (image, table, embed, checkbox).
- **Uniform metrics.** `ProjectionStyle` covers only font family, bold, italic and
  strikethrough. "All runs share the editor's font size and line height." There
  is no colour, so links are not coloured
  (`brain/source_projection.rs:projected_styles`), and there are no larger
  headings.
- **Whole-document work per compose.** `SourceSnapshot.text` is an `Arc<str>` of
  the whole document. `project()` is linear in document size per caret move
  (0.2 ms at 4 KiB and 0.76 ms at 16 KiB on this host).
- An archived native measurement on a 987-byte fixture gave medians of
  10.3 ms (Source) and 18.4 ms (Live Preview) for "first coherent trailing
  marker" (`docs/archive/native-attribution221-plan.md:3`). The extra latency is
  measurable even on tiny notes.

### 2.3 Reader render model (read-only)

- **Parse path.** `render::reader_document_from_source` (`render.rs:495`) rewrites
  source into a string: embeds become tilde fences, `[[x]]` becomes
  `tessera://open/…`, and `==x==` becomes `<mark>`. gpui-kit's `TextView` then
  parses that string with markdown-rs (`main.rs:1720`,
  `set_text_with_source`).
- **Plugins.** Custom nodes such as `local-image`, `callout`, `embed` and
  `highlight` are registered through `markdown_plugins` (`main.rs:900`).
- **No source mapping.** Offsets in this model are offsets into the rewritten
  string, and `docs/reader-source-editing.md:62` forbids reusing them as source
  offsets. `tessera_core::ir` (`Block`/`Inline`) is a legacy model used only by
  `tessera-cored`, with no byte ranges.
- **What can be reused.** The Reader's renderers (image, callout, table, embed
  plugins) can be reused as **element factories** for rendered blocks in Live
  Preview. Their parse output cannot be the editing model.

## 3. Zed "goodies": how each works, and how much it is worth to a notes editor

Effort is in engineer-days or engineer-weeks for one developer who knows the
code, including tests. It excludes native acceptance runs on the user's machines.
"MIT path" means a fresh implementation inside gpui-kit or Tessera. "GPL path"
means using or vendoring Zed's code after relicensing.

| Goodie | How Zed does it | Value for notes | MIT path | GPL path | Effort (MIT) |
|---|---|---|---|---|---|
| **Rope over `SumTree`** | `rope::Rope` = `SumTree<Chunk>`, where each chunk is an `ArrayString` of up to 128 bytes with bitmaps for chars, newlines, tabs and UTF-16 (`rope/src/chunk.rs:13-60`). The summary gives O(log n) offset↔point↔UTF-16 conversion. | Medium. Notes are small, and ropey already gives O(log n). | Keep **ropey** (already the gpui-base buffer). Use `gpui-pre-sum-tree` only for new layered indexes. | Vendor `rope` (7-crate closure, only `rope` itself GPL) | 0 |
| **Anchors** | `text::Anchor` = (insertion timestamp, offset, bias). It survives any edit without being rebased by hand (`text/src/anchor.rs:11`). | High: reveal state, rendered-block placement, comments and AI proposals anchored in text, task toggles. | Implement offset anchors rebased by an edit log (`Patch`-like `Vec<Edit>` per generation). That covers a single-user buffer; a CRDT isn't needed. | Vendor `text`+`clock`+`rope` (12-crate closure, 3 GPL) | 1–2 wk |
| **Transactions and undo grouping** | `History` with `start_transaction`/`end_transaction`, nested depth, 300 ms `group_interval` (`text/src/text.rs:150-260`). The undo map supports selective undo. | High: undo across reveal, block toggles and a Markdown command (e.g. "bold") should each be one step. | gpui-base already has an `UndoManager` with `push_transaction(change, intent)` and `EditIntent::{Typing, Backspace, DeleteForward, Atomic}` (`base/undo_manager.rs:7`). Add nested explicit transactions for multi-edit commands. | Comes with `text` | 3–5 d |
| **Multi-cursor** | `SelectionsCollection` of `Selection<Anchor>`. Commands map over all selections; `add_selection_above/below`, `select_next`, `split_selection_into_lines` (`editor/src/selection.rs:227-369`). | Low-medium for prose; nice for lists and tables. | gpui-base is single-selection. Multi-selection touches every edit command, IME and projection reveal (every selection reveals its block). | Comes with `editor` | 3–4 wk |
| **Display-map layers** | `InlayMap → FoldMap → TabMap → WrapMap → BlockMap`. Each is a `SumTree<Transform>` with `{input, output}` summaries, a `sync(edits) -> edits` step, and coordinate converters (`editor/src/display_map.rs:1-60`). | **Core to Live Preview.** Conceal = fold with an empty placeholder; rendered widget = `Replace` block; inline widget = inlay with a renderer. | gpui-base has `WrapMap`+`FoldMap` plus Tessera's projection in front. Rebuild projection as a sum_tree transform layer with incremental `sync`. | Port `display_map/*` (about 21k lines including tests, coupled to `MultiBufferSnapshot`) | 2–3 wk for a conceal layer; 3–4 wk more for blocks |
| **Folds with custom placeholders** | `FoldPlaceholder { render: Fn(FoldId, Range<Anchor>) -> AnyElement, constrain_width, merge_adjacent }` (`fold_map.rs:27`). `ChunkRenderer` lets an element stand in for a text chunk. | High: `[[link\|label]]` → "label" chip; `- [ ]` → checkbox; inline image icon. | Extend the projection with **replacement runs** (projected text plus an element factory) instead of pure deletion. | Port | 1–2 wk |
| **Block decorations** | `BlockPlacement::{Above, Below, Near, Replace(RangeInclusive)}`, `BlockProperties { height: Option<u32> /* rows */, style, render, priority }` (`block_map.rs:163-300`). | High: images, tables, embeds, callouts, mermaid/excalidraw. | New block layer in gpui-base below wrap, with **pixel** heights (Zed uses whole rows). | Port, then still add pixel heights | 3–4 wk |
| **Soft wrap** | `WrapMap` runs line wrapping on a background task and syncs edits incrementally (`wrap_map.rs`). | Required (already present). | Already in gpui-base. | n/a | 0 |
| **Syntax via tree-sitter** | `language::SyntaxMap` with injection layers: block `markdown` grammar, `inline` injected as `markdown-inline`, code fences injected by info string (`grammars/src/markdown/injections.scm`). Parsing is async; highlights are anchor-based. | Medium. Good for highlighting fenced code inside notes. **Not** for classifying Markdown (see §5). | Keep comrak for classification. Keep gpui-component's tree-sitter highlighter for fences. Use injections only inside fences. | Vendor `language` (very coupled) | 0 (already) |
| **Incremental reparse** | Edits go to `tree.edit()`, and an async reparse with a timeout is swapped in when ready; stale trees stay displayed. | High for large notes. | Block-local reparse with comrak: re-classify only the dirty top-level blocks. Fall back to a full parse for fences, HTML, lists and link-reference changes. | n/a (grammar limitation) | 1–2 wk |
| **Hover/code-action popovers** | `hover_popover.rs`, `code_context_menus.rs`, positioned from display points. | Medium: link preview on hover, wikilink autocomplete, "Create missing note". | gpui-base already ships `HoverProvider`, `CompletionProvider` and `CodeActionProvider` popovers. Wire them to Tessera link data. | n/a | 1 wk |
| **IME** | `impl EntityInputHandler for Editor` (`editor/src/input.rs:2798`): UTF-16 ranges, marked text, `bounds_for_range` from display points. | Required (CJK composition; macOS accent and dead-key marked text; Russian layouts mostly commit directly). | gpui-base implements the same gpui trait (`base/state.rs:3112`). Patches 0012/0017 already handle projected IME geometry and grapheme boundaries. Both editors share gpui's Apache platform layer. | n/a | 0, plus native QA |
| **Vim mode** | `vim` crate (34.6k lines, GPL) layered on `Editor` actions. | Low for the product brief. | Out of scope. | Only with the whole `editor` | — |
| **Large-file performance** | Background parse and wrap, anchor-based highlights, row-virtualised painting, `SumTree` everywhere so each keystroke costs O(log n) instead of O(n). | High: the current caps are 64 KiB and 4,096 nodes. | sum_tree projection plus block-local classification. Paint cost in gpui-base for large notes was not measured here (no GUI run). | Comes with `editor` | covered above |
| **Accessibility** | gpui has AccessKit plumbing (`gpui/src/window/a11y.rs`, Apache), but **Zed's editor element exposes no text-editing a11y nodes** at this commit (no hits in `crates/editor/src`). | Required eventually. | Same gap in gpui-base. Must be built either way, through gpui's a11y API. | Gives nothing | 2–3 wk (separate issue) |

## 4. How Live Preview maps onto Zed's display-map model

Zed's model has the right *shape* for Live Preview. Everything is a projection of
one canonical buffer through layers that each turn edits from the layer below into
edits in their own coordinates. Live Preview features map onto it like this:

| Live Preview behaviour | Zed primitive | Tessera equivalent (MIT) |
|---|---|---|
| Hide `**`, `#␠`, `[[`, `\|label]]` outside the active block | Fold with an empty `FoldPlaceholder` (zero-width), `merge_adjacent` | `Region::Conceal { markers }` in `source_projection`. It already exists, but whole-document. |
| Show `[[Note\|label]]` as a link chip | Fold whose placeholder renders an element; `ChunkRenderer` with `constrain_width` | New *replacement run* (projected text plus element factory) in the projection seam |
| `- [ ]` as a clickable checkbox | Inlay or fold renderer | Replacement run. A click emits **one source edit** (`[ ]`↔`[x]`) in one undo transaction. |
| Image, table, embed, callout rendered | `BlockPlacement::Replace(start..=end)` with `render` | New block layer: source range → element with a **measured pixel height** |
| Reveal on caret or selection entry | Not built in. Zed blocks and folds are explicit, and nothing reacts to the caret. | Already designed: `Active { selection, composition }`, and every touched block reveals |
| Headings larger than body text | **Not possible.** Rows have uniform `line_height`, block heights are `u32` rows. | Needs variable row metrics in gpui-base `WrapMap`/element. This is the biggest native change. |

Rules for keeping caret movement and selection sane (all but the last are already
partly in the Tessera contract, `docs/source-projection.md`):

1. **Hidden ranges collapse to one display anchor with explicit bias.** Zed uses
   `Bias::Left/Right` on every coordinate conversion; Tessera has `ConcealBias`.
   Arrows step over a hidden range in one keystroke. Then the reveal re-layout
   happens, and the caret keeps its *source* position, never its pixel.
2. **Reveal before editing.** An edit inside a concealed block first reveals the
   block in the same frame, so a delete never removes hidden bytes the user cannot
   see. This is done (`reveal_selection`).
3. **Rendered blocks are atomic for the caret.** Moving vertically into a
   `Replace` block either places the caret before or after it, or reveals it into
   source. Pick one per block kind: Obsidian reveals tables and embeds on click and
   skips over them with the arrow keys. Selection across a block includes its full
   source range, so copy is always exact authored bytes.
4. **Presentation is never an edit.** Reveal, block measurement, theme change and
   mode switch create no undo transaction and never call `set_value`. Zed follows
   the same rule: display-map changes do not touch `text::History`.
5. **Stale presentation is shown, not trusted.** Zed paints the last good syntax
   tree while a reparse runs. Tessera's `SourceStamp` pinning already refuses to
   resolve hits against a stale layout. Keep that, and add "show the stale styling
   for unchanged blocks" instead of falling back to Source for the whole document.

## 5. Measurements

The scratch crate is in the appendix. Corpus: a synthetic note made of repeated
sections. Each section has a heading; paragraphs with bold, italic, strike,
inline code, a link and wikilinks; a list with tasks; a table; and a fenced code
block. Timings are medians of 25 runs (5 runs at 1 MiB and above), on one host, in
one session.

| Size | comrak parse | `classify` (Tessera) | Style spans | `project` (caret mid) | tree-sitter-md full | tree-sitter-md incremental (1 char) | ts block-only full | ts block no-op reparse | ts block incremental |
|---|---|---|---|---|---|---|---|---|---|
| 4 KiB | 0.10 ms | 0.45 ms | 91 | 0.20 ms | 4.0 ms | 3.4 ms | 1.3 ms | 0.69 ms | 0.71 ms |
| 16 KiB | 0.39 ms | 1.7 ms | 341 | 0.76 ms | 15.6 ms | 13.3 ms | 4.9 ms | 2.8 ms | 2.7 ms |
| 60 KiB | 1.5 ms | 3.3 ms → **fallback** (`StructureLimit`) | 0 | 2.9 ms | 65 ms | 55 ms | 19.7 ms | 11.7 ms | 12.7 ms |
| 256 KiB | 6.8 ms | fallback (`InputLimit`) | 0 | — | 290 ms | 260 ms | 102 ms | 75 ms | 66 ms |
| 1 MiB | 52 ms | fallback | 0 | — | 1.20 s | 1.00 s | 420 ms | 262 ms | 259 ms |
| 4 MiB | 139 ms | fallback | 0 | — | 4.61 s | 4.55 s | 1.64 s | 1.04 s | 1.06 s |

**Positive control** (same harness, tree-sitter-rust 0.24.2, 256 KiB of Rust):
65 ms full parse, 2.5 ms no-op reparse, 2.4 ms after a one-character edit. The
harness does detect node reuse, so the absence of reuse for Markdown is a property
of the grammar, not of the probe. The one-character edit was applied: the new
tree's root ends at the new length.

Reading the numbers:

- **comrak is about 40× faster than tree-sitter-md** on the same text, and is
  already a dependency. tree-sitter-md's own README warns that it is not meant for
  uses where correctness matters.
- tree-sitter-md's incremental reparse gains little. Its external block scanner
  carries state that blocks subtree reuse: a no-op reparse costs 60–75% of a full
  parse. On top of that, the `MarkdownParser` wrapper re-runs a separate inline
  parse for **every** `inline` node on every reparse. That is 5,431 parser calls at
  256 KiB, matching old inline trees by index only.
- The prebuilt `tree-sitter-md` has no wikilink node. `EXTENSION_WIKI_LINK` exists
  in `grammar.js` but is not compiled into the shipped `parser.c`. Using it would
  mean vendoring a regenerated parser.
- Tessera's classifier costs about 4× a bare comrak parse (validation, grapheme
  checks, mapping). It is fine per keystroke up to about 16 KiB on this host.
- The **4,096-node cap, not the 64 KiB byte cap, is the first wall**: a dense
  60 KiB note falls back entirely to Source.
- `project` is linear and runs on every caret move. At 16 KiB, 0.76 ms is
  acceptable. Without a sum_tree layer it would not be at 256 KiB.

## 6. Options

### (a) Evolve the current editor: gpui-kit `EditorState` plus Tessera projection (recommended)

- Native caret, IME, undo, exact clipboard, recovery and CAS are already accepted
  for managed notes.
- Work proceeds as small vendor patches to `gpui-base` and app code in
  `tessera-core`/`tessera-shell`.
- Zed ideas come in as fresh implementations: a sum_tree transform layer,
  replacement runs, pixel-height blocks, block-local reparse and anchors.
- Risk: the vendor patch stack grows. It is 11,868 diff lines today, about 4,400
  of them for the input seam. AGENTS.md asks that it be kept small and upstreamed.
  The projection seam is generic (not Markdown-specific), which makes it a
  credible upstream contribution to `longbridge/gpui-kit`.

### (b) Apache Zed crates plus our own display layer

- As originally framed ("adopt Apache `text`/`rope`/`sum_tree`"), this is not
  possible: `text` and `rope` are GPL.
- What remains Apache is `sum_tree` (plus `collections` and `util`), and Tessera
  already gets it through `gpui-pre-sum-tree`. So (b) reduces to option (a)'s
  "sum_tree transform layer" step, or to a from-scratch editor element (c2).

### (c1) Another component editor

gpui-kit's `Editor`/`EditorState` *is* the gpui-component editor. It has LSP-style
providers, folding, a `WrapMap`/`FoldMap` `DisplayMap`
(`base/src/input/editor/display_map/`) and a tree-sitter highlighter. There is no
other GPUI editor with a compatible licence and a better model.

### (c2) A Tessera-owned editor element on gpui + ropey/sum_tree

This is the "real" Zed hommage: a buffer with anchors and transactions, a
sum_tree layer stack (conceal → replacement → wrap → block), variable row heights,
and a custom `Element` with its own IME handler.

- It gives full control over headings, blocks and multi-cursor.
- Cost: about 4–6 engineer-months to reach today's managed-editor acceptance level
  again: IME, grapheme rules, exact clipboard, recovery and attribution all have
  to be re-proven.
- It only makes sense if (a) hits a wall that gpui-kit upstream will not accept.

### (d) GPL path: reuse Zed's editor crates (requires relicensing)

Evaluated in §7.

## 7. GPL path: what reusing Zed's GPL crates would take

### 7.1 Which crates, and how coupled they are

Transitive closure over workspace crates, from the `[dependencies]` sections of
each `Cargo.toml` (computed with the `closure.py` script in the appendix):

| Entry crate | Workspace crates reached | GPL among them | Notable heavy dependencies |
|---|---|---|---|
| `rope` | 7 | 1 | `sum_tree`, `ztracing` |
| `text` | 12 | 3 (`text`, `rope`, `clock`) | none |
| `multi_buffer` | 51 | 31 | `language`, `lsp`, `settings`, `fs`, `git`, `rpc`, `proto`, `telemetry`, `task` |
| `editor` | **99** | **73** | `workspace`, `project`, `client`, `rpc`, `db`, `dap`, `terminal`, `remote`, `worktree`, `extension`, `language_model`, `prettier`, `node_runtime`, `git_hosting_providers` |

`collab` is not reachable from `editor`. But `client`, `rpc`, `proto`, `db` and
`remote` are.

The coupling is structural, not incidental:

- `Editor` holds `Option<Entity<Project>>` and `WeakEntity<Workspace>`.
- Settings come from the global `settings` store, which needs `SettingsStore`
  initialisation and the `settings_content` schema.
- Theming goes through `theme`/`theme_settings`.
- Buffers are `language::Buffer` inside a `MultiBuffer`. The display map works on
  `MultiBufferSnapshot`, not on a plain text snapshot.

### 7.2 What would have to be stubbed or forked

1. **gpui version.** Zed's crates build against in-tree `gpui` at a git revision.
   Tessera and vendored gpui-kit pin `gpui-pre 0.3.x` from crates.io. Two gpuis
   cannot share `App`/`Entity`/`Window`. Tessera and gpui-kit would have to move
   to Zed's git gpui and follow its churn: 177 commits under `crates/gpui` in the
   last three months. Every gpui-kit update would also have to match.
2. **`workspace`/`project`.** These would have to be stubbed or real-but-unused
   (fs, worktree scanning, LSP store, DAP, terminal), or Zed would have to be
   forked to cut `editor` loose. Zed does not maintain an "editor without
   workspace" build.
3. **Settings and theme.** Initialise Zed's `SettingsStore` and theme registry,
   and map Tessera's theme onto Zed's theme schema.
4. **Live Preview itself is still unsolved.** Zed has no caret-driven reveal and
   no variable line height, and its blocks are whole rows. We would be forking
   `editor/display_map` to add the same things option (a) adds, in a 155k-line
   crate we don't own.

### 7.3 Maintenance cost of tracking upstream

Commits on Zed `main` since 2026-07-06 (three months, 1,474 commits in total):

| Path | Commits |
|---|---|
| `crates/editor` | 197 |
| `crates/editor/src/display_map` | 14 |
| `crates/gpui` | 177 |
| `crates/language` | 70 |
| `crates/multi_buffer` | 18 |
| `crates/text` | 8 |
| `crates/rope` | 5 |
| `crates/sum_tree` | 4 |

`editor` is not published on crates.io and has no semver API, so every update is a
merge into a fork. In practice, the GPL path means either pinning an old Zed and
falling behind, or spending a recurring share of an engineer on rebases.
`text`/`rope`/`sum_tree` are stable (17 commits combined) and self-contained.

### 7.4 Effort compared with the MIT path

| GPL variant | What it brings | Effort | Verdict |
|---|---|---|---|
| Depend on `editor` (git) | Mature editing, multi-cursor, vim, folds, blocks | 6–10 wk to integrate (gpui migration, stubs, theme), plus the Live Preview fork work (same as MIT), plus ongoing rebases | **No.** It imports an IDE to get a text box, and still lacks the Live Preview primitives. |
| Vendor `editor/display_map` only | Inlay/fold/wrap/block layers | 4–6 wk to re-target from `MultiBufferSnapshot` to a plain buffer, and the `ui`/`theme`/`settings` dependencies still have to be cut | **No.** The port costs about as much as writing the layers fresh in (a). |
| Vendor `text`+`rope`+`clock` | Anchors, transactions and undo grouping, `Patch` edit composition, a battle-tested rope | 1–2 wk to vendor, but these replace ropey inside gpui-base, which is effectively option (c2) | **Only with (c2).** |

**What GPL concretely buys:** the right to copy about 11k lines (including
tests) of proven buffer/anchor/undo code (`text`, `rope`, `clock`) and about 21k
lines of display-map layer code as a starting point for a Tessera-owned editor (c2). It saves perhaps 4–6
weeks of a 4–6-month c2 project. It does **not** shorten option (a), which is
where the recommended work happens.

## 8. Risks

- **IME (CJK, Russian, dead keys).**
  - Both gpui-kit and Zed sit on gpui's Apache platform IME bridge.
  - Tessera already fixed projected IME geometry and grapheme boundaries
    (patches 0012/0017), and reveals any block an IME composition touches.
  - Remaining risk: replacement runs and blocks. A composition must never start
    inside a rendered element, so the containing block must be revealed first.
  - Native QA needed: macOS Kotoeri/Pinyin, Linux IBus/Fcitx (Wayland and X11),
    and Russian ЙЦУКЕН plus the macOS accent popover.
  - **Unverified here**: no GUI was run in this research.
- **Undo across rendered blocks.**
  - Blocks are presentation, so undo restores source bytes and the block
    re-renders.
  - Risks: toggle-style edits (checkbox, table cell edit) must be one transaction
    with a sensible caret restore. Undo of an edit inside a now-collapsed block must
    reveal it, otherwise the user sees nothing change.
  - Use the existing `EditIntent::Atomic` for these, and add nested transactions
    for multi-edit commands.
- **Very large notes.**
  - Today the editor falls back to Source above 64 KiB or 4,096 nodes. A 60 KiB
    dense note already loses Live Preview.
  - Fix: block-local classification plus a sum_tree projection. Keep a hard upper
    bound (for example 1 MiB → Source) with a clear, non-shifting status
    indicator.
- **Variable line height.** Larger headings and images break the
  uniform-row assumption in gpui-base's `WrapMap`, its element and vertical
  movement. This is the largest single native change and the most likely to need
  upstream buy-in.
- **Accessibility.** Neither Zed's editor nor gpui-base input publishes AccessKit
  text nodes today. Rendered blocks need text alternatives (image alt, table as
  text). Track this as a separate issue; it is not blocked by the chosen option.
- **Patch-stack growth.** Each native slice adds vendor diff lines. Upstream the
  projection seam before adding blocks.
- **Windows.** Reader source editing is Unix-only today. Live Preview inherits that
  until #354's Windows path exists.

## 9. Recommendation and slice plan

### If Tessera stays MIT (recommended)

Evolve option (a). Use Zed for ideas. Reuse only Apache `sum_tree` (already
present). Keep comrak as the classifier; do not switch to tree-sitter-md.

| Slice | Content | Estimate |
|---|---|---|
| **S1: Live Preview for ordinary notes (the "first useful slice")** | In `reader_editor.rs`, replace `ExactSource` with the managed `CachedProvider` (move it from `brain/` to a shared module). Add a Source/Live Preview toggle next to the existing source control: a glyph button with tooltip, ⌘E / Ctrl+E keeps toggling Source. Same headings/bold/italic/strike/code/links/wikilinks set. Reuse the managed native acceptance matrix (caret, arrows, Home/End, drag, IME, undo across reveal, exact copy, Save/conflict/recovery). | 3–5 d plus native acceptance |
| S2: link colour and heading weight | Add an optional colour field to `ProjectionStyle` (vendor patch), and colour links and wikilinks. Headings get bold and colour (size waits for S6). | 2–3 d |
| S3: lift the size walls | Block-local classification: split at top-level block boundaries, cache per-block results by byte hash and generation, re-run comrak only on dirty blocks. Keep a whole-document fallback for fences, HTML, lists and link definitions. Replace the 4,096-node whole-document cap with a per-block cap. Target: dense notes up to 1 MiB under 2 ms per keystroke of classification work on this host. | 1–2 wk |
| S4: sum_tree projection | Re-implement `source_projection` mapping as a `SumTree<Transform>` with `{source, display}` summaries and incremental `sync(edits)`, à la Zed's InlayMap/FoldMap. Caret moves become O(log n), and compose stops copying the whole text. | 2–3 wk |
| S5: replacement runs | Inline elements standing in for source ranges: wikilink chip, task checkbox (one-transaction toggle), inline image icon. Needs a vendor seam for element runs inside a line. | 1–2 wk |
| S6: variable row metrics | Heading sizes and pixel-height rows in gpui-base `WrapMap`/element/movement. Upstream discussion first. | 3–4 wk |
| S7: block replacement | Images, embeds, tables and callouts rendered with Reader element factories, as `Replace` blocks with measured heights. They are caret-atomic, and the source is revealed on click. | 3–4 wk |
| Later | Wikilink completion and hover preview through the existing `CompletionProvider`/`HoverProvider`; lists, blockquotes and callouts conceal; multi-cursor (3–4 wk); accessibility (separate issue). | — |

Total to Obsidian-like parity (S1–S7): about 3–4 engineer-months. S1 alone
delivers the slice the issue asks for first.

### If Tessera switches to GPL

- **Same plan.** Relicensing does not change S1–S7: the blocking work (caret-driven
  reveal, variable heights, rendered blocks over a single exact buffer) is not
  something Zed has.
- Do **not** depend on `editor`. It costs the gpui pin, ~99 crates and constant
  rebases, and it still lacks the Live Preview primitives.
- GPL becomes worth it only if a later decision chooses (c2), a Tessera-owned
  editor element, for example because gpui-kit upstream rejects variable row
  metrics or multi-cursor. Then vendor `text`+`rope`+`clock` for anchors and
  undo, and use `editor/display_map` as a reference to port. That saves about
  4–6 weeks of a 4–6-month project.
- Relicensing for the editor alone is not recommended. If the owner relicenses
  for other reasons, revisit this before S6.

## 10. Open questions

- Should Live Preview become the default mode for ordinary notes after S1, or
  stay opt-in until S3 lifts the size walls?
- Is the line-level ("cursor line") reveal Obsidian uses preferred over the
  current block-level reveal (whole paragraph)? Block-level is safer for
  multi-line emphasis; line-level matches Obsidian's feel.
- Will `longbridge/gpui-kit` accept a generic projection seam and variable row
  metrics upstream? The answer decides between (a) and (c2) for S6/S7.
- Real-vault size distribution: the walls in §5 come from a synthetic dense
  corpus. The note-size histogram of the owner's vault would size S3's priority.

## Appendix: reproducing the measurements

Scratch crate outside the repo (`[workspace]` stand-alone, with Tessera's
`Cargo.lock` copied in so versions match). Build and run with
`cargo build --release --offline && ./target/release/smart-editor-bench`.

`Cargo.toml` dependencies:

```toml
tessera-core = { path = "<tessera>/crates/tessera-core", default-features = false }
comrak = { version = "=0.47.0", default-features = false }
tree-sitter = "=0.26.13"
tree-sitter-md = { version = "=0.5.3", features = ["parser"] }
tree-sitter-rust = "=0.24.2"
```

Method:

- **Corpus.** Front matter followed by repeated sections. Each section has an
  H2; a paragraph with `**bold**`, `*italic*`, `~~strike~~`, `` `code` ``,
  `[link](url)` and `[[Wiki Note|label]]`; a second paragraph with
  `__bold__`/`_italic_`/`[[Another note]]`; a three-item list with two tasks; a
  2×2 pipe table; and a fenced Rust block. Sections repeat until the target size.
- **Timing.** One warm-up, then the median of N runs (`std::time::Instant`).
- **comrak.** `comrak::parse_document` with default options.
- **Classifier.** `tessera_core::source_classifier::classify(&Snapshot)`, then
  `source_projection::project` with a collapsed caret at a line start in the
  middle of the document.
- **tree-sitter-md.** Full parse with `MarkdownParser::parse_with_options`
  (block + inline). The incremental case applies `MarkdownTree::edit` with an
  `InputEdit` for inserting one character inside a paragraph, then reparses.
  Block-only: `tree_sitter::Parser` with `tree_sitter_md::LANGUAGE` — full parse,
  reparse with the unchanged old tree, and reparse after the edit.
  `changed_ranges` is reported (0: the insert does not change block structure).
  A guard checks that the new root ends at the edited length.
- **Control.** tree-sitter-rust on 256 KiB of small functions: full parse, no-op
  reparse, and reparse after a one-digit insert.

Crate-closure script used for §7.1 (run as `python3 -I closure.py <zed> <crate>`):

```python
import os, re, sys
root, start = sys.argv[1], sys.argv[2:]
def deps(c):
    p = os.path.join(root, 'crates', c, 'Cargo.toml'); out = []; sec = None
    for line in open(p):
        m = re.match(r'^\[(.*)\]', line)
        if m: sec = m.group(1); continue
        if sec == 'dependencies' or (sec and sec.startswith('target.') and sec.endswith('.dependencies')):
            m = re.match(r'^([a-zA-Z0-9_-]+)\s*[.=]', line)
            if m and os.path.isdir(os.path.join(root, 'crates', m.group(1))): out.append(m.group(1))
    return out
seen, stack = set(), list(start)
while stack:
    c = stack.pop()
    if c not in seen: seen.add(c); stack += deps(c)
print(len(seen), sorted(seen))
```
