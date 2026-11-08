# Tree selection and batch operations (#2681)

Click a Tree row to select it normally. Ctrl on Linux/Windows or Cmd on Mac
toggles a row without opening its document; Shift selects the visible range
from the last anchor. Ctrl/Cmd+Shift adds a range. A normal click or arrow
navigation replaces the group; Escape or a click below the rows clears it.
Selection is local to the window. Hidden/folded rows leave the selection, and
switching vaults or tree scope clears it. The open document is independent.

With multiple rows selected, the Tree shows the count and Move, Rename and
Trash actions; a selected row's context menu acts on the group. Selected
folders include their descendants once. Single-row dragging is disabled while
a group is selected so dragging cannot silently move only one of its items.

Move takes a vault-relative destination folder (empty means vault root).
Rename shows an explicit editable filename for each item, retaining the Markdown
extension for notes. Move and rename retain the existing notes/folders scope;
attachments can move with their folder or go to Trash. Occupied or duplicate
destinations and paths outside the vault are refused before starting.

The confirmation lists the effective items and explains that operations run
serially, stopping at the first refusal or cancellation. Each item uses the
existing single-item path: unsaved draft and editor locks, revision checks,
link previews/rewrite journals, system Trash and Undo. There is no overwrite or
claim that the whole batch is atomic. Stop after this item keeps unstarted
items untouched. The final notice reports completed/total and a refusal reason.
Completed moves remain in Recover link moves; Trash Undo restores one item at a
time. A vault switch stops subsequent dispatch to the old vault.

Native checks exercise real modified clicks, visible batch controls, moves and
renames, retained recovery journals, a second-item lock refusal, skipped later
items, real Trash/Undo and unchanged canonical bytes. A compiled control removes
modified-click dispatch, must fail the actual mounted click test, then restores
the exact source and requires the same test to pass.
