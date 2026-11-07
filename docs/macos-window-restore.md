# macOS window restore

The shell persists AppKit's normal outer frame in logical points and its native
Zoom/Fill (`maximized`) and fullscreen flags separately, paired with the display
UUID. The existing JSON shape remains compatible with older state files.

GPUI 0.3.3's macOS backend returns `Windowed(current_frame)` for zoomed windows.
Its `is_maximized()` compares the current frame size with `NSScreen.visibleFrame`
instead of asking `NSWindow.isZoomed`. That loses native Zoom state when AppKit's
standard frame differs from the visible area. GPUI also initializes windows with
a content rect, while the saved frame is an outer rect. Its initial secondary
screen y conversion does not round-trip the saved screen-local coordinates.

`window_state/macos.rs` is a shell adapter; GPUI core is not patched. It reads the
live NSWindow frame, `isZoomed` and fullscreen state. A frame whose four edges are within 3 logical points of the
visible area also counts as filled, even when `isZoomed` is false (manual resize,
drag-to-fill or macOS Fill tiling). The same normalization applies when opening
legacy JSON with `maximized: false`; the final outer frame is exactly
`visibleFrame`. Half/quarter tiles and ordinary frames remain exact. While zoomed/fullscreen it retains the last
normal frame. After creating the window, it restores that outer frame exactly,
then applies native Zoom/fullscreen. Zoom fills the current `visibleFrame`, so
changed menu bar/Dock insets and display dimensions cannot leave stale margins.
A surviving display UUID is preferred even if its work area changed; a removed
display falls back to the primary display. Normal frames are clamped only when
necessary. Backing pixels and `backingScaleFactor` are never used for geometry.
Fullscreen startup snapshots wait for the native transition before replacing
saved state.

Linux and Windows production save/restore behavior is unchanged. Linux retains
its existing minimum frame size and post-map WM requests; its compositor still
owns placement. Windows continues to use GPUI's `WINDOWPLACEMENT` normal frame
and `WindowBounds::Maximized` restore. This fix does not change either backend.

Regression coverage includes normal versus zoom/fullscreen serialization,
retention of the normal frame, fractional logical coordinates, displays above
and left of the primary, changed visible areas, and a Linux baseline that asserts
unchanged prepared geometry and persisted bytes. Both native macOS check lanes
run the window-state tests with non-empty discovery; `gh-check --dispatch
--macos-sync` compiles/lints the actual AppKit adapter and runs these controls.
Real Mac beta verification additionally needs title-bar Zoom → quit → relaunch,
then unzoom and a normal-frame round trip. Unit tests do not prove that GUI flow.

## The 20-point regression (#2708 / #2717)

The affected beta 29653 (source `ea33160b`) predates #2712. Its restore path
passed a saved full-size outer frame as `WindowBounds::Maximized`. GPUI then
opened that geometry as an AppKit content rect and unconditionally called native
`zoom:` (`gpui-pre-0.3.3/src/window.rs`, `Window::new` →
`gpui-pre-macos-0.3.3/src/window.rs`, `MacWindow::open` / `zoom`). The measured
result on M4 was `(0,30) 1580×850` instead of `(0,30) 1600×870`. AppKit's Zoom
standard frame is not a contract to fill `visibleFrame`. A disposable interpreted
AppKit probe on Oleg's M4 (macOS 27.2, scale 1) reproduced the exact transition:
a hidden full-size window already has `isZoomed=true`; the unconditional
`zoom:nil` toggles it off, returning bottom-left `(0,20) 1580×850`, equivalent
to top-left `(0,30) 1580×850`. `setFrame:visibleFrame` returns `(0,0) 1600×870`
in AppKit coordinates. `maxSize` / `contentMaxSize` remain unbounded, so this is
an unzoom transition, not a maximum-size clamp. Scoped native evidence lives at
`~/work/qa-evidence/okilum-window-fill/` and the shared DropBox screenshot folder.

There is no 20-point restore inset in the shell or these GPUI paths. The #1007
first-window fallback only shrinks/centers a preferred size in the visible area;
the shell duplicate cascade is 28 points and GPUI's unused default cascade is
25 points. Neither subtracts 20 from the saved size. #2712 avoids the repeated
GPUI Zoom and explicitly sets the outer frame to `visibleFrame` after native
Zoom. It already handles non-zoomed Fill within 1 point; this follow-up covers
a few points of edge rounding and legacy non-maximized near-Fill frames.

Do not reinterpret every old 1580×850 frame as filled: without its old maximized
intent it is indistinguishable from an intentionally sized ordinary window.
Regression controls assert both outcomes: saved maximized intent restores
1600×870 exactly, while an ordinary 1580×850 frame stays 1580×850. Additional
controls cover non-zoomed Fill/legacy JSON, 2-point edge rounding, fractional
points, half tiles, and the 4-point boundary. The native Mac check compiles and
runs the same production adapter. Real Mac screenshots and measured frames
are still required separately from these hosted controls.
