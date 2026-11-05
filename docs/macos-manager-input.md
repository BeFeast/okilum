# Native Manager input on macOS (#2521)

The native Manager uses the shared GPUI textarea and feed. Its compilation and
hosted tests do not change the macOS runtime panel: #2522 owns that switch.

Enter submits; Shift+Enter inserts a newline. Composer retains the Reader's
500 ms IME commit settle interval: an Enter during preedit or immediately after
the marked range clears cannot send. The regression uses EntityInputHandler's
real marked-text and commit callbacks, then advances the test clock and proves
that a subsequent Enter does send.

The app Edit menu exposes Undo, Redo, Cut, Copy, Paste and Select All. These use
the focused GPUI input actions; Copy also reaches the focused selectable feed
body. Paste keeps its existing deferred menu adapter and native clipboard
capture. Text paste replaces the current selection as one undoable edit.

Mac chat clipboard reads happen in the paste command. Explicit file URLs and
legacy Finder filename lists become attachments in clipboard order, without
checking the filesystem on the UI thread. The attachment worker handles missing
or unreadable files visibly. Ordinary text stays text, including paths into a
clipboard manager's cache. Note-editor smart paste keeps its existing policy.
PNG from AppKit retains its original bytes. TIFF (Preview / screenshot) is
converted losslessly to PNG on the bounded preparation worker because T3 does
not accept TIFF; output size budgets, previews and PNG filename/MIME stay in sync. Finder drops and the multi-file picker use the
same revision/scoped attachment pipeline as Linux.

## Hosted and live checks

`scripts/ci/check-macos.sh` runs non-empty filters for every native Manager
module and `platform::exact_macos_clipboard::native_tests`. This covers Enter,
Shift+Enter and marked-text commit; command shortcuts and the actual Edit menu
actions; selection replacement/Undo; PNG/TIFF on isolated named NSPasteboards;
file URL/legacy filename decoding; GPUI file drops; chooser options; and exact
image upload/send/provider bytes. These fixtures never touch the user's general
pasteboard, real T3 projects or credentials.

The owner still checks an active Mac application after #2522: real Japanese /
Chinese candidate windows, Hebrew/Russian dead keys, AppKit menu enablement and
key equivalents, screenshot/Preview/Safari clipboard acquisition, Finder copy
and drop delivery, and the visible NSOpenPanel with multiple images and PDFs.
The GPUI prompt response proves options and the attachment pipeline, not that
AppKit presents the panel. Hosted test names, counts and negative controls are
recorded in the PR; passing tests do not certify these live OS interactions.
