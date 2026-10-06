# Native Tasks query blocks (#557)

Markdown checkboxes and Tasks emoji metadata remain canonical. The Reader renders
read-only query results from a rebuildable in-memory projection of the existing
reconciled source snapshot. Initial indexing runs on the Ready worker; subsequent
updates parse only changed sources and remove deleted paths, through #487/#545.

Support the dashboard's status, due/done dates, relative dates, missing due dates,
and due/priority/path/done sorting. Daily templates are queried only after date
substitution. Unsupported clauses are disclosed and do not produce misleading
partial matches. Results include status, text, metadata, source navigation and
counts. No checkbox write-back, service integration or Dataview implementation.

Validate parsing/query boundaries, incremental replacement/removal, native block
rendering and navigation. Run fmt, affected-crate clippy and one GLM review before
PR delivery. Owner QA is on the published beta with the real dashboard/daily note.
