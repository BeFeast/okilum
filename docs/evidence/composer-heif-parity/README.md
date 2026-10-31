# G03 HEIC evidence bundle

Start with the new [T3-parity scoped report](native/PARITY-SCOPED-REPORT.md): **backend PASS** for reference-decodable pixel parity plus matching T3 error outcomes and supplementary allocation/corrupt/cap controls. **Product GUI visible error remains untested.**

The [original preregistered gate](GATE.md), [pure-Rust report](REPORT.md), [native stock/CSP report](native/REPORT.md), [bounded report](native/BOUNDED-REPORT.md), [failclosed report](native/FAILCLOSED-REPORT.md) and [resource review](native/RESOURCE-PATH.md) are verbatim immutable historical evidence. Their complete-corpus UNKNOWN/rejected outcomes are unchanged. The scoped report is an explicit manager disposition, not an edit to those gates/results.

## Included evidence

- [Public fixture provenance/checksums](fixtures.json) and [package/reference provenance](sources.json); no fixture payloads.
- [Nine byte-exact results / three original quality UNKNOWN](native/failclosed-results/valid-csp-bounded-failclosed.json).
- [Fresh matching error outcomes for each of the three](native/parity-results/reference-errors.json), both reference and native with bounded execution.
- [Malformed/cap controls](native/failclosed-results/corrupt-resource-csp-bounded-failclosed.json), [whole-HEIF allocation positive/negative controls](native/failclosed-results/failclosed-inband.json), direct SPS/API controls under native/logs.
- [Pinned build/source manifest](native/failclosed-source-build-manifest.json), reviewable patches and [preregistered failclosed experiment](native/FAILCLOSED-EXPERIMENT.md).
- [Artifact inventory](artifact-list.json) records SHA256 and size for every verbatim source copy in this bundle. `bundle-checksums.sha256` covers the final publish bundle including this index.

## Deliberately omitted artifacts

No raw RGBA, image fixtures, source archives/checkouts, binaries/static libraries, compiler logs/CMake caches or private vault material are included. Historical manifests/report prose may name omitted build artifacts and QA build-prefix paths; those are archival provenance, not links required to assess the included measurements. Their original bytes were preserved rather than rewritten. Full manifest hashes remain useful for identifying the omitted artifacts. All new navigation links in this index and the scoped report resolve within this bundle.

Publication, repository location and the manager-authorized follow-up issue **viewer: HEIF beyond T3 decoder** are handled by the primary agent. This bundle itself makes no product integration or native UI acceptance claim.
