# Sidebar bulk folding (#712)

All four owner-requested controls: reversible Folders-only glyph in the sidebar
header, Alt/Option-click on a section header to collapse/expand all left sections,
primary+Shift+Left/Right for Folders-only/expand, and a visible collapse-all-folders
glyph in the Folders header. Preserve per-vault settings and the right Properties
panel. The toolbar restore snapshot persists with sidebar state; an ordinary
section toggle clears it so a later restore cannot undo intervening manual edits.

The former primary+Shift+Left Focus current action loses that binding; its existing
glyph/menu stays available. Add both new bound actions to the shortcut-sheet
catalog when #663 is available. New actions are already in the More menu.

Dependencies: local branch stacks on #692; publication waits for that merge/CI
slot. QA sub-session supplies Linux light/dark before/after; executors do not
operate muninn. No source-note writes. Test actual glyph clicks, Alt-click,
keyboard bindings, folder collapse, reversible persistence and Properties isolation.
