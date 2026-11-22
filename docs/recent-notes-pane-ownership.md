# Recent-note gestures belong to their starting pane

Follow-up contract for [#773](https://git.oklabs.uk/BeFeast/okilum/issues/773).
The existing Ctrl+Tab / Ctrl+Shift+Tab switcher already freezes an MRU list,
highlights without navigating, and commits once Control is released. It uses
the ordinary guarded note-opening path. No second switcher or tab bar is needed.

The overlay and deferred release are window-owned UI state. An explicit change
of the active pane must cancel that gesture, including a release already queued
by the compositor. A later modifier event must neither navigate the new pane nor
return focus to the old pane. A fresh gesture in the newly active pane still
works normally. Temporary pane swaps used by save/exit guards are not user focus
changes and must not cancel unrelated UI state.

The regression controls create two real Reader panes and three canonical notes.
They select a third note, change pane focus before or after queuing Control
release, then require both pane documents to stay put. A positive control starts
a new gesture in the new pane and requires it to navigate. Canonical bytes stay
unchanged. These controls must be run against the unchanged implementation before
claiming a defect or a fix; source inspection alone is not runtime evidence.
