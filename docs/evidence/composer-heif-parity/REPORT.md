# HEIC acceptance spike: neither pure-Rust candidate passes

No portable Rust decoder is accepted for composer G03. Both tested candidates fail the preregistered raw RGBA gate on the simplest official 8-bit control, independently of multi-image selection or transform issues. The gate was not changed.

## Fixed gate and exact reference

`GATE.md` was written before decode/measurement and still hashes to `f66dabe3863f9401b7fdadcfc13328ebd96a44959d9359e7cd398c7f255ed8d5`: dimensions/transforms/alpha exact, RGB mean absolute delta <=0.25, maximum <=2; source <=50MiB and <=64M decoded pixels before allocation; corrupt files return errors without panic.

- Candidates: crates.io `heic-rs =0.1.4` (`std`, serial decode), `heic-decoder =0.1.1` (default RGBA conversion). Exact archive registry SHA256 values and locked transitive dependencies are in `sources.json` / `rust/Cargo.lock`.
- Reference: exact **heic-to/csp 1.5.2** decoder source, verified against published npm SHA512 integrity. Its CSP libheif source header pins **libheif 1.22.2 / libde265 1.0.16** and runtime reports1.22.2. `reference-csp.mjs` only adapts ES export syntax to a Node CommonJS require context; it does not alter decoder code. It uses image.display raw RGBA and opaque alpha initialization exactly like the high-level worker, with no canvas/JPEG recompression.
- The first exploratory reference used the package's normal libheif build. The final table uses its actual CSP `libheif-without-unsafe-eval.js`; all 9 successfully decoded controls have byte-identical RGBA between normal/CSP. Both old results and final results are retained (`valid.json`, `csp-valid.json`). No tolerance or candidate setting was adjusted after a failure.
- Same machine/session: baldr x86_64, rustc1.99.0 pinned to repository toolchain, Node24.19.0. Build7.57s; no GPUI target created. Candidate/reference processes used a2GiB process-address-space guard and90s deadline. None timed out or hit that guard.

## Official controls and measurements

Nokia HEIF conformance repository commit `6ff1e33481d325296d8a787432453ef3fa91f591`; libheif official tests commit `12a259a5882dd99d45ec5fc77e296a3b620d1e2e`. Each source URL, byte count and SHA256 is recorded in `fixtures.json`. Official conformance descriptions are retained in `downloads/nokia-descriptions.xlsx`.

- C002: ordinary8-bit1280x720.
- C006:8-bit with auxiliary alpha.
- C007: primary2x2grid.
- C008/C014: derived identity image with rotation / crop+rotation.
- C042:mirror.
- MIAF002:10-bit2048x2048 (both probes confirm10bit).
- MIAF007:combined transforms.
- libheif rainbow451x461, alpha512x512 and crop controls.

| Official control | T3 CSP reference | heic-rs 0.1.4 | heic-decoder 0.1.1 |
|---|---|---|---|
| nokia-C002.heic | 1280×720 | FAIL: mean 1.733753, max 31, alpha 0 | FAIL: mean 1.738861, max 32, alpha 0 |
| nokia-C006.heic | 1280×720 | FAIL: mean 1.733753, max 31, alpha 0 | FAIL: mean 1.738861, max 32, alpha 0 |
| nokia-C007.heic | 1280×720 | FAIL: dimensions/transforms differ | FAIL: dimensions/transforms differ |
| nokia-C008.heic | 1280×720 | FAIL: candidate did not decode | FAIL: dimensions/transforms differ |
| nokia-C013.heic | reference error | UNKNOWN: reference did not decode | UNKNOWN: reference did not decode |
| nokia-C014.heic | 1280×720 | FAIL: candidate did not decode | FAIL: dimensions/transforms differ |
| nokia-C042.heic | 1280×720 | FAIL: mean 99.955283, max 255, alpha 0 | FAIL: mean 1.738861, max 32, alpha 0 |
| nokia-MIAF002.heic | 2048×2048 | FAIL: mean 6.792868, max 26, alpha 0 | FAIL: mean 6.791924, max 26, alpha 0 |
| nokia-MIAF007.heic | reference error | UNKNOWN: reference did not decode | UNKNOWN: reference did not decode |
| rainbow-451x461.heic | 451×461 | FAIL: mean 8.180478, max 28, alpha 0 | FAIL: mean 0.281435, max 5, alpha 0 |
| with-alpha-512x512.heic | 512×512 | FAIL: mean 1.436456, max 67, alpha 4076 | FAIL: mean 0.200556, max 59, alpha 4076 |
| clap_cropped.heic | reference error | UNKNOWN: reference did not decode | UNKNOWN: reference did not decode |

Each candidate: **9 FAIL / 3 UNKNOWN / 0 PASS** under this whole-file, actual-T3-selection gate. Reference decoded 9/12; it rejected C013, MIAF007 and clap_cropped with its own security-limit/decode errors. Those rows remain UNKNOWN, not candidate failures or passes. See `reference-logs/`.

## Separate image-selection differences from pixel failures

Actual published `dist/csp/heic-to.js` was inspected, not inferred from libheif list order: its worker calls decoder.decode and then takes `D1[0]`. Source `src/worker.js:15` likewise says `const image = data[0]`; `esbuild.mjs:13` maps the CSP worker to `libheif-without-unsafe-eval`. Snippets are in `highlevel-selection-proof.txt`.

For official C007, that first image is a 1280x720 nonprimary component; the primary grid is 2560x1440. For C008/C014, first image is likewise a nonprimary untransformed image while the primary is rotated/cropped. The candidates' public decode APIs return the primary image. Their dimension mismatch here is an **image-selection parity difference**, not evidence that grid composition or rotation itself decoded incorrectly. heic-rs additionally cannot probe the derived iden primaries (missing hvcC). No pixel metric is reported across different image selections.

The adoption rejection does **not** depend on these selection rows: on C002, all three paths select the same sole 1280x720 image. heic-rs mean 1.733753/max 31 and heic-decoder mean 1.738861/max 32 both fail0.25/2. On the sole 10-bit MIAF002 image, means are6.792868/6.791924, max 26. On the primary512x512 alpha control both differ on4076alpha pixels; exact-alpha fails. heic-rs additionally differs severely on sole-primary mirror C042 (mean 99.955283/max255). These are measured output differences; this spike does not assign unproven internal root causes.

## Input/error/allocation evidence and limits

- Tiny wrapper checks source metadata50MiB before reading, reads at most50MiB+1, then probes display and coded primary dimensions before decode; checked multiplication rejects zero/overflow/>64M. heic-rs also receives max_pixels=64M.
- 1 unit test checks exact64M boundary, over-limit, zero and multiplication overflow: PASS.
-6 controlled invalid inputs (empty, nonHEIF,8-byte/64-byte headers, half-truncated C002, sparse50MiB+1 source) × 2 candidates: **12 errors,0 panics,0 uncontrolled failures**. This finite corpus is not a general fuzz/safety proof.
-Comparison positive controls PASS: identicalRGBA accepted; RGBmax3 rejected; alpha1 rejected; shape mismatch rejected; failed reference UNKNOWN.
- **Full64M preallocation gate is NOT SATISFIED by source review.** heic-decoder exposes a fixed268M internal limit rather than a configurable64M limit; primary probe alone does not constrain every grid tile/auxiliary image. heic-rs max_pixels bounds declared/output dimensions, but decode_item→decode_coded→hevc::decode_still does not receive that limit; Context::coded_size prefers container ispe, and HEVC Plane::new has a separate1<<30 sample limit. The wrapper therefore does not prove a64M ceiling for inconsistent coded geometry or all auxiliaries. Do not promote it as a safe production adapter.

## Recommended next route — not yet built or accepted

Evaluate **decode-only statically built libheif 1.22.2 + libde265 1.0.16 from pinned open source**, matching the actual T3 decoder versions, rather than adopting either failed pure-Rust library. Disable encoders/unneeded codec plugins and avoid runtime system-package/plugin discovery; package the built decoder for each supported target. Preserve50MiB/64M preallocation guards over every coded plane/grid/alpha, decide actual first-image selection explicitly, and rerun this exact unchanged quality gate before product integration. This recommendation is a next experiment, **not a passing-library claim or implementation approval**.

Pinned upstream source archives and checksums (downloaded for reproducibility, NOT built):

- libheif 1.22.2, commit`763bc8bb87788d64d39ece623ceed988de12dc5b`, archive SHA256`0a3559b557793d8d634d071ee7181af34f2b69b0d61cd48a0dcdbfe4ea79fc43`.
- libde265 1.0.16, commit`7ba65889d3d6d8a0d99b5360b028243ba843be3a`, archive SHA256`9dedf12d5b5047c270d1fc54f5d92fc29d9ea21b94d3f2d998200258353189a0`.

## Inspect/reproduce

- `rust/src/main.rs`, `rust/Cargo.toml`, `rust/Cargo.lock`, `rust/rust-toolchain.toml`: tiny candidate CLI plus unit limit test.
- `reference-csp.mjs`: exactT3 CSP raw decode adapter.
- `measure.py`, `check-csp-reference.py`: measurement and final CSP fidelity confirmation.
- `csp-valid.json`, `corrupt.json`, `table.md`, `host.txt`, `binary.sha256`: final machine-readable and human evidence.
- `sources.json`, `fixtures.json`, `next-native-sources.json`, `checksums.sha256`: provenance.
- Raw RGBA/stdout/stderr retained on baldr at `/home/god/work/qa-evidence/composer-parity/heic-spike/results/`; final reference files end `.csp-reference.rgba`.

No product source, PR, push, CI, native UI host, system package, service or credential was changed. All measurements finished; no background codec process remains.
