# Read-only Tasks query blocks

The Reader displays fenced `tasks` queries using Markdown checkboxes from the
vault. Files remain the source of truth; the query index is disposable and rebuilt
from the Reader source snapshot. The existing incremental watcher updates only
changed/deleted notes. Results do not modify checkboxes or metadata.

Supported clauses (one per line, filters combined with AND):

- `not done`, `done`, `no due date`
- `due before|on|after <date>` and `due <date>`
- `done before|on|after <date>` and `done <date>`
- Dates: `YYYY-MM-DD`, `today`, `in N days`, `N days ago`
- `sort by due|priority|path|done`, optionally followed by `reverse`
- `group by filename|path|due|priority|status` (multiple groups nest in order)

Multiple sorts are applied in order, with source path and line as deterministic
tiebreakers. Undated tasks sort last in ascending date order and first in reverse
order. Blank lines and lines starting with `#` are ignored. Template dates
must already be substituted. Unknown clauses are displayed as small unsupported
notices, with results withheld rather than silently ignoring a filter. This is
not the full Obsidian Tasks language; Dataview is not implemented.

Task metadata includes due (📅), scheduled (⏳), start (🛫), completion (✅), and
priority (🔺 ⏫ 🔼 normal 🔽 ⏬). Dates use the machine's local calendar date;
open query blocks refresh after midnight. Large lists show 20 results at a time
and expose the full count and a Show more action. Background index updates retain
the expanded page size; ordinary prose edits do not invalidate query results.

A source action validates that the indexed task still exists at its source
position before opening its containing list. Unique displayed text is then
revealed precisely and highlighted, as with existing Reader search navigation.
Repeated or unsupported display text leaves the containing
list visible with an explicit notice; a stale task must be refreshed before
navigation. Checkboxes are read-only. Native owner QA remains required on the
published beta, using the dashboard and a daily note with substituted dates.

Results default to source-note groups. Explicit grouping replaces that default;
query sort order is preserved within groups. Equal filenames from different
folders remain distinct groups. Compact rows show a disabled checkbox, task text,
a note-name backlink, a monochrome priority glyph, and a due pill (red only for
open overdue tasks). Metadata is hidden only in the result presentation; the
canonical task text and source navigation remain intact.

Exact copies carried between notes collapse under their first source group with
an expandable “×N notes” action. Every original backlink stays available on
expansion. Different statuses, dates, priorities, or two occurrences in the same
note remain separate. Explicit query groups never collapse across group boundaries.
