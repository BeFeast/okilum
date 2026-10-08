# Wrapped bidi row boundaries (#780)

Home and End resolve the logical beginning/end of the **painted visual row**.
On an RTL row the logical beginning can be at the right edge. Ctrl+Home and
Ctrl+End remain document navigation. Repeated Home/End stay on that row.

The last layout must have the current source stamp. Resolve the source caret
through that layout's projection, locate its shaped row using both wrap and bidi
affinity, and map the row boundary back through the same projection. Home uses
Before affinity (the first grapheme); End uses After affinity (the last grapheme)
and end-of-wrap affinity. Source mapping includes concealed boundary markers:
Home uses left conceal bias; End uses right conceal bias. A missing/stale layout retains the existing fallback.
No new shaping, bidi selection geometry, or paragraph-direction policy is added.

The original reproduction was measured by typing a character after Home. On the
baseline, Home itself could keep the same screen Y while choosing the row's
logical **end** (a left-edge hit in a Hebrew continuation). Insertion then moved
to the next row. Therefore caret Y alone is not sufficient acceptance evidence.
End also used the separate logical display map, which need not match the painted
wrapped row. Both commands now use the same painted-row lookup.

## Regression evidence

`scripts/check-bidi-row-boundaries-native.py` drives the real shared editor with
Noto Sans 24, mixed English/Hebrew and a Hebrew run split by a wrap. It checks
Source/Live Preview, light/dark, widths 500/800/1100, five rows each:

- Each row's end equals the following row's start in canonical source bytes.
- Home/End and repeated presses keep the same visual row.
- Insertion at Home modifies the exact source boundary. Unlike navigation, an
  edit can legitimately change wrapping (e.g. Latin inserted before Hebrew).
- Undo restores the entire source byte-for-byte.

Run the same script against the baseline binary as a positive control. The
native example records caret and scroll coordinates as well as source offsets.
This is shared-editor X11 evidence; Reader/Wayland acceptance remains a separate
gate and must not be inferred from the fixture.
