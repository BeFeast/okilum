# Indexed task mutation boundary (#636)

The native worker receives the immutable Tasks index and exact occurrence captured
by the displayed row. Open the source through the existing FileEditor lock/draft
and path guards, then derive the edit target from that locked source and displayed
index. Do not fetch a fresh index or capture a new target independently of the row.

Reuse the existing plan, save and opaque Undo receipt. Stale source (including a
prose-only change), invented occurrences, active editor locks and dirty recovery
must refuse without changing canonical Markdown. No FileEditor changes, native
controls, service endpoints or vault migration are part of this slice.
