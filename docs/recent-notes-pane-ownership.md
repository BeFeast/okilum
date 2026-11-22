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

The unchanged-production run on `86506061e` confirmed both failures:
[hosted shell controls](https://github.com/BeFeast/okilum/actions/runs/45033853247).
Control release navigated the new active pane from A to C, and a focus change
left an already queued gesture open. A separate existing cached-frame timing
assertion also failed in that run; it is not evidence for this gesture defect.

Actual pane activation cancels the existing switcher state, including its release
generation. The queued callback therefore becomes stale, and a later release has
no selected target to commit. Save/exit's temporary swaps do not call activation,
so they keep a valid current-pane gesture. No navigation, save guard, source API,
MRU ordering or release delay changes.
