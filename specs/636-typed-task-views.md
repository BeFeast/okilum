# Typed Tasks edit plans (#636, follow-up to #670)

The merged #670 owns typed_view selection and section parsing. This slice adds
only revision-bound task_edit plans and guarded Undo preimages, with no UI or file
writes. Native layout/Settings and safe-write integration remain later slices.

Test exact Unicode/BOM/CRLF, nested/quoted tasks, repeated occurrences, malformed
metadata, external changes and stale Undo. fmt, core clippy, one PR-Agent review
and required CI before merge. UI slices require Linux light/dark before/after.
Owner vault stays untouched; native QA is manager-owned.
