# Recent-note switching and adjacent panes (#773)

Design proposal for the approved no-tabs workflow. Delivery order remains
Ctrl+Tab, modified-link navigation, then split controls and persistence.

## Window and pane ownership

Keep one window-level workspace: vault session, watcher/index subscriptions,
sidebar, application commands and persisted window geometry. Extract document
state from Reader into a pane entity before mounting a second document. Each pane
owns its accepted document/canonical snapshot, navigation generation, history,
Read/Edit and Source/Live Preview mode, Find, selection and scroll (including
native Tasks). A stable pane ID identifies asynchronous work; completion checks
both pane ID and navigation generation. Closing or replacing a pane cannot
publish a late load into the surviving pane.

At most two panes, arranged left/right. The active pane determines sidebar
selection, Properties/Contents/backlinks, note commands and navigation targets.
Each pane keeps the existing pinned document header. Focus changes do not reset
history or scroll. One shared vault session continues to fan source/index changes
out to both panes; do not instantiate a second watcher, sidebar or cache owner.

## Editor ownership and safe exits

Use the existing FileEditor ownership and guarded save API unchanged. The same
canonical note may appear in both panes, but only one pane may own its editor.
The other displays the accepted saved source; it never pretends to show the
unsaved buffer. Attempting Edit there focuses the existing editor, with a concise
notice. This applies to sibling windows too; no cloned editor buffers, automatic
ownership transfer or conflicting recovery files. Task dashboard writes keep the
same lock/revision checks and refuse a source with an active incompatible editor.

Leaving an edited note, replacing/closing its pane, unsplitting and quitting use
the existing save/protect/conflict lifecycle. Failure retains the pane and draft;
no partially collapsed layout is persisted. Focus alone does not destroy the
editor. Normal focus-save behavior remains intact. On unsplit, keep the active
pane; close the other only after its lifecycle guard succeeds.

## Ctrl+Tab slice

Use actual Control on every OS, including macOS. At first press, snapshot the
current window's Recent MRU, deduplicate and exclude unavailable/non-note entries.
Put the active note at the start; first Ctrl+Tab selects the previous note,
Ctrl+Shift+Tab reverses. Repeated presses cycle the frozen list; do not navigate,
save or reorder Recent on highlight. Releasing Control commits one ordinary
navigation in the active pane. Escape/focus loss cancels. A conflict keeps the
current document and its draft; an unavailable selected note produces the normal
explicit notice. Zero/one candidates are inert.

Use a small overlay with note name and a muted location only when needed to
disambiguate equal names. No tabs, modal dialog or layout shift. Preserve existing
Quick Open and its saved Recent ordering; this is a distinct interaction.

## Modified links and staged split

Resolve the shared Markdown link model before deciding destination. Keep source
identity, heading/block fragment, ambiguity and external-link behavior. Primary
modifier-click opens a resolved local note in the other pane, creating the second
pane if needed. Primary+Shift-click requests a new window through the existing
window-opening path. Source mode retains its current modifier-click follow-link
gesture until the destination-routing slice explicitly updates that contract.

Because an adjacent-link slice must actually open beside, it includes the minimal
two-pane host and document-state extraction. It cannot be a routing-only PR that
claims completion without an adjacent destination. The final split slice adds
Open in split menus, split/unsplit shortcuts, focus shortcuts, divider resizing
and durable layout. Keep attachments/external URLs on their existing routes;
do not silently coerce an attachment into a note pane.

## Persistence

Extend the existing per-vault UI state with optional versioned pane snapshots:
one/two panes, active pane, bounded divider ratio, and each note's mode, history
and surface-specific scroll anchor. Old state restores as a single pane. Validate
paths using normal navigation, clamp invalid ratios and refuse unsupported state
without rewriting notes. A missing secondary note restores the usable primary
with a notice; recovery drafts stay in operational editor storage, never layout
JSON. Reopening two copies of one note restores at most one editing owner.

Follow #592: only the active sibling window owns the vault's persisted layout.
Explicit duplicate-window opening snapshots the caller's layout, then the windows
navigate independently. Native Tasks positions must not be stored as hidden
Markdown block offsets.

## Acceptance by slice

1. MRU state tests: frozen ordering, forward/back wrap, release exactly once,
   cancellation, deleted target and conflict preservation. Native tests from Read
   and editor focus; Linux light/dark overlay screenshots.
2. Routing and panes: unique/ambiguous links, heading destination, stale worker
   suppression, one watcher, same-note editor refusal and independent histories.
   Test rendered links, Source/Live Preview and note backlinks explicitly.
3. Layout: close/unsplit save failure, quit with two drafts, same-note restart,
   missing secondary note, scroll/history/modes round trip, sibling-window owner
   and Linux/macOS/Windows shortcut labels. Screenshots in both themes; published
   beta QA uses the dedicated muninn session.

## Navigation extraction checkpoint

Before mounting a second surface, group the existing per-document history,
positions, pending landing and load/reconciliation generations in
`reader_navigation::State`. Keep current Reader call sites and the persisted
layout schema; this is an ownership refactor with no new navigation behavior.
`reader_link_navigation` owns the existing prepared-link dispatch, including
ambiguity, unresolved targets, attachments and fragments. The extraction is a
small prerequisite PR, not delivery of adjacent links. Existing native regressions
for nested links, heading/Back, session restore and independent windows must pass
before destination routing is added.

## Reader decomposition (before the pane host)

`Reader` (`crates/okilum-shell/src/main.rs`) is one struct of about 150 fields.
Only `reader_navigation::State` is per-document today. A second visible document
needs the rest of the document state moved behind a pane-owned type first. Raw
`.field` reference counts below are upper bounds (some names also match other
structs), but they size the churn: `content` ~226 refs in 16 files, `current_rel`
~213/25, `loading` ~207/21, `editing` ~197/19, `link_notice` ~117/20,
`hover_preview` ~64/2, `backlinks` ~63/12, `file_preview` ~52/14, `note_source`
~40/9, `prepared_links` ~35/6. Expect roughly a thousand call sites; this is why
it is staged as mechanical moves, not one change.

### Field ownership

Per pane (`pane::Document`, keyed by a stable `PaneId`):

- Rendered document: `content` (+ `_content_sub`), `note_source`,
  `note_canonical_source`, `current_rel`, `current_title`, `outline`,
  `properties`, `properties_open`, `show_hidden_properties`, `backlinks`,
  `backlinks_expanded`, `use_html`, `table_overlay`, `timeline`, `file_preview`,
  `typed_navigation`, `hover_preview`.
- Find: `find_input`, `find_open`.
- Link preparation: `prepared_links`, `link_presentations`, `link_identities`,
  `link_choices`, `link_original_source`, `link_preparation_generation`.
- Load pipeline: `loading`, `pending_open_document`, `queued_open_note`,
  `usable_document`, `last_recorded_document`; every spawned task captures
  `(PaneId, generation)`.
- Editor and recovery for that document: `editing` (see ownership below),
  `recovery_offer/checked/error/dismissed/startup`.
- `navigation` (already extracted) and a pane `focus_handle`.

Window level (stay on `Reader`, one instance): vault and index (`vault`,
`searcher`, `vault_root`, `watcher*`, `incremental_*`, `tasks_index`,
`deferred_vault_changes`, `cache_lease`, `index_dir`), session and persisted UI
(`ui_state`, `shared_session`, `session_*`), sidebar and tree (`tree*`, `sidebar*`,
`inbox`, `projects`, `section_scroll`), panels and layout (`panels`,
`panel_*`, `body_*`, `resizing_panel`), file operations (`creation*`, `renaming`,
`move_*`, `trash_*`, `note_move_pending`), `recent_switcher`, `quick_open`,
`shortcut_sheet`, `backlink_titles`, `sel_format`, `reader_window`, toasts and
their generation counters (`notice_generation`, `history_notice_generation`),
`link_notice`/`displayed_*` presentation of notices.

Needs a decision in its own stage, not assumed here:

- `editing`: the per-pane value is a handle; ownership is a window-level
  registry `canonical note -> owning PaneId`, so the second pane of the same note
  is read-only (design above). Sibling windows keep using FileEditor's lock.
- `link_notice`: a window toast naming the originating pane, not pane state.
- Right-panel data (`outline`, `properties`, `backlinks`) is computed per
  document but rendered for the active pane only.

### Stages

Every stage is behavior-preserving until stage 4, merges alone, and keeps one
visible pane. Native regressions listed in the checkpoint above must pass at each.

1. **Document container.** `pane::Document` holding the rendered-document, find
   and link-preparation groups; `Reader` owns `panes: Vec<Document>` with one
   entry and `active`. Mechanical `self.x` to `self.doc().x`, done group by group
   in several small PRs so each diff is reviewable.
2. **Load pipeline binding.** Move the load group, key every background task and
   completion by `(PaneId, generation)`. Add the stale-result tests (late load
   after replace/close) while there is still one pane; positive control: a
   deliberately stale completion must be observed and dropped.
3. **Editor ownership registry.** Window-level owner map, guarded
   replace/close/unsplit hooks that run the existing save/protect/conflict
   lifecycle. Still one pane; tests drive the hooks directly.
4. **Second pane host + modified-link routing.** Render two documents, active
   pane drives sidebar/right panel, single watcher fan-out to both, same-note
   read-only second pane, Ctrl/Cmd+click opens beside, +Shift opens a window.
   First user-visible stage: Linux light/dark screenshots, muninn QA.
5. **Split controls and layout persistence** as specified above.

### Risks to watch

- The `Reader` render and `reader_loading` paths read many fields in one
  function; extracting groups can silently change borrow order and notify timing.
  Compare native heading/Back, session restore and window restore after each PR.
- Measurements for open/scroll performance (#653 line of work) need a baseline
  and comparison on one machine, in one session.
- Other executors edit these files (tree/menus, history, windows). Rebase often
  and land stage 1 groups quickly to keep the conflict window small.
