# Linux HEIC/HEIF previews (#2984)

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
or native-package installation without a decoder reports the existing
unreadable-image message. macOS retains its system `sips` decoder; Windows
HEIC viewing is unchanged.

## Bounds and failures

- Source: at most 50 MiB, checked before reading and again after a bounded read.
- Linux child: 1 GiB address space, 20 CPU seconds, 256 MiB per output file,
  core dumps disabled. `prlimit` applies these before native parsing/decoding.
- Every decode: 30-second wall deadline; failure/timeout kills and reaps the
  child. Native allocation failures/crashes remain child failures.
- PNG handed to GPUI: at most 256 MiB, positive dimensions no greater than
  16384 per side and 64,000,000 pixels, checked before raster decoding.

Multi-image containers show the first top-level still (`preview-1.png`), as
libheif's converter orders them. They are not a photo-album/sequence UI. The
CLI may decode other top-level/auxiliary images into the private directory;
the process bounds still apply and all outputs are removed with that directory.
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

This is bounded Linux product-viewer support, not a whole-HEIF-corpus or T3
pixel-parity certification. [#2177](https://git.oklabs.uk/BeFeast/okilum/issues/2177)
remains open for the broader valid corpus and its independent pixel references.
The historical [HEIF reports](evidence/composer-heif-parity/README.md), failed
quality controls and UNKNOWN results are unchanged. In particular, the new
process limits do not claim the historical instrumented libde265 SPS guard or
allocation-marker evidence. #703 certified macOS support only; it did not ship
Linux coverage. A merge/hosted check is not an installed Linux beta walkthrough.
