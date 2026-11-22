# Independent window UI scale (#2848)

Oleg approved implementation on 2026-11-22 after the research in issue #2848,
comments 62470/62481. `0003-window-ui-scale.diff` adds a generic production
`Window::set_ui_scale` API to the checksum-pinned gpui-pre 0.3.3 core.

The OS display factor remains platform-owned. Rendering uses OS scale × UI
scale; the UI viewport and native window-local input divide by UI scale.
Synthetic `dispatch_event` input remains in UI units, so touch recognition and
accessibility-generated clicks are not converted twice. Pixel scrolling is
converted; line scrolling and dimensionless pinch magnification are unchanged.
Native file/data drops, pointer pressure, touch predictions and gesture positions
use the same boundary. Native IME range/element rectangles multiply by UI scale,
and character lookup points divide by it. Native window bounds and placement
remain in screen/OS coordinates. Client insets, native window-menu anchors,
traffic-light positions and input regions convert outgoing UI coordinates.
Accessibility bounds already multiply by `Window::scale_factor` in `element.rs`.
DPI resize callbacks recompose the factor instead of overwriting the UI scale.

The patch includes a GPUI test for viewport/render scale, native pointer and
pixel-scroll conversion, synthetic-event identity, resize and invalid input.
The application supplies its own persistence, limits and controls; none enter GPUI.

## Ownership and upkeep

[Crate owners](https://crates.io/api/v1/crates/gpui-pre/owners) identify Jason Lee
(`huacnlee`) as the snapshot publisher. The [published package metadata](https://docs.rs/crate/gpui-pre/0.3.3/source/Cargo.toml.orig)
identifies `zed-industries/zed` as the source owner (`crates/gpui` and platform
backends). The recommendation remains a generic core boundary rather than four
independent backend adapters. Rebase this patch and rerun mapping tests on each
snapshot upgrade, auditing every new native coordinate API. There are no
Okilum-specific hooks. No upstream Zed issue or PR is authorized or submitted.

## Platform acceptance

X11, Wayland, macOS and Windows share this core conversion. The old spike only
exercised native X11. New native IME, mixed-DPI transitions, popup hitboxes and
runtime multi-window acceptance on each backend must be recorded separately;
source inspection and synthetic tests are not native certification.
