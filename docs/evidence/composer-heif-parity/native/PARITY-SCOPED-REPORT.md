# G03 T3-parity scope — backend PASS

Manager's explicit acceptance disposition is T3 parity. This is a new, narrower assessment alongside the immutable [original gate](../GATE.md), [original native report](REPORT.md) and [failclosed report](FAILCLOSED-REPORT.md). Their full-corpus UNKNOWN result remains unchanged. No unsupported feature is accepted by file hash, and no original failure/UNKNOWN is relabelled.

**CSP-bounded-failclosed backend T3-parity scope: PASS.** Nine T3-reference-decodable controls are RGBA byte-exact; the three controls rejected by T3 are also rejected by this backend with matching underlying security-limit diagnostics, without crash, timeout or RGBA output. Supplementary source/coded-pixel/corrupt controls and positively controlled preallocation/error propagation pass.

**Product GUI visible error: NOT TESTED.** This backend result does not claim that an attachment preview displays the error, that send remains blocked, or that a GPUI/native product adapter is complete. Those are subsequent adapter/UI acceptance checks. No product dependency or integration was changed by this experiment.

## Positive decode scope

[Valid corpus results](failclosed-results/valid-csp-bounded-failclosed.json) retain the original quality verdicts. Reference-decodable C002, C006, C007, C008, C014, C042, MIAF002, rainbow451×461 and alpha512×512:9PASS, dimensions/first-image selection/transforms/alpha exact, RGBmean0/max0, every RGBA byte identical. There is no tolerance adjustment. The original quality comparison still records three UNKNOWN rows because a pixel reference does not exist for them.

## Independently rerun error scope

Each of the three was freshly executed through both pinned T3-CSP reference and native failclosed decoder, on the same baldr host/session, with30s per-process deadline and2GiB address-space limit. Every process exited normally with error status/exit1; none crashed, hung, or created RGBA. Native elapsed7–14ms and reference398–737ms; these durations document bounded completion, not a performance acceptance claim.

| Control | Shared underlying T3/native diagnostic | Backend error parity |
|---|---|---|
| C013 | Security limit exceeded: Image size1280×720 exceeds maximum image size132496 | PASS |
| MIAF007 | Security limit exceeded: Image size1280×720 exceeds maximum image size298496 | PASS |
| clap_cropped | Security limit exceeded: Image size256×256 exceeds maximum image size16384 | PASS |

T3 wraps these with generic `Error: HEIF processing error`; its decoder stdout includes the detailed security diagnostic, matching native. These controls are not evidence of broad HEIF feature support. The observed cause is coded-size/security policy against transformed item dimensions, not successful transform decoding. See [fresh error evidence](parity-results/reference-errors.json). Fixture hashes are provenance only; candidate has no hash blacklist or filename-dependent behavior.

## Supplemental negative controls and allocation positive control

[Eight finite malformed/source/metadata controls](failclosed-results/corrupt-resource-csp-bounded-failclosed.json):8PASS, normal errors/exit1/no unexpected RGBA or panic. Source ceiling50MiB is enforced before compressed read; coded-pixel ceiling64,000,000 is checked with unsigned64-bit multiplication before libde265 derived geometry/frame allocation.

[Whole-HEIF in-band controls](failclosed-results/failclosed-inband.json) retain small valid hvcC/ispe1280×720. Original in-band SPS plus matching PPS decodes exact reference RGBA and increments frame-allocation marker1; this is the allocation positive control. Oversized8001×8000 in-band SPS plus PPS returns explicit normal `coded parameter out of range` plugin error/exit1, no RGBA, frame marker0 and zero oversized derived markers. The sole geometry marker belongs to the valid1280×720 configuration read before the rejected in-band SPS. This proves the exercised overcap path is rejected before its dimension-dependent allocations, rather than inferring safety only from missing output.

[Direct SPS/API control evidence](logs/bounded-parser-original.json): original1280×720 and exact64M8000×8000/wide64000×1000 parse normally;8001×8000 returns normal error8 before derived markers, also through normal de265 decoder API. See [allocation/source review](BOUNDED-REPORT.md) and historical [resource-path review](RESOURCE-PATH.md). The guard is per coded-picture area; these finite tests do not claim an aggregate heap/reference-frame budget or exhaustive malformed-bitstream safety.

## Pinned implementation and reviewable patches

- T3 reference: verified heic-to/csp1.5.2, bundled libheif1.22.2/libde2651.0.16, raw RGBA via actual CSP source; first top-level image selection, no JPEG step. [Public package/checksum provenance](../sources.json).
- libheif1.22.2 commit763bc8bb87788d64d39ece623ceed988de12dc5b, archive SHA2560a3559b557793d8d634d071ee7181af34f2b69b0d61cd48a0dcdbfe4ea79fc43.
- libde2651.0.16 commit7ba65889d3d6d8a0d99b5360b028243ba843be3a, archive SHA2569dedf12d5b5047c270d1fc54f5d92fc29d9ea21b94d3f2d998200258353189a0.
- [Decode-only mask registration](decode-only-mask.patch) and [explicit CSP filter profile](csp-profile.patch), preserving actual T3's disabled DEBLOCKING/SAO without defining Emscripten globally.
- [Unsigned64M SPS guard](csp-bounded.patch), plus separately recorded [QA instrumentation](csp-bounded-instrumented.patch).
- [Four-line failclosed plugin propagation](csp-bounded-failclosed.patch): only DE265_ERROR_CODED_PARAMETER_OUT_OF_RANGE returns existing decoder-plugin error with static upstream error text; no normal-flow or valid RGB processing change. Patch SHA256c547ff1e8cf7b09fdf0ff3259633d041f0ea175c1d47b580d1ede5c85ef166cd.

Built on baldr -j4 in isolated QA prefixes, static codecs, exactly1decoder/0encoders, no dynamic codec/plugin closure. [Failclosed preregistration](FAILCLOSED-EXPERIMENT.md) predates measurements. [Build/source manifest](failclosed-source-build-manifest.json) and [fixture provenance](../fixtures.json) remain unchanged. Stock/pure-Rust/earlier bounded profiles retain prior rejection outcomes; only this final profile has the narrower backend parity PASS.

Follow-up requested by manager: **viewer: HEIF beyond T3 decoder** — expand HEIF support independently of parity, investigating transformed-item/coded-size policies and obtaining valid pixel references. Publication/issue creation belongs to the primary agent. No Forgejo action was performed here.
