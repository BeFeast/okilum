# CSP-bounded-failclosed QA result

All exercised supported-input and guard controls PASS. Overall complete-corpus adoption remains UNKNOWN: three reference fixtures still cannot be decoded, so their quality gate cannot be evaluated. No product adoption/integration performed. Stock/CSP/bounded/strict profiles and reports remain unchanged and rejected as previously recorded.

## Exact profile

Pinned libheif1.22.2 commit763bc8bb87788d64d39ece623ceed988de12dc5b and libde2651.0.16 commit7ba65889d3d6d8a0d99b5360b028243ba843be3a. Existing CSP filter define, decode-only mask-registration exclusion, and instrumented64,000,000 coded-pixel SPS guard retained. This profile adds exactly4lines in libheif libde265_v1_decode_next_image2 immediately after de265_decode: only DE265_ERROR_CODED_PARAMETER_OUT_OF_RANGE returns existing heif_error_Decoder_plugin_error/heif_suberror_Unspecified with de265_get_error_text. Upstream error text is a static string literal. No WAITING_FOR_INPUT_DATA or other flow/error treatment, valid RGB conversion, driver or acceptance threshold changes.

Preregistration FAILCLOSED-EXPERIMENT.md SHA256 fec477b91f915740063f012280413fb4072631235c65f08b53aa73d1e618d6bb. Minimal propagation patch csp-bounded-failclosed.patch SHA256 c547ff1e8cf7b09fdf0ff3259633d041f0ea175c1d47b580d1ede5c85ef166cd. Measured plugin source SHA2565959915f744cded22bfe8cae334fa7281224a24d8ac463630e32d61b8cb70f99.

## Measured controls

- Unchanged official corpus:9PASS with every RGBA byte identical to CSP reference (RGBmean0/max0, alpha mismatches0).3UNKNOWN remain because reference failed: C013, MIAF007, clap_cropped. No tolerance relaxation.
- Existing8finite malformed/source/ispe controls:8PASS, error/exit1/no panic/no RGBA.
- Whole-HEIF positive: C002 original1280×720 hvcC/ispe with matching original SPS+PPS inserted in-band: exit0, decoded1280×720, derived markers2, frame marker1. RGBA SHA256 fdde142de119c50bdc43fefa88e0fc6d111772d6e18fd902dae88933ad76af68 equals unchanged reference.
- Whole-HEIF negative: unchanged small1280×720 hvcC/ispe, in-band SPS rewritten8001×8000 plus matching PPS: exit1, explicit `decode: Decoder plugin generated an error: Unspecified: coded parameter out of range`; no RGBA; frame markers0; only initial valid1280×720 config-derived marker1; zero oversized geometry markers. Thus the error is returned through the normal decoder plugin route before fallback to retained valid SPS/frame.
- Previously measured direct parser/API boundaries remain unchanged:8000×8000 and64000×1000 parser success;8001×8000 normal error8 before derived marker, same error8/more0 through normal de265 decoder API. Those parser-only controls are supported by the full-picture allocation positive control above.

## Resource review scope

See BOUNDED-REPORT.md source path review and RESOURCE-PATH.md. The SPS guard precedes conformance parsing/derived geometry and frame/map allocation. Decoder SPS is installed only after successful seq_parameter_set::read; dpb allocates frames from installed SPS; image-copy/SAO geometry derives from those frames, and CSP disables SAO. set_resolution is encoder-only. This plugin patch now returns the normal coded-range error instead of leaving heiferrOk/retrying. No alternate decoder SPS installation path was found in the pinned source.

The positive/negative instrumentation demonstrates the exercised oversized SPS cannot reach derived geometry/frame allocation, rather than merely observing no output. It does not prove an aggregate heap budget, total reference-frame count bound, or safety of arbitrary untested malformed bitstreams. Small NAL/parser/SPS structures can exist before the coded-area guard. Coded-picture area is capped64M; source driver cap remains50MiB. Three reference-unknown fixtures remain UNKNOWN; do not call the entire original gate PASS or adopt from this partial corpus.

## Build closure and artifacts

Built on baldr -j4, separate source/build/prefix directories. Static libheif+libde265,1decoder/0encoders, dynamic closure only C/C++ system runtime/libm/libgcc/loader. No dynamic codecs. New binaries, prefix archives, CMake cache/arguments and hashes recorded separately in logs/csp-bounded-failclosed-*. Results are failclosed-results/*. Prior result/report/manifests not modified. No product files/dependencies, system install, PR/push, Forgejo/CI or GUI/native host actions.
