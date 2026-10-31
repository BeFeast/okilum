# HEIC portability spike: preregistered acceptance gate

Declared before any candidate/reference decode or metric measurement, 31 Oct 2026.

Scope: HEIC/HEIF source preparation for composer G03, preserving actual T3 heic-to/libheif decoding behavior. No product edit, PR, native UI acceptance or release claim.

Each candidate is compared against pinned public libheif-js raw RGBA on the SAME host/session/fixtures, before any JPEG recompression:

- Decoding succeeds for official valid controls, including 8-bit, 10-bit, transform (rotation/mirror), grid and alpha controls where official public fixtures exist.
- Dimensions, applied transforms and alpha channels match the reference exactly.
- RGB mean absolute channel delta <= 0.25 AND maximum absolute channel delta <= 2 per fixture.
- Input source ceiling 50 MiB. Dimension/pixel metadata must be inspected and rejected above64,000,000 pixels BEFORE decoded image allocation.
- Corrupt/truncated files return an error without a Rust panic or uncontrolled process failure.
- Dependencies are pinned with source/package checksums; no unverified prebuilt native binaries or system package/service modifications.

A failed gate stays failed. Missing controls/reference evidence means UNKNOWN, never PASS. Cheap implementation is not a substitute for a passing library.

Measurement output is written separately; this file is not changed after results.
