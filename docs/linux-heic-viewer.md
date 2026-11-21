# HEIC/HEIF previews (#2984, #2177)

Linux, macOS and Windows open `.heic`/`.heif` in the image viewer with the
platform's own codec, always in a separate bounded process and off the UI
thread. Before any decoder runs, the snapshot must start with an ISO-BMFF
`ftyp` box; other bytes are an ordinary unreadable image. On macOS, other
raster file previews come from a Quick Look thumbnail; HEIC/HEIF skip it, so
an edited file refreshes through this decoder and a broken one shows the
viewer's message (#3064).

| Platform | Decoder | Missing decoder message |
|---|---|---|
| Linux | `heif-convert` bundled in the AppImage, else from PATH | names `heif-convert` (libheif-examples/libheif), or the libheif HEVC plugin when libheif reports `Unsupported codec` |
| macOS | `/usr/bin/sips` (ImageIO) | none; always present |
| Windows | Windows PowerShell 5.1 + WPF `BitmapDecoder` (WIC) | names Microsoft's HEIF Image Extensions and HEVC Video Extensions |

The viewer's fallback text comes from the last decode that reached a decoder:
a missing codec is a property of the system, and the next successful or
ordinary failed decode clears it.

## Windows (#2177)

Windows has no built-in HEIF codec. With the Store extensions installed, WIC
decodes HEIC for desktop processes. Okilum starts the system
`%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe` with an
`-EncodedCommand` script (not subject to the script execution policy), in the
private temporary directory holding the snapshot. The child is created
suspended, placed in a Job Object (1.5 GiB committed memory per process, 20 s
user CPU, kill on close, no error dialogs) and only then resumed; the 30 s
wall deadline kills it. The script checks the frame size before decoding
pixels (same 16384/64,000,000 limits), applies `System.Photo.Orientation`
and writes `preview.png`; Okilum repeats the PNG checks below.
`WINCODEC_ERR_COMPONENTNOTFOUND`, `WINCODEC_ERR_COMPONENTINITIALIZEFAILURE`
(the inbox HEIF container decoder without the Store codec, as seen on the
hosted `windows-2022` runner), `WINCODEC_ERR_UNKNOWNIMAGEFORMAT` (after the
`ftyp` check) and Media Foundation errors exit with code 3, shown as the
missing-extensions message.

Verified: the hosted `windows-2022` runner (no extensions) pins that message
for a valid HEVC fixture, and the suspended spawn, job and timeout path. Not
verified: real pixels on a Windows host with the extensions installed, and
how Microsoft's codec reports `irot`/`imir` (it is not documented; the
script assumes unrotated pixels plus `System.Photo.Orientation`). Run
`OKILUM_HEIC_EXPECT=decoded` with the extensions installed to check both.

## Linux

Linux opens `.heic`/`.heif` (case insensitive) in the existing read-only image
viewer. Local Markdown embeds use the same asynchronous image asset. The
source remains unchanged; decoding uses a bounded private temporary snapshot
and PNG, removed on success or failure. File notifications evict both the
rendered image and HEIC asset, including cached errors, so replacement,
corruption and restoration update the selected preview.

## Decoder and packaging cost

The AppImage producer and cache seeder install Ubuntu 22.04's
`libheif-examples`. `linuxdeploy` stages `heif-convert`, `prlimit` and their
dynamic dependency closure beside Okilum. This adds the native libheif/HEVC,
PNG/JPEG and codec dependencies to the image; it adds no Rust dependency or
GPUI patch. Ubuntu 22.04 libheif links its HEVC decoder directly, avoiding
host codec/plugin discovery. Bundled sibling executables take precedence
over PATH. The packaged tools have linuxdeploy's relative library paths.

Ubuntu 24.04 hosted test jobs explicitly install `libheif-plugin-libde265`
in addition to `libheif-examples`: their `--no-install-recommends` dependency
installation otherwise omits the HEVC decoder. The real HEVC control caught
this as `Unsupported codec` in initial fast run 43728820827; it remains a
failing decoder result, not a skipped or weakened control. Ubuntu 22.04's
AppImage producer and cache seeder retain their built-in decoder package set.
The [retired reader CI image recipe](linux-ci-image.md) is based on
`act-24.04`, not Ubuntu 22.04; its reference package list also explicitly
includes `libheif-plugin-libde265`. This does not revive its removed workflow
or change an active runner/image.

The build requires bundled `libheif.so.1` and `libde265.so.0`, then executes
real HEVC/pixel/transform controls through the staged binaries. The extracted
artifact's dependency audit includes both executables and repeats those
controls in the Ubuntu/Fedora runtime smoke containers, which install no
HEIF package. The dependency inventory remains `bundled-libraries.txt`;
the exact size depends on the producer's distribution packages. Distribution
copyright/source notices and complete GPL/LGPL texts are included alongside
the separately executed tools and codec libraries. Package updates require
rerunning the same controls; no experimental decoder branch is imported.

Native Linux installs use `heif-convert` and `prlimit` from PATH. Arch's
optional dependencies name libheif and its libde265 plugin. A development
or native-package installation without `heif-convert` names it in the
viewer instead of the generic unreadable-image message (#2177).

## Bounds and failures

- Source: at most 50 MiB, checked before reading and again after a bounded read.
- Linux child: 1 GiB address space, 20 CPU seconds, 256 MiB per output file,
  core dumps disabled. `prlimit` applies these before native parsing/decoding.
- Every decode: 30-second wall deadline; failure/timeout kills and reaps the
  child. Native allocation failures/crashes remain child failures.
- PNG handed to GPUI: at most 256 MiB, positive dimensions no greater than
  16384 per side and 64,000,000 pixels, checked before raster decoding.

Multi-image containers show the primary image (`pitm`). They are not a
photo-album/sequence UI. libheif's converter writes every non-hidden
top-level image in item order, and the primary is not always first: Nokia
C007, C008 and C014 list it last. So the private snapshot gets the `infe`
hidden flag on every other item, one bit each with no size or offset change
(`reader_files/heic/primary.rs`; Python twin `scripts/appimage/heif_boxes.py`).
libheif still uses hidden items as grid tiles, `iden` sources, alpha planes
and thumbnails, and writes only `preview.png` (#2177). If the item list
cannot be read, the bytes are left as they are and the first numbered output
is shown. The CLI may still decode auxiliary images into the private
directory; the process bounds still apply and all outputs are removed with
that directory.
The file-size bound is per output, not a total scratch-disk quota. The OS
address-space cap is a process bound, not proof of a codec preallocation guard.
No source-size, timeout, allocation or codec error becomes a successful preview.

## Evidence scope

The synthetic HEVC fixtures and independent RGB/whole-image transform
references are documented in
[the fixture README](../crates/okilum-core/tests/fixtures/viewers/images/README.md).
`scripts/appimage/heic-control.py` checks real decoded pixels, exact `irot` and
`imir` transforms, first-still selection, unchanged source and corrupt input.
It passed with host libheif 1.21.2 and, separately, Ubuntu 22.04 distribution
libheif in a disposable container. Rust controls exercise file/embed dispatch,
the real decoder, oversize-source refusal, timeout/failure and the native
Reader's visible error/replacement/deletion/restoration lifecycle. Those Rust
controls run on the exact source through hosted checks; installed beta evidence
is separate.

`scripts/appimage/heif-reference.py` compares the viewer path with an
independent pixel reference on the 12 pinned valid files in
`docs/evidence/composer-heif-parity/fixtures.json`. The reference uses no
libheif code: its own container parse plus FFmpeg's HEVC decoder. It includes
the three valid controls the pinned T3 CSP decoder rejects (C013, MIAF007,
clap_cropped). Results, method, thresholds and negative controls:
[heif-viewer-reference](evidence/heif-viewer-reference/README.md). With libheif
1.21.2 and 1.23.7, 11 of 12 match. C006's limited-range alpha is not expanded
by libheif. The AppImage's libheif 1.12.0 matches 9 of 12: no `iden` support
(C008, C014 show the unreadable message), and 10-bit MIAF002 colours are off.

This is bounded product-viewer support, not a whole-HEIF-corpus or T3
pixel-parity certification.
The historical [HEIF reports](evidence/composer-heif-parity/README.md), failed
quality controls and UNKNOWN results are unchanged. In particular, the new
process limits do not claim the historical instrumented libde265 SPS guard or
allocation-marker evidence. #703 certified macOS support only; it did not ship
Linux coverage. A merge/hosted check is not an installed Linux beta walkthrough.
