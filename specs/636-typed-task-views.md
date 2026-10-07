# Typed Tasks safe-write and Undo (#636)

The merged #670 owns typed_view selection and sections; #674 owns revision-bound
edit plans. This slice commits plans through the existing Unix FileEditor and
returns an opaque receipt for revision-checked Undo. Snooze changes scheduled date
while preserving due. No-op actions do not save. Active editors, dirty recovery,
invalid paths and stale revisions refuse the write. Native dashboard activation,
Settings and UI remain subsequent slices.

Test exact Unicode/BOM/CRLF save and Undo, durable history, active locks, retained
dirty recovery, external changes, vault binding, symlink replacement and malformed
scheduled metadata. fmt, core clippy, one PR-Agent review and required CI before
merge. Owner vault stays untouched; native QA is manager-owned.
