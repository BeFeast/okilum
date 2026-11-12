# DirectX atlas upload (gpui-pre-windows 0.3.3)

Oleg approved this narrow exception on **2026-11-12 at 09:05 Israel**:
a separate, independently reviewed PR may change DirectX atlas upload in
`gpui-pre-windows`. GPUI core, editor, shaping, timing thresholds and the
Windows stack reserve stay unchanged. See the exception in [AGENTS.md](../../AGENTS.md).

`UpdateSubresource` into an atlas used by an earlier GPU frame can block the
UI thread while inserting a previously unseen glyph. The
[upstream report](https://github.com/zed-industries/zed/issues/56349) describes
full-atlas copies and the staging-copy approach. Okilum's VM150 experiment
confirmed long label-paint spans with legacy uploads and no spans at the
5 ms observation threshold with staging uploads in the same executable.
The implementation does not change glyph generation or cache keys.

For each new tile, create a matching small staging texture, map it for writing,
copy each source row using the mapped `RowPitch`, unmap, then queue
`CopySubresourceRegion` into the original atlas at its allocated offset.
All three atlas formats remain intact. Upload failure returns an error and
releases the uncommitted tile; it must never cache missing pixels as a success.

`scripts/gpui-pre-windows-setup.py` follows the existing checksum-pinned vendor
setup mechanism. It reads the exact published crate archive, applies
`scripts/patches/gpui-pre-windows/0001-directx-atlas-upload.diff`, and verifies
the complete generated tree. It rejects changes outside `src/directx_atlas.rs`.
The separate patch namespace is intentionally excluded from the gpui-kit
patch glob. Cargo uses the generated backend through a path patch; no registry
source is mutated and `gpui-pre` stays on the same pristine version.

New Windows WARP tests read pixels back from real GPU resources: narrow,
padded rows, nonzero atlas offsets, all formats, retained existing pixels,
and failure/retry with no poisoned cache or leaked allocation. Missing WARP
is a test failure. The original deallocation test stays unchanged.
`scripts/ci/windows-atlas-upload.py` compiles five runtime negative controls:
legacy upload, invalid upload reported as success, unreleased allocation,
packed rather than mapped row pitch, and wrong destination offset. Each must
compile and fire its intended assertion. The script restores source bytes and
rebuilds the positive test artifact before returning.

The existing native Reader runs retain their thresholds and stack reserve.
VM mouse-wheel before/after results, fixture/executable hashes and screenshots
are recorded in the implementation PR; those portable measurements do not
establish installed-beta acceptance.
