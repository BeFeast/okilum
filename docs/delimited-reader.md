# CSV and TSV Reader (#869)

The Reader presents CSV/TSV as a read-only table. The first record is a pinned
header by default; **First row is header** is a visible header toggle (in ⋯ at
narrow widths), written per file and restored after restart. Preferences live in application state outside the
files. Muted row numbers count data rows. [CSV/JSON tables](data-file-viewer.md)
add worker sorting/filtering over the explicitly loaded preview and Open in DuckDB.

CSV detects comma, semicolon or tab from the first 32 logical records; TSV uses
tab explicitly. UTF-8 BOM is hidden in the derived view. Quoted separators,
escaped quotes and embedded CRLF/LF stay in cell values. Uneven rows are padded
for display, unexpected/incomplete quotes retain the parsed values, and a quiet
notice reports irregular rows or ambiguous separator detection. The source is
never rewritten by parsing, selection, copying or the header preference.

Rows and columns are virtual. Initial reads are bounded to 1 MiB, 1,002 records (including
header/lookahead), 64,000 cells, 256 columns and 64 KiB per decoded field. Files above 1,000 data rows
initially show that prefix with **Load more rows**. That explicit action reloads on a
worker with a 32 MiB / 100,001-record / 500,000-cell budget; the same column bound
and field bounds remain, including for full-value tooltip shaping. Files beyond those bounds retain a usable preview with an external-open
notice. Incomplete records at a byte boundary are omitted rather than presented
as malformed source; actual malformed EOF records remain visible. These limits
bound reading and allocation, not only rendering. Columns
scroll horizontally, numbers align right, and text follows its first strong
direction. Long values show an ellipsis and a full-value tooltip. Click selects
a cell; drag or Shift-click extends a rectangle. Copy produces TSV with quoted
tabs, newlines and quotes, or the plain value of a single cell.

The header uses the shared Table / Raw switch, Find field, header toggle and
Reset sort. CSV and TSV default to Table and remember explicit view choices
independently; JSON arrays keep their four-view switch and have no header toggle.
Raw text is loaded only on its first selection, with the code viewer's 64 MiB
budget, long-line paging and whole-file Find. Table previews can remain useful
when Raw exceeds that limit. Raw never writes source bytes.

**Find in loaded rows** filters matching rows and counts matching cells. Enter
and Shift+Enter select the next/previous matching cell and reveal it on both
axes; Match case changes the worker projection. Escape clears the query,
restores loaded rows and keeps the source-cell selection. Bursts are debounced
before a cancellable worker scan. Sorting follows the displayed row order;
Reset sort is disabled with “Nothing is sorted” until a sort is active.

One bottom status line reports grouped row/column counts, file size, sorting,
preview limits, malformed-row notices, no matches and Load more rows. A warning
badge by the name jumps to the first known ragged record, clearing a filter if
needed. Unknown warning locations explain why navigation is unavailable. A table
that cannot be read falls back to Raw, disables Table and retains the reason.

Copy cells is in Copy and uses the same keyboard binding as the grid, with a
toast after copying. Open with contains Open in DuckDB; while the process runs
it is checked and disabled as “DuckDB is open”. Launch failures use a toast with
Get DuckDB instead of adding a viewer row. ZIP entries and standalone tables
use the same compact slots planned against their measured pane width.

**Edit** opens the existing plain-text source editor and revision-aware FileEditor,
also used for TXT. There is no in-table editing or CSV serialization. Existing
delimiter, quotes, BOM and line endings survive unchanged outside the edited
span. Source recovery, explicit Save, lifecycle saves and conflicts use the same
outside-vault journal as note editing. Read reloads the table from saved source.
Markdown links and Live Preview are unavailable for plain files.

Native acceptance uses the same disposable fixture folder in Linux light/dark
BEFORE and AFTER builds produced by `commit-ci`'s `linux-binary` lane. Cover
comma/semicolon/TSV, quoted separators/newlines, Hebrew and numbers, malformed
rows, wide/long cells, a 10,000-row scroll and pinned header, header preference
after restart, rectangular clipboard paste, and a one-span edit with byte
comparison. Windows smoke follows the published beta; no real vault is edited.
