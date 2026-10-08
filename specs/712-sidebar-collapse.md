# Sidebar bulk folding (#712)

All four owner-requested controls: reversible Folders-only glyph in the sidebar
header, Alt/Option-click on a section header to collapse/expand all left sections,
primary+Shift+Left/Right for Folders-only/expand, and a visible collapse-all-folders
glyph in the Folders header. Preserve per-vault settings and the right Properties
panel. The toolbar restore snapshot persists with sidebar state; an ordinary
section toggle clears it so a later restore cannot undo intervening manual edits.

The former primary+Shift+Left Focus current action loses that binding; its existing
glyph/menu stays available. The executor landing #663 will add both new bound actions to its shortcut-sheet
catalog; #712 lands first without depending on that feature. New actions are already in the More menu.

Dependencies: local branch stacks on #692; publication waits for that merge/CI
slot. The executor supplies local Linux X11 fixture before/after in light and dark.
The QA sub-session owns strict UX and released-build verification on muninn;
executors do not operate that screen. No source-note writes. Test actual glyph clicks, Alt-click,
keyboard bindings, folder collapse, reversible persistence and Properties isolation.

## QA restoration correction

Bulk actions must restore the effective visible state, including temporary scroll
folding, rather than only persisted flags. Explicit expansion stays revealed at
the current tree offset; returning to the top resumes ordinary auto-folding.
The restore snapshot survives restart. Regression coverage includes all 32 states
of the five left sections through save/load and repeated scroll observation, plus
native glyph/Alt-click/shortcuts with content focus and effective visibility.

## Ordinary relaunch correction

Explicit reveals of Recent/Pinned/Inbox at a scrolled tree position persist in
sidebar state and are restored before the selected note positions the tree.
Automatic folding remains temporary. Returning to the top clears persisted
reveals through the existing ordered background sidebar writer. Unknown or
inapplicable reveal entries are ignored on load.

Regression: create a long tree, expand sections while scrolled, save, close the
window, and create a fresh Reader from the same profile. Assert the nonzero scroll
position and effective visibility of all five sections. Linux process evidence
also uses Ctrl+Q followed by a fresh launch with the same profile in both themes.
