# Delivery process v2 — 31 October 2026

Historical implementation notes. Process policy is superseded by **Okilum — how
we work (v3, 02.11)** (see [AGENTS.md](../AGENTS.md)). The cadence, review and
reporting policy from v2 is not an instruction to agents. The sections below
describe CI and producer mechanics, not additional merge gates.

## Merge admission

`fast / check (pull_request)` is the sole required main protection context.
It runs the exact PR head on baldr's dedicated `fast-linux` Forgejo host runner (capacity two):
desktop/supervisor build, `cargo check --locked --workspace --tests` (compilation
only), the complete Linux headless shell binary suite
(`cargo test --offline --locked -p okilum-shell --bin okilum`, #2415), plus
deterministic okilum-paste library unit tests. The shell suite also catches
runtime contract drift and executor teardown failures that compilation cannot.
The fifteen-minute job budget includes setup and build. The LAN compiler cache and
Cargo downloads are reused; two separately locked persistent targets
allow two simultaneous builds. Each job has three compiler workers (six total).
The runner queues additional jobs before their fifteen-minute execution budget starts.
The budget includes both desktop code generation and workspace test compilation
after a base-crate change; see [the measurements](fast-runner.md).
A timeout or failed build/test fails closed. No GitHub hosted job or bridge participates in PR admission.
Windows and macOS compilation moves to each post-merge beta producer. This is
post-merge OS validation, not an OS waiver. The first production timing must be
measured rather than inferred from timeout.

Review and merge policy comes from v3. Native OS acceptance, Brain/transport, perf,
apt timing and main-health checks stay in the full workflows. The process-local nofile helper
remains; no system hard limits change. The shell tests use GPUI's headless test
platform; real desktop/device acceptance stays in the full workflows.

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
