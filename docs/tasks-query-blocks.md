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

Multiple sorts are applied in order, with source path and line as deterministic
tiebreakers. Blank lines and lines starting with `#` are ignored. Template dates
must already be substituted. Unknown clauses are displayed as small unsupported
notices, with results withheld rather than silently ignoring a filter. This is
not the full Obsidian Tasks language; grouping and Dataview are not implemented.

Task metadata includes due (📅), scheduled (⏳), start (🛫), completion (✅), and
priority (🔺 ⏫ 🔼 normal 🔽 ⏬). Dates use the machine's local calendar date;
open query blocks refresh after midnight. Large lists show 50 results at a time
and expose the full count and a Show more action.

A source action validates that the indexed task still exists at its source
position before opening its containing list. Unique displayed text is then
revealed precisely. Repeated or unsupported display text leaves the containing
list visible with an explicit notice; a stale task must be refreshed before
navigation. Checkboxes are read-only. Native owner QA remains required on the
published beta, using the dashboard and a daily note with substituted dates.
