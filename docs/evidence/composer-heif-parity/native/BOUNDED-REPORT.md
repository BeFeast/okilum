# CSP-bounded QA result — NOT ACCEPTED

This report is separate from immutable stock/CSP measurements and root GATE. Original native report/manifests unchanged.

## SPS guard and controls

The separate QA libde265 source adds `uint64_t(width) * uint64_t(height) > UINT64_C(64000000)` immediately after existing dimension checks in `seq_parameter_set::read`, before conformance-window parsing or derived dimensions. Error is normal `DE265_ERROR_CODED_PARAMETER_OUT_OF_RANGE` (8). Width/height individual limits remain upstream 65535. `csp-bounded.patch` is guard-only; `csp-bounded-instrumented.patch` is the actually measured guard plus diagnostic markers before derived dimensions and frame allocation.

Official C002 SPS is parsed and its unsigned Exp-Golomb width/height fields rewritten, preserving remaining RBSP with emulation-prevention re-escaping. Direct parser results: original1280×720 PASS/error0; exact64M8000×8000 PASS/error0; wide64M64000×1000 PASS/error0; oversized8001×8000 PASS/error8. Oversized parser has zero derived/frame markers. Normal libde265 API confirms original SPS error0 and oversized SPS error8/more0. These direct tests do not alone prove the allocation positive control.

Full HEIF controls preserve C002 hvcC configuration and1280×720 ispe. Insert length-prefixed SPS+PPS inside sole mdat image extent, updating iloc extent length and mdat large size. Matching PPS is necessary: a valid replacement SPS invalidates old PPS upstream. Initial exploratory duplicate-SPS-only control failed to decode and was corrected before acceptance comparison.

Positive full HEIF valid SPS+PPS decodes1280×720 and increments derived and frame markers. Oversized full HEIF replacement SPS never reaches its derived/frame marker: only original1280×720 configuration geometry and1280×720 frame appear. Original and oversized full HEIF RGBA SHA256 both equal unchanged C002 CSP reference: fdde142de119c50bdc43fefa88e0fc6d111772d6e18fd902dae88933ad76af68.

## Pixel and finite malformed gate

Unchanged valid corpus:9PASS(all RGBA byte-exact),3UNKNOWN(reference did not decode). Existing8finite malformed/source/ispe controls all error/no abort/no output. However, NEW oversized in-band SPS full HEIF is incorrectly reported as a successful decode using preexisting valid configuration. Therefore the full malformed-input error gate FAILS. No adoption.

## Source allocation review and limits

Decoder SPS installation occurs only in decctx.cc read_sps_NAL after successful seq_parameter_set::read. Failed SPS is never installed. dpb image allocation obtains dimensions from installed SPS; image copy and SAO allocations derive dimensions from such images (SAO disabled in CSP). The only set_resolution caller is encoder/encoder-context.cc, outside decoder driver. Decoder frame allocations go through image.cc alloc_image; no second decoder SPS parser/installation path was found. Earlier allocations include NAL buffers, fixed SPS/PPS structures and slice contexts; marker counts only cover coded-dimension-derived geometry/frame paths, not all heap allocations. Input size remains capped50MiB by unchanged driver. This is a per-picture coded-area guard, not a total memory budget or proof over arbitrary malformed bitstreams/aggregate reference frames. Three reference-unknown transform fixtures remain UNKNOWN.

libheif plugin decoder_libde265.cc libde265_v1_decode_next_image2 initializes heif err=Ok, breaks on de265_decode error and returns err unchanged; no warning is attached. Upper decode logic can call again and decode a valid retained SPS. This explains full-file success despite direct libde265 range error.

## CSP-bounded-strict

Separately preregistered BOUNDED-STRICT-EXPERIMENT.md enables supported strict_decoding=1 and rejects all returned decoding warnings before output write. Positive and oversized full HEIF both decode, warnings0. Strict/warning API does not expose this error. Full corpus results recorded separately. No extra upstream propagation patch applied.

Builds isolated on baldr -j4; linked existing immutable CSP libheif static archive against new bounded libde265 static archive. Product files/dependencies/PRs/native GUI unchanged.
