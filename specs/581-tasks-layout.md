# Tasks query layout (#581)

Owner QA found the working Tasks renderer too sparse and noisy to use.
Use compact rows, theme checkboxes, note-name backlinks, due pills with overdue
color, monochrome priority, and source-note grouping by default. Honor supported
explicit query grouping and keep unsupported clauses visible. Page at 20 rows.
Collapse exact carried copies across source notes with expandable backlinks;
differing metadata or same-note repetitions remain distinct.
Preserve the read-only source contract and incremental refresh/navigation.

Verify with focused Tasks tests, fmt/clippy, one GLM review, CI, and before/after
screenshots of Tasks Dashboard.md on muninn workspace 5. Deliver a merged PR and
published Linux beta; owner QA decides acceptance.
