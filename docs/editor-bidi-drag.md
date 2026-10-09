# Mouse drag across a bidi run boundary (#879)

A line whose first strong character is Hebrew is an RTL paragraph. In
`"שלום, world",12,"change-me"` the Latin run is painted left to right inside it,
so the end of `change-me` and the trailing edge of the RTL space before `world`
share one X. That X has two source offsets: 31 (after `e`) and 11 (before `w`).

The plain hit test owns the cell under the pointer. A drag that ends a pixel
inside the space resolved to offset 11. From the anchor before `change-me` that
selected the logical range `world",12,"`; the highlight and the copied text
followed that range, not the pointer.

Rule: when the edge nearest the pointer carries more than one offset, the moving
end of a selection takes the offset whose own cell lies between it and the fixed
end, nearest that end. The highlight then reaches the pointer. A press on such an
edge has no direction yet, so its candidates (source offsets with their cells) are
kept until the drag moves, and the same rule picks the anchor. Shift-click uses
the moving-end rule. Plain clicks, double/triple clicks, keyboard movement and
`Geometry::selection` are unchanged.

Selection stays logical. Dragging from inside `change-me` into the Hebrew run
selects bytes on both sides of `world` because they are logically between; copy
still equals the highlight.

## Regression evidence

`scripts/check-bidi-drag-native.py` drives `native_bidi737` configured like the
Reader Source editor (`TESSERA_BIDI_APP`: markdown highlighter, exact-source
projection, document newlines) with Cascadia Code 13 and the BOM + CRLF fixture,
in light and dark. Positions come from caret readouts, not fixed pixels.

- Positive control: a plain click one pixel inside the RTL space reports offset 11,
  so the pointer is on the ambiguous edge. Without the fix the forward drag
  copies `world",12,"` and the script fails there.
- Forward and reverse drags over `change-me` copy `change-me`.
- A drag from that edge into the Hebrew run copies only Hebrew and `, `.
- A drag across the whole Latin run copies `world",12,"change-me"`.
- Every drag checks the X11 clipboard against the selected source bytes.

The isolated example passed on both main and the QA baseline when its drags ended
inside the `e` cell; only the boundary pixel reproduces the Reader failure.
