# Static HEIC experiment: pixel parity passes, full acceptance remains blocked

**Overall: NOT ACCEPTED for product integration.** The source-built Linux CSP profile is byte-exact to actual T3 on every reference-decodable control (9 PASS / 3 UNKNOWN), but the universal64M-before-coded-allocation gate is not proven. No product/Rust FFI integration route is approved from this experiment.

The original immutable GATE.md is unchanged (`f66dabe3863f9401b7fdadcfc13328ebd96a44959d9359e7cd398c7f255ed8d5`). No tolerance was adjusted and every stock FAIL/reference UNKNOWN is retained.

## Build and runtime closure

- Exact source archives previously pinned and SHA256-verified: libheif1.22.2 commit763bc8bb87788d64d39ece623ceed988de12dc5b; libde2651.0.16 commit7ba65889d3d6d8a0d99b5360b028243ba843be3a.
- Built on baldr x86_64, CMake4.2.3 / GCC15.2.0, `-j4`, isolated prefix `/home/god/work/qa-evidence/composer-parity/heic-spike/native/prefix`. No system installation.
- Both codec libraries are STATIC/PIC. Driver directly links the QA-prefix libheif.a + libde265.a, with no pkg-config/system codec library selected at link time. Exact static archive/ELF SHA256 values are in `logs/*-binaries.sha256`.
- Runtime assertions: **exactly1decoder (libde2651.0.16),0encoders**. All encoder/other codec/plugin-loading/sharpyuv/header-compression/uncompressed-codec flags areOFF; TIFF/JPEG/PNG/WebP/ZLIB/Brotli helper discovery is disabled. CMake builds an unused minimal heifio helper archive, but it is not linked into the driver.
- `ldd`/ELF NEEDED show only standard libstdc++, libm, libgcc_s, libc and loader. No dynamic libheif/libde265, image codec, plugin or downloaded binary exists in runtime closure. See `logs/*-ldd.txt`, `host-runtime.txt`, CMake caches and exact arguments.

## Explicit, recorded source-profile changes

No product source was changed. QA-only patches are preserved separately:

1. **Common decode-only registration patch:** upstream unconditionally registers its built-in mask encoder despite all encoder CMake switchesOFF. The initial setup feature assertion caught encoder_count1 before any native image decode. `decode-only-mask.patch` omits that registration in both builds, yielding encoder_count0. Other codec code is untouched.
2. **CSP filter profile patch:** upstream `decoder_libde265.cc` disables DEBLOCKING and SAO in its existing `__EMSCRIPTEN__` branch. A one-line guarded change also selects that branch when `OKILUM_HEIC_CSP_FILTER_PROFILE` is defined. It does not define `__EMSCRIPTEN__` globally or change transforms/colour math. `stock` retains native filter behavior; `csp-profile` matches actual heic-to/csp behavior. Both profiles were declared in EXPERIMENT.md before native image measurements.

The common setup correction and profile patch are not hidden as pristine upstream. `source-build-manifest.json` hashes driver, patch files, source profile and build inputs.

## Same reference, metrics and first-image selection

Reference remains the exact verified heic-to/csp1.5.2 bundled libheif1.22.2/libde2651.0.16 raw RGBA. No JPEG step. The tiny C++ driver obtains the same top-level ID list and decodes **its first ID**, not the primary item. It uses interleaved RGBA8, geometric transformations enabled, libheif default colour-conversion settings, non-strict decode and one codec thread.

| Official control | Stock native mean / max / alpha mismatches | CSP profile mean / max / alpha mismatches |
|---|---|---|
| nokia-C002.heic | FAIL 0.331705 / 8 / 0 | PASS 0.000000 / 0 / 0 |
| nokia-C006.heic | FAIL 0.331705 / 8 / 0 | PASS 0.000000 / 0 / 0 |
| nokia-C007.heic | FAIL 0.187148 / 7 / 0 | PASS 0.000000 / 0 / 0 |
| nokia-C008.heic | FAIL 0.331705 / 8 / 0 | PASS 0.000000 / 0 / 0 |
| nokia-C013.heic | UNKNOWN: reference did not decode | UNKNOWN: reference did not decode |
| nokia-C014.heic | FAIL 0.331705 / 8 / 0 | PASS 0.000000 / 0 / 0 |
| nokia-C042.heic | FAIL 0.331705 / 8 / 0 | PASS 0.000000 / 0 / 0 |
| nokia-MIAF002.heic | FAIL 0.055375 / 3 / 0 | PASS 0.000000 / 0 / 0 |
| nokia-MIAF007.heic | UNKNOWN: reference did not decode | UNKNOWN: reference did not decode |
| rainbow-451x461.heic | FAIL 0.129555 / 5 / 0 | PASS 0.000000 / 0 / 0 |
| with-alpha-512x512.heic | FAIL 0.110212 / 28 / 4076 | PASS 0.000000 / 0 / 0 |
| clap_cropped.heic | UNKNOWN: reference did not decode | UNKNOWN: reference did not decode |

For the CSP profile all9 comparable controls have **RGB mean0 / max0 / alpha mismatch0**: dimensions, applied transform/first-image selection and every RGBA byte match. This includes8-bit,10-bit MIAF002, sole-primary mirror, alpha and actual first-image behavior on multi-item grid/rotation files.

Stock profile:9 FAIL /3 UNKNOWN. CSP profile:9 PASS /3 UNKNOWN. C013/MIAF007/clap_cropped remain UNKNOWN because actual T3 reference itself rejects them; native libheif produces the same coded-size/security-limit errors. They are not silently counted as passes. The previous pure-Rust candidates' failures remain unchanged.

## Real configured limits and finite controls

`decode.cc` checks source file size<=50MiB before compressed-buffer allocation/read. It copies finite default context limits, sets `max_image_size_pixels=64000000` through **actual libheif API before parse/decode**, then checks transformed first-handle dimensions before output. No global limit is disabled. Context/data/handle/image/options lifetimes are RAII-managed; errors return JSON+exit1 instead of aborting.

Each native profile ran8 controls: empty, nonHEIF,8-byte/64-byte headers, half-truncated C002,50MiB+1 source, over64M declared ispe, and64x64 ispe with a real1280x720 hvcC SPS. All16 runs returned errors without abort/unexpected output. The ordinary valid controls are positive controls showing the decoder path executes.

The inconsistent small-ispe/large-SPS control returned **“Image size1280x720 exceeds the maximum image size16384”** from the context's tightened SPS check before plugin decode. Over64M declared output was rejected by driver preflight before decode. Source-over50MiB was rejected before read. These controls prove those paths are wired, not every conceivable allocation path.

## Remaining resource gate and precise follow-up boundary

See RESOURCE-PATH.md for exact source paths/lines. libheif checks **configuration hvcC SPS** coded dimensions before plugin creation/push/decode, and grid/alpha image items receive context limits. However decoder.cc explicitly notes its unresolved check for replacement SPS inside the image bitstream. Its libde265 API5 adapter does not install a64M allocation guard. libde265 SPS parsing only checks dimensions individually<=65535; `de265_image::alloc_image` later allocates pixels without the application's area cap.

A narrow possible policy patch belongs after the existing positive/max-dimension checks in `seq_parameter_set::read` (libde265/sps.cc), **before derived CTB/sample maps and image buffers**: checked64-bit width×height<=64000000; use normal `DE265_ERROR_CODED_PARAMETER_OUT_OF_RANGE`. It must preserve valid wide/tall images under the area cap, not replace the policy with an arbitrary8192-width limit. A redundant alloc_image entry guard can cover internal callers but is later than SPS-derived structures. This guard was **not** implemented blindly in the experiment.

Required follow-up controls: exact64M wide/tall boundaries, over64M/overflow/zero, **in-band SPS replacement after small trusted config**, auxiliary alpha and grid tile paths, and proof of rejection before buffers/maps; then rerun the unchanged9 raw-RGBA comparisons. Current plugin failures may propagate as a bounded50-attempt “did not give a decompressed image” error; preserve/refine that explicit failure path, never null success/abort.

Until that resource gate is satisfied, this experiment establishes a matching Linux decode profile and static dependency closure, not a passing portable production adapter. No Mac/Windows build, Rust FFI integration, packaging, GUI or provider acceptance is claimed.

## Inspect/reproduce

- `build.sh`, `decode.cc`, two named patches, `EXPERIMENT.md`: reproducible QA-only build/driver/profile.
- `source-build-manifest.json`, `logs/*-cmake-args.txt`, `*-CMakeCache.txt`, `logs/*-features.json`, `logs/*-binaries.sha256`, `host-runtime.txt`: complete source/build/runtime provenance.
- `valid.json`, profile-specific result JSON, `table.md`, `corrupt-resource-*.json`: immutable result records.
- Raw RGBA/stdout/stderr: baldr `/home/god/work/qa-evidence/composer-parity/heic-spike/native/results/`.
- `RESOURCE-PATH.md`: limited source proof and missing in-band SPS guard; no fabricated universal safety claim.

All experiment build/decode processes finished. No system packages/services/credentials, product source, PR, push, CI, Forgejo request or GUI host was changed.
