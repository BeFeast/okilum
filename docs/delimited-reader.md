# CSV and TSV Reader (#869)

The Reader presents CSV/TSV as a read-only table. The first record is a pinned
header by default; **No header row** in the document ⋯ menu changes that file's
view and survives restart. Preferences live in application state outside the
files. Muted row numbers count data rows. There is no sorting or filtering.

CSV detects comma, semicolon or tab from the first 32 logical records; TSV uses
tab explicitly. UTF-8 BOM is hidden in the derived view. Quoted separators,
escaped quotes and embedded CRLF/LF stay in cell values. Uneven rows are padded
for display, unexpected/incomplete quotes retain the parsed values, and a quiet
notice reports irregular rows or ambiguous separator detection. The source is
never rewritten by parsing, selection, copying or the header preference.

Rows are virtual. Initial reads are bounded to 1 MiB, 1,002 records (including
header/lookahead), 64,000 cells and 256 columns. Files above 1,000 data rows
initially show that prefix with **Show all**. That explicit action reloads on a
worker with a 32 MiB / 100,001-record / 500,000-cell budget; the same column bound
remains. Files beyond those bounds retain a usable preview with an external-open
notice. Incomplete records at a byte boundary are omitted rather than presented
as malformed source; actual malformed EOF records remain visible. These limits
bound reading and allocation, not only rendering. Columns
scroll horizontally, numbers align right, and text follows its first strong
direction. Long values show an ellipsis and a full-value tooltip. Click selects
a cell; drag or Shift-click extends a rectangle. Copy produces TSV with quoted
tabs, newlines and quotes, or the plain value of a single cell.

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
