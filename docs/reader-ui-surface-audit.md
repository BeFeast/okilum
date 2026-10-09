# Reader UI surface audit (#621)

Source baseline: `bb566fde` (Properties #831 merged), 2026-10-08.
This is a source inventory, not native visual acceptance. It does not change UI
behaviour or close #621. Rule numbers refer to the approved project-brain note
`Dev/Areas/okilum/design/ui-rules.md`. Source paths below are relative to
`crates/okilum-shell/src/`. Sizes are relative follow-up scope: S = one surface,
M = shared presentation or several states; they are not delivery estimates.

## Confirmed remaining source findings

| Surface and trigger | Source | Rule / observed evidence | Proposed bounded follow-up | Size |
|---|---|---|---|---|
| Recover notes: request draft recovery | `reader_source_history.rs::source_history(true)` | 7: primary copy contains retention limits (`20 versions`, `30 days`, `128 MiB`); rows format a path, raw timestamp and internal `protected` flag. 8: verbose empty-state instruction. | Human note names/dates; short recovery explanation; retain full identity, retention policy and protection reason in details. Preserve recovery selection and every revision/overwrite guard. | M |
| Recover link moves: open retained operations | `reader_move.rs::recover_link_moves` | 7: primary implementation-oriented paragraph starts “Original bytes are retained”; warning strings are rendered directly. | Audit each failure state before replacing copy; concise action guidance, full diagnostics on demand. Never weaken stale-content refusal. | M |
| Empty document: close the selected note / open empty vault | `reader_document_menu.rs::render_empty_vault` | 8: the `Recent` heading is unconditional even when no recent notes survive hidden-file filtering. | Omit the heading and list together when the visible recent set is empty. Preserve New note/Search/shortcuts and populated recent navigation. | S |

The Recover notes branch is distinct from ordinary Note history: `source_history(false)`
returns through `open_timeline`. Do not reintroduce the old history modal or duplicate
#715. Raw paths in explicit diagnostic details and ambiguity disambiguation are not
blanket violations: removing identity there can make recovery unsafe.

## Surface and trigger inventory

“Inspect” means no visual verdict has been established by this source audit. Existing
merged work is a baseline to preserve, not permission to claim current native PASS.

| Surface / trigger | Source anchor | Rule focus / current disposition | Next action | Size |
|---|---|---|---|---|
| App menu / More | `reader_app_menu.rs` | 4, 9: glyphs, tooltips and platform labels | Inspect Linux and platform wording | S |
| Document menu / document header controls | `reader_document_menu.rs::render_document_header` | 4, 5: consistent controls; authored title is content | Inspect normal and narrow widths | S |
| Breadcrumbs / path navigation | `main.rs::render_breadcrumbs` | 7, 9: human navigation without losing ambiguity | Inspect duplicate names | S |
| Tree context menu / file row secondary click | `reader_tree.rs` | 1, 4, 9 | Inspect menu and inline rename; do not replace filesystem guards | M |
| New note/folder / creation actions | `reader_create.rs` | 1, 2: inline creation and Undo | Preserve existing flow; inspect collision/error states | M |
| Template chooser / New from template | `reader_templates.rs` | 1: popup conversion already merged in #736 | Preserve dismissal/no-write behaviour | S |
| Quick open / command shortcut | `quick_open.rs` | 5, 8, 10 | Inspect keyboard selection and empty query | S |
| Search contents / content-search shortcut | `quick_open.rs` | 5, 8, 10: #772 already merged via #782 | No duplicate implementation; retain outstanding Mac-specific verification | S |
| Find in note / find action | `main.rs::render_find_bar` | 3, 5, 8 | Inspect narrow width and no-match state; coordinate Find owner | S |
| Recent switcher / recent navigation gesture | `reader_recent.rs::render_recent` | 5, 10; cancellation has separate work | Inspect only after current cancellation changes settle | S |
| Move destination picker / Move action | `reader_move_picker.rs::render_move_picker` | 1, 7, 8 | Inspect empty destination and duplicate-folder labels | M |
| Link-update review / move that affects links | `reader_move.rs` (`open_dialog`) | 1: risk-bearing multi-file review; modality alone is not evidence to remove consent | Inspect density/details; preserve explicit reviewed changes | M |
| Trash confirmation / affected links | `reader_trash.rs` (`open_dialog`) | 1, 2: distinguish risky linked deletion from ordinary Undo flow | Inspect linked/unlinked cases separately | M |
| Trash/move/create result and Undo | `reader_toast.rs`, `reader_trash.rs`, `reader_move.rs`, `reader_create.rs` | 2, 3: overlay and Undo lifetime | Inspect no layout shift, one result, correct expiry | M |
| Save conflict / recovery actions | `reader_editor.rs::render_save_status`, `reader_toast.rs` | 3, 7: user action vs raw errors | Inspect real conflict; preserve source bytes | M |
| Note history / history action | `reader_timeline.rs` | 6, 7: #715 baseline | Inspect populated/empty timeline, not legacy recovery list | S |
| Recovery preview / select retained draft | `reader_source_history.rs::preview_source_version` | 1, 4: destructive restore needs review; read-only preview is functional | Inspect safe cancellation and diagnostic disclosure | M |
| Unreadable-items report / attention action | `reader_diagnostics.rs::show_unreadable_items` | 7: short summaries and optional full report already present | Preserve #672; inspect many items / narrow viewport | S |
| Properties inline/panel / expand Properties | `main.rs::render_properties_strip`, `render_properties_grid` | 6, 7: #831 removes outer frame and primary YAML diagnostics | Merged; preserve valid/malformed light/dark evidence | S |
| Backlinks / right panel | `main.rs::render_backlinks` | 7, 8: keep explicit ambiguity, suppress empty noise | Preserve #657; inspect duplicate titles | S |
| Outline / right panel | `main.rs::render_outline` | 8, 10 | Inspect long headings, empty outline and narrow panel | S |
| Table expansion / wide table | `main.rs::render_table_overlay` | 1, 6: intentional larger reading surface | Inspect close/focus/scroll; do not infer an unwanted dialog from overlay alone | S |
| Drawing expansion / expand drawing | `reader_drawing.rs::open_expand` | 4, 5: glyph zoom controls already present | Inspect zoom/reset and small viewport | S |
| Drawing error / failed parse or render | `reader_drawing.rs` | 7: #740 concise copy and on-demand diagnostics | Preserve #770 geometry fix and existing evidence | S |
| PDF unavailable / embedded PDF failure | `reader_pdf.rs::render_unavailable` | 7, 8 | Inspect unavailable/oversized/password cases with PDF owner | M |
| Loading / document or vault loading | `reader_loading.rs::render_loading` | 3, 7 | Inspect delayed load and transitions; coordinate loading owner | M |
| Keyboard shortcut sheet / help action | `reader_shortcuts.rs::render_shortcut_sheet` | 5, 8, 9 | Inspect filtered empty state and platform keys | S |
| Reading controls / reading control popup | `reader_reading_controls.rs::render` | 4, 5 | Inspect text size/width alignment | S |
| Settings: Appearance / settings navigation | `reader_settings.rs`, `Section::Appearance` | 4, 5; labelled theme segments are explicitly allowed | Inspect baseline and theme selection | S |
| Settings: Files / settings navigation | `reader_settings.rs`, `Section::Files` | 5, 7, 8 | Inspect no-vault and populated states | S |
| Settings: Updates / settings navigation | `reader_settings.rs`, `Section::Updates` | 3, 7, 9 | Inspect update states with release owner | M |
| Settings: About / About action | `reader_settings.rs`, `Section::About` | 1: #810 moves About into Settings | Preserve single Settings window routing | S |
| Settings: Inbox / settings navigation | `reader_settings.rs`, `Section::Inbox` | 7: explicit optional connection explanation | Inspect current disconnected state; no new integration implied | S |
| Settings: Sync / settings navigation | `reader_settings_sync.rs` | 5, 7: operational identity and recovery safety matter | Coordinate Sync owner; do not hide actionable state | M |
| Startup and empty vault / no selected note | `reader_startup.rs`, `reader_document_menu.rs::render_empty_vault` | 8: empty Recent heading confirmed above | First small implementation candidate | S |

## Implementation and acceptance order

1. Start with the empty Recent heading: small, independently testable, no mutation.
   Use zero recents, all recents filtered out, and visible recents as positive control.
2. Take Recover notes presentation separately. Establish native fixtures for retained,
   protected and absent drafts before changing text or labels. Keep canonical paths
   available in details and test same-title entries; no change to durable recovery.
3. Coordinate other surfaces with their owners before editing shared render files.
   Existing issue claims and merged PRs take precedence over this list.

Every subsequent UI PR needs native Linux before/after in light and dark, the same
fixture/window size, keyboard/click checks and one review. A source inspection cannot
prove visual spacing, contrast, font size, focus or platform equivalence. This docs-only
inventory has no new screenshot or native PASS claim. Hosted commit CI is used before
PR publication; no heavy cargo runs on maestro.
