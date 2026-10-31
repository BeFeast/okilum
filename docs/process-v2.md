# Delivery process v2 — 31 October 2026

Owner-approved policy: main beta replaces Sprint. Target cadence is approximately
30 minutes; Stable runs Sunday and Wednesday after the previous night's full
acceptance. P0/P1 releases may run outside that cycle.

## Merge admission

`fast / check (pull_request)` is the sole required main protection context.
It uses an exact-head hosted matrix: Linux, Windows and macOS desktop/supervisor
builds plus the deterministic okilum-paste library unit tests. The aggregate
fails if any platform fails, skips or times out. The bridge is bounded to ten
minutes including queue time; the build step has a six-minute bound with time retained for cache persistence.
The bridge checks all three exact platform receipts directly, requiring source,
run/attempt, runner label and successful executed build/test step. It does not
need a fourth hosted aggregate runner. The API run summary may lag the actual
jobs; explicit cancelled/failed records and missing/skipped steps never pass. Cold
caches or unavailable hosted capacity are real failures, never fallback success.
PR-local fast refs persist until PR-close cleanup so compiler caches survive
corrective pushes; updates are normal fast-forwards, never force pushes.
The first production timing must be measured rather than inferred from timeout.

No approval requirement blocks merge. Review happens asynchronously after merge;
findings get a follow-up fix. Full shell/native, Brain/transport, perf, apt timing
and main-health checks stay in the full workflows. The process-local nofile helper
remains; no system hard limits change. No data/native gate is added to fast CI.

## Full acceptance

Full Forgejo CI, Brain, Inbox and compatibility suites run at 22:05 UTC Tuesday
and Saturday (the nights before Wednesday/Sunday Stable), or workflow_dispatch.
The full Linux and native macOS scripts keep their complete suites. AppImage
native installation tests reuse the original main beta artifact on the nightly
GitHub run, retaining producer/source/digest identity. Nightly does not fabricate
fresh production bytes or promote a release.

## Independent beta producers

Main pushes trigger all three platform workflows. Before compiling, a queued
producer checks the current main tip and skips an obsolete snapshot. Existing
source descriptors suppress duplicate builds. Active builds and signing are not
cancelled. Their trusted main snapshots may finish and publish monotonically even
if main advanced; this prevents continuous merges from starving beta.

Windows push builds are unsigned; manual re-sign remains separate. Stable still
requires signed Windows metadata and verified signatures. macOS push delivery
uses hosted shipping by day; the existing nighttime lane switch admits the M4.
Developer ID, notarization and Sparkle signing remain required. Linux builds the
AppImage once on GitHub; the protected publisher waits for the original accepted
package, checks source/size/digest/provenance, signs and publishes it independently
of Windows/Mac. Beta package checks are separate from overnight native suites.

All publication retains immutable source/build identities and monotonic feeds.
No beta action promotes Stable. Sprint's workflow has no triggers. Historical
Sprint branches must receive the same trigger retirement before another push.

## Measurement

For the rollout merge, retain its exact SHA and timestamp, each platform producer
run, descriptor/build and the first verified beta-feed observation. Report
merge-to-feed wall minutes separately for Linux, Windows and macOS, including
queue/sign/notary/publication waits. A failed or missing platform stays explicit;
it never inherits another platform's readiness. Receipts live under
`qa-evidence/process-v2`, not a new telemetry service.
