# Markdown table editing (#1624)

Live Preview keeps the existing persistent table grid and source-backed grouped
Undo/Redo (#2301, #2345), including the table popup selection formats (#2306).
The source navigation and structural model originally published in #2269 is
already in that baseline; this change completes owner comment 43943.

A committed edit in a Live Preview table arms a 750 ms idle formatter. It aligns
source pipes and delimiter cells with Unicode display widths (including CJK,
combining marks and emoji sequences). It formats only the table containing the
edit. Every later buffer mutation cancels the pending task; background results
must match both the source revision and exact bytes before they can be applied.
Opening a note, clicking a cell, moving the caret and Undo/Redo do not arm it.

Formatting is one atomic source Undo transaction, separate from the existing
cell typing group. The native cell inputs remain mounted, with their caret,
selection and authored spaces intact. In an explicitly revealed source table,
the source caret keeps its offset in the same cell. Surrounding text and tables
without edits retain their exact bytes. LF/CRLF and escaped cell source are
preserved. Preedit and IME settling block formatting and structural commands.

- Tab and Shift+Tab select adjacent cells and skip the delimiter row. Tab from
  the final cell appends a data row, selecting its first cell.
- Cmd/Ctrl+Enter inserts a row below the current row.
- Alt+Up and Alt+Down move a data row, retaining its exact cell bytes and the
  file's newline convention. The header and delimiter stay fixed.
- The Table submenu in both source and cell context menus inserts/deletes rows
  and columns, sets column alignment, and explicitly formats the table. The
  existing grid controls remain available.

Commands keep the existing admission limits: current top-level regular GFM
blocks, at most 64 KiB per table and 128 columns; a grid has at most 2048 inputs.
Ragged rows, mixed newline conventions, stale revisions, pending uncommitted
cell values, deletion of the header or final column, and container/literal
lookalikes are refused. Exact Source editing remains available. All accepted
edits use the existing safe-save and recovery pipeline; no new file writer or
recovery store is introduced.
