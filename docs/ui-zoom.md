# Whole-UI zoom (#2848)

The application UI scale is shared by all windows and saved as `ui_zoom` in the
existing application UI state. It is separate from `text_zoom`, the OS display
scale and native window placement. Existing state without `ui_zoom` opens at
100%; malformed non-finite values normalize to 100%. The range is 50% to 200%.
Each wheel notch or key press moves by the step chosen in Settings → Appearance
→ UI zoom step (`ui_zoom_step`: 2%, 5% or 10%; default 5%, since 10% was
reported as too coarse in #3228) and lands on a whole multiple of it. Unknown or
malformed step values fall back to 5% without rejecting the rest of the state. Saving uses the existing generation-aware, locked atomic state writer and
merges only this changed field, preserving independent settings.

Use Cmd/Ctrl+Shift+wheel, Cmd/Ctrl+Shift+plus/minus/0, or View → Zoom In UI /
Zoom Out UI / Actual UI Size. Cmd/Ctrl+wheel and unshifted document zoom keys
continue to affect note text only. Shift-wheel reported on the horizontal axis
is accepted too. Precision wheel input accumulates 40 OS logical pixels before
one step so trackpad event frequency does not drive the scale immediately to a
limit. Root overlays capture before scrolling editors, dialogs or menus.

Linux (xkb) reports symbol keys with Shift already applied and drops the Shift
modifier: Ctrl+Shift+= arrives as `ctrl-+`, Ctrl+Shift+- as `ctrl-_` and
Ctrl+Shift+0 as `ctrl-)`. Those forms are bound as well (#3229); the shifted
forms stay last so menus keep showing Ctrl+Shift+= / - / 0.

At 150%, a 1440px-wide window at OS scale 1 has a 960 UI-pixel viewport. The
existing 1000 UI-pixel sidebar breakpoint intentionally uses mutually exclusive
overlays here. Crossing into compact mode closes transient overlays and retains
the wide dock preferences; toolbar sidebar controls open either side explicitly.
This behavior is covered by a regression test rather than lowering the breakpoint
and squeezing document content between two docks.

Popup menus default to scrolling and clamp their content to the UI viewport
minus the existing edge spacing. More also reserves room for its toolbar anchor.
The vendor change adds a generic Root presentation-overlay facility, mounted
outside normal content layout, so auxiliary windows get the same wheel behavior.

## Verification boundaries

Unit/UI coverage exercises bounded scales and modifiers, persistence independent
of text zoom, existing/new window synchronization, the compact sidebar transition,
and oversized popup scrolling. The core prerequisite has its own coordinate
mapping tests. Fast agent checks compile/clippy the workspace but do not run Rust
tests; do not claim those tests ran from a fast-gate success alone.

Native acceptance must separately cover X11, Wayland, macOS and Windows: physical
hitboxes at 50/100/150/200%, More and nested menus, CJK IME caret and click lookup,
OS DPI transitions, two windows and a newly opened auxiliary window, and the
independence of document zoom. The preserved spike screenshots cover only the old
X11 proof; they are not acceptance evidence for this implementation.

## Render-scale diagnostics (#3244)

A blur after resetting a restored zoom was reported on macOS and did not
reproduce on Linux. Each window therefore logs a `ui_scale` line to the
diagnostic log when it opens, on its first frame, on every UI-zoom change, and
on bounds callbacks whose scale, display, screen presence or drawable match
changed. Feedback metadata carries the same reading as flat keys: GPUI
`render_scale`, `ui_zoom`, `viewport`, `display` and, on macOS, the screen and
window `backingScaleFactor`, `window_on_screen` (GPUI assumes 2.0 for a window
without a screen), the window frame, the layer `contentsScale` and size, the
`drawableSize` and whether it matches the layer. The code only reads these
values and never changes a scale.
