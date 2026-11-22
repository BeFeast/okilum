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
  file names is announced as `DataEntered` with the pasteboard's types. At
  `performDragOperation:` only the accepted flavors are read and sent as
  `DataSubmit`: every accepted text type, small metadata (`public.url-name`,
  `org.chromium.source-url`, `vscode-editor-data`) and one image: the first
  offered image type, in preference order (PNG before JPEG, GIF, WebP, HEIC,
  TIFF), that can be read within its limit. A browser often adds a far
  larger TIFF of the same image; it is read only when the PNG cannot be.
  Private and dynamic types and file promises are never read; a promise is
  received on request, never by reading the pasteboard.
- Byte limits: text 32 MiB, image 64 MiB, metadata 64 KiB per representation,
  128 MiB for all reads of one drop. A representation over its limit is
  skipped (not truncated) and released at once by its own autorelease pool.
  `ExternalData::types` still lists everything offered; only read flavors
  have contents, and Okilum's `DroppedData` offers only those to the router.
- Unit tests for the type selection and for bounded reads of a real (uniquely
  named) pasteboard: an oversized representation, a second image and a
  private flavor are not copied, the aggregate limit holds, and a PNG over
  its limit falls back to the offered JPEG.
  `scripts/ci/gpui-macos-vendor-tests.sh` requires both to run.

`scripts/patches/gpui-pre-linux/0001-external-data-drops.diff` (Linux):

- Pinned `gpui-pre-linux` 0.3.3 supports both X11 XDND and Wayland
  `wl_data_device`; neither requires a core event change. On enter, only
  public MIME names are selected and `DataEntered` is emitted. Contents are
  requested only on drop, on the background executor.
- UTF-8 (`text/plain`, `text/plain;charset=utf-8`, `UTF8_STRING`), UTF-16
  (`text/plain;charset=utf-16`, `utf-16le`, `utf-16be`), HTML, URI lists and
  PNG/JPEG/GIF/WebP/TIFF are accepted. Unknown/private types are never read.
  Limits match the macOS transport: 32 MiB per text representation, 64 MiB
  per image, 128 MiB per drop. Failed transfers conservatively consume their
  allowance; the first successfully read image prevents reading other images.
  No metadata/private clipboard extensions are requested.
- X11 uses a separate background connection and requestor window for
  `XdndSelection`, including INCR transfers. Property chunks and announced
  lengths are checked against the remaining budget. A four-second deadline
  bounds each representation; `XdndFinished` follows reading, not precedes it.
- Wayland accepts an offered public MIME and negotiates Copy. At drop the
  offer is read through nonblocking pipes with byte limits and a four-second
  wall-clock deadline per representation. `finish`/`destroy` follow reading.
  It uses the ordinary data-device protocol, not privileged data-control.
- URI-list comments are ignored. Local file URLs still produce
  `ExternalPaths` + `Entered`/`Submit`; non-file lists produce `ExternalData`.
  The temporary data drag is cleared before entering the file drag.
- `scripts/ci/gpui-linux-vendor-tests.sh` runs the vendor's own selection,
  aggregate-budget, pipe oversize/timeout, URI classification and real X11
  selection/INCR controls under Xvfb, plus the Okilum MIME routing test.
  `.github/workflows/linux-external-drops.yml` runs these independently of
  the fast gate. X11 INCR covers ordinary, incremental and oversized offers.
  Native interaction with third-party applications on a real Wayland
  compositor remains a manual integration check; the pinned crate has no
  display-server API limitation preventing this implementation.

`scripts/patches/gpui-pre-windows/0002-external-data-drops.diff` (Windows):

- OLE `IDataObject` offers are queried for known `TYMED_HGLOBAL` formats.
  Nonempty `CF_HDROP` paths keep the existing file route; empty/missing paths
  fall back to `DataEntered`. Only type availability is queried on enter;
  contents are copied at `Drop` and delivered as `DataSubmit`.
- Accepts `CF_UNICODETEXT`, registered UTF-8 text, `HTML Format` (CF_HTML),
  `UniformResourceLocatorW`/`UniformResourceLocator`, PNG/JFIF/JPEG/GIF/WebP/TIFF,
  `CF_TIFF`, `CF_DIBV5` and `CF_DIB`. Private formats and promised `FileContents`
  are not requested. One successful image prevents reading further images.
- `GlobalSize` is checked before `GlobalLock` and copying: 32 MiB per text,
  64 MiB per image, 128 MiB aggregate. DIB conversion reserves the extra
  14-byte BMP header within both limits and validates pixel/palette/mask layout.
  `ReleaseStgMedium` runs on all return paths; locks are released before medium
  release. OLE delayed rendering remains synchronous on the apartment owning
  the drop; byte bounds do not impose a deadline on an unresponsive provider.
- `DroppedData` maps Windows names into the shared router's flavors, decodes
  UTF-16, strips NUL termination, and extracts the CF_HTML fragment and source
  URL with the same parser used for clipboard paste. Malformed HTML falls back
  to the offered plain text. DIB is delivered as BMP image data.
- `.github/workflows/windows-external-drops.yml` compiles the pinned Windows
  backend and runs real shell `IDataObject`/`HGLOBAL` controls (including empty
  and nonempty HDROP), image selection/budget/DIB controls, and Okilum router
  and CF_HTML regression tests. Each test must appear as executed in the log.
  Third-party application drags on an interactive Windows desktop remain a
  manual integration check.

## Okilum side

`note_paste::DroppedData` exposes the dropped `ExternalData` through the same
`offer::Pasteboard` trait as the clipboard, so a drop goes through
`note_paste::router::route` exactly like Cmd+V: plain text as it is, HTML and
RTF converted to Markdown (remote images downloaded into attachments
afterwards), image data saved as an attachment. In the editor the drop lands
at the pointer; on the rendered note it switches to Edit and goes in before the
block under the pointer.

Not covered yet: file promises without image data or HTML (some apps offer an
image only as a promised file). Linux data drops use native MIME identifiers mapped
by `DroppedData` to the router's existing flavor names; UTF-16 byte order and
URI-list comments are decoded at that boundary.

## Upkeep

The archives, patches and resulting trees are checksum-pinned in
`scripts/gpui-pre/manifest.json`, `scripts/gpui-pre-macos/manifest.json`,
`scripts/gpui-pre-linux/manifest.json`, and `scripts/gpui-pre-windows/manifest.json`.
On a `gpui-pre*` bump the affected diffs must be rebased and the digests of each
manifest refreshed; a changed context fails patch application instead of
silently changing behaviour.

Windows upkeep preserves the separately pinned DirectX atlas patch unchanged.
The Windows manifest additionally pins the external-drop diff; the setup allows
only `src/external_drop.rs`, its module declaration, and `src/window.rs` alongside
that existing atlas patch. Regeneration verifies this complete changed-file set.

## Upstream plan

The source owner is [zed-industries/zed](https://github.com/zed-industries/zed):
`crates/gpui/src/interactive.rs` and `crates/gpui/src/window.rs` for the event,
`crates/gpui_macos/src/window.rs` for the macOS backend, and
`crates/gpui_linux/src/linux/{x11,wayland}/client.rs` for Linux. The Linux
[package metadata](https://docs.rs/crate/gpui-pre-linux/0.3.3/source/Cargo.toml.orig)
also names Zed and snapshot revision `5b055fa`. The Linux patch is general:
public MIME selection, bounded asynchronous transport and existing GPUI events,
with no Okilum hooks. Rebase it on each Linux snapshot bump and submit it as a
separate Linux follow-up after the core events; no upstream submission is
authorized by this downstream work. The pinned snapshot's
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

Windows source belongs to Zed's `crates/gpui_windows` (not the snapshot publisher);
the pinned [Windows package metadata](https://docs.rs/crate/gpui-pre-windows/0.3.3/source/Cargo.toml.orig)
names `zed-industries/zed`. The Windows follow-up is general OLE format handling
and uses the existing GPUI data events without Okilum hooks. Rebase on each
snapshot bump, propose the bounded reader and event wiring as a separate upstream
Windows change after the core events, and retire the downstream diff once included
in a published snapshot. No upstream issue or PR is opened by this work.
