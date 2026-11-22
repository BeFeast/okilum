# Application UI scale (#2848)

Oleg chose the window scale-factor override on 2026-11-12 («второй путь наиболее
разумный») and, on 2026-11-22, put the implementation first in the queue.

## Problem

Cmd/Ctrl+wheel zooms note text only. The request is a second zoom, Cmd/Ctrl+Shift+wheel,
that scales the whole interface the way Electron apps do: chrome, text and icons
together, independent of the operating system's display scale and of note text zoom.

`Window::set_scale_factor` is test-only and does not change the viewport or pointer
coordinates. `GPUI_X11_SCALE_FACTOR` is a process-wide startup value.

## The patch

`scripts/patches/gpui-pre/0003-ui-scale.diff`:

- `App::set_ui_scale` stores one zoom for every window. `1.0` is unchanged.
  Non-finite and non-positive values are ignored. The product clamps to 50–200%.
- Painting uses display scale × UI zoom, so glyphs stay sharp.
- The logical viewport and incoming pointer, wheel, touch and drop coordinates
  are divided by the zoom. Pixel scroll distances are divided; line scrolls are not.
- Outgoing native rectangles (resize, window menu, traffic lights, client inset,
  input region, IME bounds, character-index queries) are multiplied back.
  Wayland exclusive-zone sentinels `0` and `-1` are not scaled.
- `Window::scale_factor` remains device pixels per logical pixel. `display_scale`
  is the operating-system scale alone. `Window::bounds` stays in platform coordinates
  so a saved window frame is the real window.

A window unit test resizes a 2× test window, sets zoom 2, and checks the viewport,
device scale, pointer, pixel and line scrolls, and the size passed to the platform.

## Okilum

The zoom is one value for the application, remembered with the other UI state.
Controls: Cmd/Ctrl+Shift+wheel, Cmd/Ctrl+Shift+plus/minus/0, and Zoom Interface
In/Out/Actual Interface Size in the View menu (and the in-window menus on Linux
and Windows). Note text zoom is unchanged. The compact-sidebar breakpoint uses
OS logical width, so a 1440-wide window zoomed to 150% does not become an overlay.
The More menu and the document menu scroll when they are taller than the window.

## Upkeep

The archive, patch and tree are checksum-pinned in `scripts/gpui-pre/manifest.json`.
On a `gpui-pre` bump the diff is rebased and the digests refreshed.

## Upstream plan

The source owner is [zed-industries/zed](https://github.com/zed-industries/zed):
`crates/gpui/src/window.rs`, `crates/gpui/src/app.rs` and `crates/gpui/src/platform.rs`.
The pinned snapshot's package metadata names that repository;
[crates.io owners](https://crates.io/api/v1/crates/gpui-pre/owners) lists Jason Lee
(`huacnlee`) as the snapshot publisher, who does not own the code. Zed
[Universal zoom #21816](https://github.com/zed-industries/zed/issues/21816) is open.
Current Zed main still has a test-only scale setter.

The change is general (no Okilum hook) and suitable to propose as
`Window::set_ui_scale` with the coordinate mapping above and the unit test.
Opening an upstream issue or PR needs Oleg's separate approval; none has been opened.
