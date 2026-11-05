# T3 nightly candidate compatibility gate

The updater keeps T3 on the nightly channel. At most one quiet-window update per day may be considered, after this probe passes against the candidate and the current compiled Okilum client. This code does not install, restart or switch a production server. A protocol-version change requires a coordinated client/server update and must block unattended switching.

## Schema policy

Additive fields, RPCs and variants are informational. Removed fields (including optional fields), RPCs or variants, new required fields and narrowed types are breaking. Generation, opaque required consumer schemas or runtime-probe errors fail closed. The historical 3814 schema is provenance, not permission to invent a missing historical contract: its `subscribeThread.output` is `{}` because the upstream RPC output was unknown. Explicit decoded stream/bounded consumer schemas are needed in addition to RPC schemas. Decoded-schema checks are distinct from the actual client wire round trips.

## Removed budget flag

Upstream commit `553203c57b780cef9fb30ad835d759befe0dcb7c` removed `payloadBudgetExceeded`. It was not renamed: bounded paging uses `hasMoreHistory` and an opaque cursor, and item detail may be hydrated separately. Okilum removes the stale flag and misleading budget warning while retaining Load Earlier, pagination and hydration checks. A bounded page must not be presented as complete thread history.

## Inbox pilot 510 — diagnosis only

`inbox/bridge/t3_questions.py:58–59` requires the removed `payloadBudgetExceeded` to be explicitly `False`; modern omission becomes `None` and deterministically yields `partial_source`. Proposed fix: validate modern `hasMoreHistory == false` plus an explicitly present null history cursor, validate the legacy flag only when present, and preserve request/item joins and fail-closed withdrawal safeguards. This is a source-level diagnosis; no live pilot poll, restart, repair or deployment was performed.

## Safety and evidence

Use an isolated candidate state directory and matching official binary. The probe creates its own idle scratch thread, validates its identity before any metadata mutation, and never asks for a provider/model run. Credentials are private inputs, not command arguments, logs or receipt fields. Every required operation needs actual runtime success; an unexecuted operation is not PASS. Exact usage is in [the probe README](../scripts/t3-contract/README.md).

## Validation receipt

The [sanitized candidate receipt](evidence/t3-contract-3991.json) records official
nightly3991 and the exact server/client/schema/baseline digests. All five actual
operations passed in a private loopback candidate; schema comparison found
0 breaking changes and16 additive observations. The candidate process was
stopped afterwards. This does not authorize a production server switch.

The full Rust client suite passed317 tests (3 ignored), and the real HTTP/GPUI
history pagination regression passed. All37 Python semantic/process/receipt
regressions passed under a1GiB address-space ceiling. Nested-schema fingerprints
use bounded structural digests; the suite includes actual large consumer roots
and removed/opaque/deep-field controls. No acceptance was relaxed to obtain PASS.

## Execution hosts

Never build Rust/Okilum, run Rust tests, or start a T3 candidate/probe server on
maestro. Build the client artifact and run candidate servers/tests on baldr or CI.
On baldr build in an `okilum-slot` build slot: it sets `nice -n 19`, the Cargo
job count and the slot's warm target, so do not set your own `CARGO_TARGET_DIR`.
The old shared `build.lock` recipe is retired.
The `--client-probe` argument always names a **prebuilt artifact from CI/baldr**;
it is not a request to compile locally. This CLI never builds that artifact.

Run the full Python wrapper where the isolated candidate state and matching CLI
are available (baldr/CI). Maestro may run only the ready-made native probe artifact
against an existing candidate, using private stdin prepared by the candidate
owner; it must not start a candidate server or share/copy live server state.
The hosted nightly bootstrap belongs exclusively to CI.
