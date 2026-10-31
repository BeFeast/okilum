# Resource-path source review — no full PASS claim

Sources: exact libheif1.22.2 commit763bc8bb87788d64d39ece623ceed988de12dc5b and libde2651.0.16 commit7ba65889d3d6d8a0d99b5360b028243ba843be3a.

## What the driver actually configures

`heif_context_get_security_limits` → copy defaults → set `max_image_size_pixels=64000000` → `heif_context_set_security_limits`, before `heif_context_read_from_memory_without_copy` and every decode. It leaves other finite defaults enabled. Source metadata50MiB check precedes compressed input allocation. First transformed handle width/height are checked before RGBA output.

## Config/metadata path

- libheif `image-items/grid.cc:277,352`: grid canvas and grid input dimensions checked against the context limits.
- `image-items/image_item.cc:1229`: per-image limits tightened using ispe dimensions and codec coding-unit padding.
- `codecs/decoder.cc:372`: `get_coded_image_size_from_config` → check actual SPS coded geometry before creating/pushing to the decoder plugin.
- `codecs/hevc_dec.cc:54`: parses SPS from hvcC configuration to return uncropped coded dimensions; it does not trust only display/ispe dimensions.
- Alpha/tiles follow the same image-item decoding path. This supports the finite metadata/config positive controls but is not proof about every future bitstream path.

## Remaining gap

`codecs/decoder.cc:370` explicitly says the plugin also needs the check because SPS packets may occur in the actual image bitstream. The libde265 plugin reports API5 and does not use `options->limits` to configure an allocation guard. Its `de265_decode` call precedes `convert_libde265_image_to_heif_image` / `heif_image_add_plane_safe` limit checks.

libde265 `sps.cc:238–250` reads picture width/height, rejects zero and dimensions above65535 individually, then proceeds to conformance/chroma/derived state; it has no64000000-area policy. `image.cc:236` `de265_image::alloc_image` eventually allocates coded image buffers without the application's area cap. Thus context security API alone does not prove the required64M-before-every-coded-allocation property if an in-band SPS replaces trusted configuration.

## Narrow proposed follow-up — not implemented in this experiment

Put the area policy immediately after the existing positive width/height/max-dimension checks in `seq_parameter_set::read` (`sps.cc`), before derived CTB/sample maps and frame buffers. Use64-bit checked multiplication (or division), preserving valid wide/tall images whose area is<=64000000; do not substitute an arbitrary8192-width cap. Return `DE265_ERROR_CODED_PARAMETER_OUT_OF_RANGE` through the normal decoder error path. A redundant first-entry guard in `de265_image::alloc_image` may protect internal calls, but alone is later than SPS-derived metadata allocation.

Tests needed before acceptance: exact64M area with wide/tall valid geometries; over64M and overflow/zero; in-band SPS replacement after a small hvcC/ispe configuration; grid tile and auxiliary alpha cases; evidence that rejection occurs before image/derived-map allocations. Then repeat unchanged raw-RGBA controls to prove the guard/profile does not alter valid outputs.

The plugin's current decode error handling can return no image after a libde265 error; `Decoder::decode_single_frame_from_compressed_data` bounds retries50 and returns an explicit “did not give a decompressed image” error. Any policy guard integration should preserve/refine this error mapping rather than aborting/panicking or treating a null output as success.
