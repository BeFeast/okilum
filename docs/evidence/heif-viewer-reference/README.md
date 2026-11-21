# Linux HEIF viewer vs an independent pixel reference (#2177)

Checked 2026-11-20 with `scripts/appimage/heif-reference.py` on the 12 pinned
valid files in [`fixtures.json`](../composer-heif-parity/fixtures.json)
(9 Nokia conformance files, 3 libheif test images; SHA-256 checked on every
download). The historical [composer HEIF reports](../composer-heif-parity/README.md)
are unchanged.

## Method

- **Viewer path**: the bytes the Linux viewer hands to libheif (non-primary
  items marked hidden, see below), decoded by `heif-convert` under the
  viewer's `prlimit` bounds. Exactly one `preview.png` must come out.
- **Reference path, no libheif code**: `scripts/appimage/heif_boxes.py` parses
  the container. Each coded HEVC item becomes an Annex B stream (hvcC
  parameter sets + item NAL units), decoded by FFmpeg's own HEVC decoder
  (`-f hevc`, not its HEIF demuxer). Grid composition, `iden`, `clap`, `irot`
  and `imir` are applied in `ipma` order in numpy. `imir` axis 0 exchanges top
  and bottom: libheif's reading, and Nokia's description of C042 ("vertically
  mirrored"). Alpha is the `auxl` item's luma, mapped from its coded range.
- **Thresholds**, fixed before the first comparison: equal dimensions, mean
  absolute RGB delta ≤ 1.5, ≥ 99.9% of RGB samples within 8 levels, alpha
  within 1 level.
- **Negative controls**, on every file: each geometric step perturbed (clap
  shifted 8 px, irot reversed, imir axis swapped, grid tiles rotated) and a
  mirrored viewer image. Every control must fail the same thresholds. All
  64 controls across the three runs (files the viewer decoded) failed, so
  the metric is not blind.

Two reference choices changed after the first run. Both are recorded here:

1. Chroma upsampling: the first run used bilinear. `clap_cropped` then
   differed by mean 6.35 (max 95), and C013/C014 had 99.90% of samples
   within 8 levels. HEIF does not define the upsampling filter. libheif
   uses nearest-neighbour, and with nearest-neighbour `clap_cropped` matches
   with mean 0.027, max 1. The reference now uses nearest-neighbour.
2. Alpha range: the first version used stored alpha samples. C006's alpha
   picture is limited range (VUI `video_full_range_flag` 0, samples
   16..235, no alpha SEI). Taking it as stored would never reach full
   transparency or full opacity. The reference now maps 16..235 to 0..255,
   like any limited-range luma; libheif 1.12 does the same. This change
   *creates* the C006 failure for libheif ≥ 1.21 below. It does not hide one.

## Results

| File | Content | libheif 1.21.2 (host) | libheif 1.23.7 (source build) | libheif 1.12.0 (Ubuntu 22.04 = AppImage) |
|---|---|---|---|---|
| C002 | single image | PASS 0.54 / max 1 | PASS | PASS |
| C006 | alpha (limited range) | **FAIL alpha Δ20** | **FAIL alpha Δ20** | PASS (alpha Δ1) |
| C007 | 2×2 grid, primary last | PASS 0.59 / 1 | PASS | PASS |
| C008 | `iden` + irot, primary last | PASS 0.55 / 1 | PASS | **FAIL** `iden` unsupported (viewer error) |
| C013 | two clap'd stills | PASS 0.55 / 1 | PASS | PASS |
| C014 | `iden` + clap + irot, primary last | PASS 0.53 / 1 | PASS | **FAIL** `iden` unsupported (viewer error) |
| C042 | imir | PASS 0.54 / 1 | PASS | PASS |
| MIAF002 | 10-bit, thumbnail | PASS 0.49 / 2 | PASS | **FAIL** mean 14.0 (10-bit) |
| MIAF007 | clap + irot + imir | PASS 0.54 / 1 | PASS | PASS |
| rainbow 451×461 | odd-size clap | PASS 0.02 / 1 | PASS | PASS |
| with-alpha 512×512 | alpha (full range) | PASS 0.00 / 1, alpha 0 | PASS | PASS |
| clap_cropped | centred clap | PASS 0.03 / 1 | PASS | PASS |

Figures are mean / max absolute RGB delta. Raw rows:
[1.21.2](libheif-1.21.2-host.json), [1.23.7](libheif-1.23.7-source.json),
[1.12.0](libheif-1.12.0-ubuntu2204.json) ([packages](libheif-1.12.0-ubuntu2204-packages.txt)).
The reference results are identical under FFmpeg 8.0.1 (host), 6.1.1
(Ubuntu 24.04) and 4.4.2 (Ubuntu 22.04).

C013, MIAF007 and clap_cropped are the three valid controls the pinned T3 CSP
decoder rejected. All three now have independent pixel references, and the
viewer matches them on every libheif version tested.

## Fixed in the viewer

Before this change, the Linux viewer showed the wrong picture for C007, C008
and C014. libheif's converter writes every non-hidden top-level image as
`preview-N.png` in item order, and the viewer showed `preview-1.png`. In
these three files the primary item is last: the viewer showed a grid tile or
an `iden` input instead of the primary. The viewer now sets the `infe`
hidden flag on every non-primary item in its private snapshot. This is one
bit per item; no size or offset changes. libheif still uses hidden items as
tiles, `iden` sources, alpha planes and thumbnails, but writes only the
primary. On libheif 1.21.2 the output is pixel-identical (ImageMagick AE 0)
to the primary from the old multi-output run, for C007, C008, C013 and C014.
libheif 1.12.0 honours the flag in the same way. That run is the
`primary-second.heic` control in `heic-control.py`, which passed in an
Ubuntu 22.04 container.

## Not resolved here

- **C006 alpha, libheif ≥ 1.21** (host distributions; 1.23.7 is the latest
  release): libheif passes the limited-range alpha samples through unscaled.
  Opaque pixels come out at 235/255 opacity and transparent ones at 16/255.
  No upstream libheif issue covers this (searched 2026-11-20). Okilum cannot
  correct it after decoding without knowing which libheif behaviour it got.
- **AppImage (libheif 1.12.0)**: no `iden` support. C008 and C014 show the
  viewer's ordinary "couldn't be read" message; no crash or hang. 10-bit
  MIAF002 decodes with large colour errors. A newer bundled libheif would
  fix these, but would also bring the C006 alpha behaviour above.
- Real pixels on macOS (`sips`) and Windows (WIC) were not part of this
  comparison.
