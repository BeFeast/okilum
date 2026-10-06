# Tasks polish (#596)

Owner-approved follow-up to #581: remove repeated source names/dates from grouped
rows, show daily sources as human dates, attach honest task/occurrence counts to
section headings, hide empty tasks, collapse unsupported Dataview queries, and
raise backlink/priority typography to secondary-text size.

Keep source metadata and task identities intact. A grouped row's title opens its
exact source task; the group heading and expanded-copy backlinks stay navigable.
Counts exclude empty tasks and retain all nonempty occurrences. Unsupported
Dataview source is available explicitly and is never executed.

Validate focused native rendering/navigation and data tests, fmt/clippy, one GLM
review and required CI. Deliver PR → Linux beta → manager QA steps. Only the
manager's dedicated muninn QA sub-session takes after screenshots and runs GUI QA.
