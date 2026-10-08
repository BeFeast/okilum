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
