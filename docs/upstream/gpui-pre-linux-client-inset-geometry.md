# Wayland window geometry after a client inset change (#3261)

Approved by Oleg on 2026-11-24 (#3261, question «Новый патч gpui-pre-linux»).
Found while investigating #3244.

## Problem

With client-side decorations, a Wayland surface includes the shadow area
around the window (the client inset). The xdg window geometry tells the
compositor which part of the surface is the window itself: the surface minus
the inset. GPUI sends that geometry on every configure and resize.

`set_client_inset` (`gpui-pre-linux` 0.3.3 `src/linux/wayland/window.rs`)
stores the new inset and updates the opaque region, but sends no new
geometry. When the inset changes on a floating window with nothing else
changing, the compositor keeps the geometry computed with the old inset until
the next resize. Okilum scales the inset with its UI zoom (20 px × zoom), so a
restored 200% zoom followed by Ctrl+Shift+0 changed the inset from 40 to 20 px
and GNOME kept a 1420×920 window instead of 1460×960: content shifted 20 px up
and left, and resize edges, snapping and the outline used the wrong rectangle.
Maximized and tiled windows are not affected in practice because the
compositor reconfigures them.

## The patch

`scripts/patches/gpui-pre-linux/0002-wayland-client-inset-window-geometry.diff`:
when the inset changes on a configured window with client decorations,
`set_client_inset` sends `xdg_surface.set_window_geometry` computed exactly as
the configure and resize paths do (`inset_by_tiling` of the current bounds).
Before the first configure nothing is sent, because the configure handler
sends the geometry itself. The request is double-buffered and takes effect
with the redraw commit that `set_client_inset` already requests. X11, server
decorations, layer-shell surfaces, macOS and Windows are unchanged; the crate
is a dependency of `gpui_platform` only on Linux and FreeBSD.

## Verification

`acceptance/zoom-inset-floating.yaml` runs a floating 800×500 window with
client-side decorations (sway `border csd`; Okilum requests server
decorations, which GNOME lacks) on a 1.3-scale headless output, restores a
200% zoom and presses Ctrl+Shift+0. The surface keeps its size, so the window
sway places must grow by 2 × 20 px per axis. On the base commit it stays the
same (+0×0) and the content sits 20 logical px up-left of the window sway
placed, the GNOME symptom; with the patch it grows by 40×40 and the content
starts at the placed rectangle. `scripts/wayland_rig.py` gained the
`output_scale` and `floating` options and a `window()` query, exposed to
scenarios as the `rig:` keys and the step `window:` assertion.

## Ownership and upstream plan

The [package metadata](https://docs.rs/crate/gpui-pre-linux/0.3.3/source/Cargo.toml.orig)
identifies the crate as Zed's `gpui_linux` (snapshot of zed@5b055fa,
repository [zed-industries/zed](https://github.com/zed-industries/zed)); the
`gpui-pre*` snapshots are published by Jason Lee (`huacnlee`). The code owner
is therefore Zed, file `crates/gpui_linux/src/linux/wayland/window.rs`. On
2026-11-23 Zed `main` still has the same `set_client_inset` without a geometry
update. The change is generic (any GPUI app whose CSD inset changes at
runtime) and small enough to submit as a Zed PR, together with the reproduction
above. Submission needs Oleg's separate approval; no upstream issue or PR has
been opened. On each snapshot upgrade, drop the patch if upstream has fixed it,
otherwise rebase it and rerun the scenario.
