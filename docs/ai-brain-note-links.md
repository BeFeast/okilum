# Insert a note link

In a writable managed Source or Live Preview, place the caret or select plain
single-line text, then choose **Insert note link**. Search by title or path and
choose a note. Okilum checks the destination through its existing preview
resolver and inserts a root-relative wikilink, preserving the selected text as
its label. Duplicate basenames are distinguished by their full paths.

The link remains an unsaved edit. Undo/Redo and Source/Live Preview share the same
buffer and history. Use ordinary Save when ready; existing conflict and draft
recovery still apply. Cancel does not alter the draft. This action does not select
Context, start work or modify the destination note.

Finish any IME composition before opening the chooser. Changing the source,
selection, goal or workspace while it is open requires choosing again. A changed,
unreadable or unresolved destination is refused. Filenames with wikilink delimiters
or unsafe path syntax, and selections whose Markdown syntax cannot be preserved
as a plain label, are refused without changing the draft. Choose another note or
place the caret instead; Okilum does not guess an escaped filename.

The chooser lists existing readable Markdown sources in this workspace. Its list
is not a complete resolver inventory: the read-only preview check must return the
exact chosen path. Observed casefold/extension collisions refuse; this does not
promise detection of every hidden collision or prevent links breaking after a
later external rename.
