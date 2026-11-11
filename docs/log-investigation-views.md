# Log investigation views

Issue #2398 extends the existing LogView and saved-query storage. It introduces no
new renderer, credential store, or write operation on a log source.

## Follow and Back

The cell actions menu offers **Follow this request** for detected correlation
fields. The selected record's details offer the same action. **Use as correlation
field** marks another field for the current canonical file or Kibana source.
Known aliases and W3C traceparent detection continue to work alongside that choice.

A follow pushes filters (including disabled typed chips), advanced query text,
columns and widths, field choices, time scope, selection, range anchor and scroll.
Back restores that frame. Its button, breadcrumb and loaded count share the
single conditional query row, including at 700 px. The shortcut is Cmd+[ on macOS and Alt+Left on Linux and
Windows. Nested follows retain their breadcrumb and individual Back frames.

A local follow searches loaded rows across enabled files, clears unrelated local
filters for the child, and orders matches by timestamp. Its count explicitly says
Loaded rows. Clearing child chips keeps the request constraint and the Back frame.

The remote navigation hook is Navigation in reader_log/view_stack.rs. The adapter
in reader_log_remote/investigation.rs clones the **applied** source, retains KQL
and scope filters, freezes its current absolute time window, adds a server
correlation clause and sorts ascending on the configured time field. It reads
all pages up to the existing bounded history limits. The current credential is
reused only for the same source identity and endpoint. Navigation never overwrites
the source definition. Pending or failed server queries cannot display a complete
request count from the parent's snapshot.

Back restores a retained immutable EventWindow and its source/query/scope/sort,
range and quality without HTTP. Existing history cursors are closed. Apply opens
a new history traversal; Start can immediately follow the restored query.
ResultStatus supplies partial, truncated, unknown-total and lower-bound labels.

## Message and live

Columns > Choose message field lists observed string fields. An existing message field is
the default; other field names are never guessed. The choice is remembered per
canonical file/source. Rows stay one line and bounded; the selected message is
shown in full, wrapped, above the other detail fields. Original JSON and raw copy
retain every field unchanged.

Follow/Start and Stop retain the active filters, columns and field choices. A
request drill-down keeps its correlation constraint during live reconciliation.
Kibana keeps the existing document-identity deduplication and bounded complete
window reconciliation. Live status includes the last completed update time.

## Saved views

The existing Saved menu now offers Save current view even without active filters.
New entries retain the source, time preset, scope/query, typed chips, columns,
message and correlation choices, and optional request context. Remote absolute
coverage is separate from its reusable time preset. No API keys are serialized.
Saved remote sources must already belong to the current vault. A same-source
restore reuses its scoped credential and re-queries all pages; another source
uses its own OS credential identity. Saved file views reopen their canonical file
off the UI thread. Missing sources and invalid queries report errors explicitly.
Older entries without a source remain portable filters.

## Synthetic QA

The fake_kibana example accepts investigation. It serves repeated request ids
under standard aliases and an arbitrary flow field, hour-spaced timestamps, and
long details with no message. New events continue arriving, including late events.
All endpoint, index and field examples in this fixture are synthetic.

Acceptance tests cover cell and detail navigation with separate press/release
frames, exact Back restoration and shortcut routing, message/source persistence,
file follow, server query scope and all pages, partial/truncated quality, live
deduplication, and saved-view round trips. TestPlatform's absent credentials are
not used to claim that a real authenticated query succeeded; server tests use the
synthetic key through the existing worker transport.

Remote-result readiness (#2446) has two completion points: the panel receives the
worker's result, then the table publishes its filtered rows. Tests observe each
entity's notifications separately. Replacing a remote window cancels its previous
filter and clears the running flag before starting the new one; otherwise the
replacement can queue forever behind a cancelled task. A regression forces this
ordering before either filter can run, independently of fake-server timing.
