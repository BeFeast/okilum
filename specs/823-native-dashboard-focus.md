# Keep shortcuts after native Tasks navigation (#823)

A rendered heading link transfers the old Markdown view's focus to the new
TextView. For a native Tasks dashboard that TextView is hidden, so it has no
Reader key dispatch path. The visible native surface must take that focus.

Transfer only focus belonging to the replaced, hidden Markdown view when Tasks
is selected. Do not steal focus from Find, the sidebar, or another window, and do
not wait for asynchronous heading landing to change focus. Canonical Markdown,
query rendering, and guarded writes stay unchanged.

Regression: focus an ordinary note, prove keyboard Find works, follow its heading
link into Tasks, then use keyboard Find and type a query. Verify a real filtered
result and Ctrl/Cmd+K after dismissing Find. Linux light/dark before/after captures
must use the rendered link and keyboard, with ordinary-note Find as a positive
control.
