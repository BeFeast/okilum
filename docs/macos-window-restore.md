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
live NSWindow frame, `isZoomed` and fullscreen state. A frame already covering the
visible area also counts as filled. While zoomed/fullscreen it retains the last
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
