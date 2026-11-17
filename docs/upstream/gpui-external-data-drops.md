# External drops of text, HTML, RTF, URLs and images (#1615)

Approved by Oleg on 2026-11-17 (#1615, Q1 «Да, сначала macOS»).

## Problem

GPUI accepts only file drags from other applications. On macOS the window
registers just `NSFilenamesPboardType` (`gpui-pre-macos` 0.3.3
`src/window.rs`, `registerForDraggedTypes`), and `FileDropEvent` carries only
`ExternalPaths` (`gpui-pre` 0.3.3 `src/interactive.rs`). A selection dragged
from a browser, Mail, Notes or an IDE, and an image dragged out of a web page,
never reach the application. The Linux (X11/Wayland) and Windows backends emit
the same paths-only event.

## The patches

`scripts/patches/gpui-pre/0002-external-data-drops.diff` (core):

- `ExternalData`: a drag's typed representations, keyed by the platform's own
  type identifiers (uniform type identifiers on macOS). While dragging it holds
  the types only; at the drop, the contents that could be read.
- `FileDropEvent::DataEntered { position, data }` and
  `FileDropEvent::DataSubmit { position, data }`. The window turns them into the
  same internal drag as a file drop, so `on_drop::<ExternalData>` and
  `drag_over::<ExternalData>` work like their `ExternalPaths` counterparts.
  `DataSubmit` replaces the drag's value with the dropped contents.
- A window unit test drives both events into an `on_drop::<ExternalData>`
  target.

`scripts/patches/gpui-pre-macos/0003-external-data-drops.diff` (macOS):

- The window also registers plain text (UTF-8, UTF-16), HTML, RTF, RTFD, URL
  and common image types (PNG, JPEG, TIFF, GIF, WebP, HEIC).
- A drag with file names is unchanged (`Entered`/`Submit`). A drag without
  file names is announced as `DataEntered` with the pasteboard's types; at
  `performDragOperation:` the contents of every type are read and sent as
  `DataSubmit`. Dynamic (`dyn.*`) types and file promises are not read: a
  promise is received on request, never by reading the pasteboard.
- Unit tests for the type rules and for reading a real (uniquely named)
  pasteboard; `scripts/ci/gpui-macos-vendor-tests.sh` requires them to run.

Backends that never emit the new variants (Linux, Windows) behave as before.

## Okilum side

`note_paste::DroppedData` exposes the dropped `ExternalData` through the same
`offer::Pasteboard` trait as the clipboard, so a drop goes through
`note_paste::router::route` exactly like Cmd+V: plain text as it is, HTML and
RTF converted to Markdown (remote images downloaded into attachments
afterwards), image data saved as an attachment. In the editor the drop lands
at the pointer; on the rendered note it switches to Edit and goes in before the
block under the pointer.

Not covered yet: file promises without image data or HTML (some apps offer an
image only as a promised file), and the Linux and Windows backends. Both are
follow-ups; the Linux one can reuse the #1618 MIME reader once GPUI hands over
the XDND / `wl_data_offer` contents.

## Upkeep

The archives, patches and resulting trees are checksum-pinned in
`scripts/gpui-pre/manifest.json` and `scripts/gpui-pre-macos/manifest.json`.
On a `gpui-pre*` bump both diffs must be rebased and the three digests of each
manifest refreshed; a changed context fails patch application instead of
silently changing behaviour.

## Upstream plan

The source owner is [zed-industries/zed](https://github.com/zed-industries/zed):
`crates/gpui/src/interactive.rs` and `crates/gpui/src/window.rs` for the event,
`crates/gpui_macos/src/window.rs` for the macOS backend. The pinned snapshot's
[package metadata](https://docs.rs/crate/gpui-pre/0.3.3/source/Cargo.toml.orig)
names that repository; [crates.io owners](https://crates.io/api/v1/crates/gpui-pre/owners)
lists Jason Lee (`huacnlee`) as the snapshot publisher, who does not own the
code. On 2026-11-17 Zed `main` still registers only `NSFilenamesPboardType`
(now through `objc2`, `NSArray::from_slice`) and `FileDropEvent` still carries
only paths.

The change is general (no Okilum hook) and suitable for upstream:

1. Port the macOS part to the `objc2` API Zed now uses.
2. Upstream may prefer opt-in registration of data types (every window now
   shows a copy cursor for a text drag, even where nothing takes it); offer a
   window option if asked.
3. Submit the core event and the macOS backend together, then Linux and
   Windows as separate PRs.
4. Drop the downstream diffs once a published snapshot includes the change.

Opening an upstream issue or PR needs Oleg's separate approval; none has been
opened.
