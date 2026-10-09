# Maintenance capability and release compatibility matrix

Issue [162](https://git.oklabs.uk/BeFeast/okilum/issues/162), P3 test infrastructure.
No product capability or hash schema changes. Historical generic maintenance CI312
remains failed: 117 passed, 15 enabled-link fixture assumptions failed. Its status
must never be rewritten from a newer, differently scoped matrix result.

## Two gates, explicitly separate

Normal enabled `main` runs full unfiltered `cargo test --workspace`. CI also runs
six small script guard tests. That ordinary PR CI does **not** run a release
artifact matrix and must not be reported as doing so.

`maintenance-test-matrix.py` recognizes only the exact `NEW_LINKS_ENABLED` literal.
For maintenance it requires artifact inputs before running any tests. It enumerates
actual Cargo tests, verifies the 15 specifically classified names, excludes only
those enabled-link setup tests and executes every other workspace test. A missing
classified test or a new test accidentally matched by Rust's substring `--skip`
refuses execution. Unrelated failures remain fatal. Enabled builds never use this
exclusion list. Retained-operation and wire-format tests remain in the maintenance
suite. Names and the classification are in `scripts/maintenance-feature-tests.json`.

Passing the reduced maintenance source suite is insufficient: the same command
then runs actual frozen-binary posture, operation-disposition, source-recovery and
reviewed-packet compatibility gates. Missing artifacts, changed hashes, missing
provenance or an unsupported feature graph refuse the run. The maintenance source
commit must match the checkout exactly. CI can supply a reviewed local manifest
via `OKILUM_MAINTENANCE_ARTIFACTS`; this change creates no runners, artifact download
service or automatic release enrollment. A maintenance CI job without that input
fails deliberately instead of passing on exclusions alone.

## Build and freeze before verifying

Use one actual full GUI+cored Cargo invocation for each supported release. Do not
build only cored: GPUI's feature graph enables serde_json preserve_order, which the
existing goal-definition/review hash schema depends upon. This matrix does not
change that historical schema. Never enable links in the release maintenance
binary to satisfy enabled-feature tests.

```bash
scripts/vendor-setup.sh
scripts/vendor-setup.sh --verify
CC=/usr/bin/cc CXX=/usr/bin/c++ cargo build --release -p okilum-shell -p okilum-cored --message-format=json
```

Freeze the actual emitted GUI and cored executables together, their SHA256 values,
exact source commit and Cargo compiler-artifact records. Copy before any other
build can overwrite the target directory. Feature graph and provenance must be
reviewed; a manifest is a binding to supplied evidence, not an independent
cryptographic attestation that a claimed build command ran. The matrix validates
both frozen files, their receipt hash, source commit and compiler-artifact entries.
No GUI is opened by the artifact gate.

The input manifest has schema `tessera-maintenance-matrix-input/v1` and exactly
three required artifact entries, `enabled`, `maintenance` and `old5b`. Each uses:

```json
{
  "binary": "../release/okilum-cored",
  "sha256": "<64 lowercase hex>",
  "source": "<40 lowercase hex>",
  "freeze": "../release/freeze.json",
  "freeze_sha256": "<64 lowercase hex>",
  "feature_graph": "gui+cored"
}
```

Paths resolve relative to the manifest. The freeze receipt has `commit`, `files`
(`okilum-cored`, and `okilum` for full builds), and the actual `compiler_artifacts`
entries. GUI is the frozen cored's sibling. The `old5b` entry instead uses
`historical-cored-only`; its binary must match the specifically retained SHA256
`5b129b192156585bd3fbf3aa48f4bc5c8f5c726a8df782edfbad1b78e7c47bd9`.
Do not regenerate that artifact or substitute an arbitrary broken binary.

Run source posture plus mandatory artifacts on a maintenance checkout:

```bash
python3 scripts/maintenance-test-matrix.py --artifacts /absolute/path/artifacts.json --output /absolute/path/result.json
```

Run just a reviewed historical/current release-pair matrix from an enabled checkout:

```bash
python3 scripts/maintenance-test-matrix.py --matrix-only --artifacts /absolute/path/artifacts.json --output /absolute/path/result.json
```

The latter records `source_unit_tests_ran: false`; it cannot replace source CI.
Artifacts may include personal build paths in local receipts, never committed to
this repository. Keep evidence for different releases separate.

## What actual binaries must prove

1. Existing `verify-maestro-maintenance.py`: new links refused, retained link and
   settings readable, new observations retained, original goal unchanged, no new
   stages, exact link/unlink replay including offline and restart.
2. Existing `verify-maestro-dispositions.py`: held real GET produces pending,
   abandonment wins its late response, tombstones prevent delayed first work,
   retained rejection/query/unlink/replay survive enabled↔maintenance restart.
3. `verify-maintenance-source-recovery.py`: a real permission failure leaves a
   committed link receipt and pending Markdown projection. Maintenance startup
   flushes that original write, restores exact replay, preserves goal bytes and
   survives another restart. A positive file-write control must actually fail;
   root/DAC-bypass execution is refused rather than falsely proving the fault.
4. `verify-reviewed-packet-maintenance.py`: the writer prepares and explicitly
   reviews a packet; the reader opens that unchanged packet, then reopens after
   restart. Source, goal, guidance, citations, manual pins and both hashes remain
   exact. Actual export retains canonical bytes. A real source edit makes it stale
   and rejects another review. Run enabled→maintenance **and** maintenance→enabled.
   A newer helper may seed the exact citation for an older writer lacking the brief
   API; that writer still creates/reviews the packet. Seeding is never substituted
   for packet compatibility.
5. Old5b negative control: the same frozen reviewed packet and hashes remain on
   disk, but readback specifically reports unreviewed/stale with `goal definition
   changed; prepare and review a new context`. Startup failure, missing API, timeout
   or any other error fails the negative control instead of counting as success.

All brains and providers are isolated temporary fixtures. GET-only traces include
an actual forbidden-verb positive control. No installed service, user brain, T3,
Todoist or Maestro service is contacted. Existing Backend lifecycle cleanup owns
only its child processes. These tests do not claim native UI or phone acceptance.
