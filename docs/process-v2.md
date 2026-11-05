# Delivery process v2 — 31 October 2026

Historical implementation notes. Process policy is superseded by **Okilum — how
we work (v3, 02.11)** (see [AGENTS.md](../AGENTS.md)). The cadence, review and
reporting policy from v2 is not an instruction to agents. The sections below
describe CI and producer mechanics, not additional merge gates.

## Merge admission

`fast / check (pull_request)` remains a required main protection context.
It runs the exact PR head on baldr's dedicated `fast-linux` Forgejo host runner (capacity two):
desktop/supervisor build, `cargo check --locked --workspace --tests` (compilation
only), plus deterministic okilum-paste library unit tests. Test fixture/API drift
therefore blocks merge without executing the full runtime suite inside this job.
A separate `fast / shell-full (pull_request)` job executes the complete unfiltered
Linux `okilum-shell --bin okilum` suite with four test workers and a nonempty
inventory control (#2415). It has its own twenty-minute budget and reuses the
same locked persistent source/target pool after `check`. No test is removed or
weakened. Main protection is changed only after this reporting context passes
reliably twice on the fast runner. PR #2434 and issue #2415 record the resulting
required-status decision and measured margin; see [the rollout](fast-runner.md).
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

Review and merge policy comes from v3. Native, Brain/transport, perf,
apt timing and main-health checks stay in the full workflows. The process-local nofile helper
remains; no system hard limits change. No data/native gate is added to fast CI.

## Full acceptance

Full Forgejo CI, Brain, Inbox and compatibility suites run at 22:05 UTC Tuesday
and Saturday (the nights before Wednesday/Sunday Stable), or workflow_dispatch.
The full Linux and native macOS scripts keep their complete suites. AppImage
native installation tests reuse the original main beta artifact on the nightly
GitHub run, retaining producer/source/digest identity. Nightly does not fabricate
fresh production bytes or promote a release.

## Independent beta producers

The three Forgejo beta producers run hourly at minute 17 UTC, plus explicit
`workflow_dispatch` for urgent fixes (#2500). They read the current main tip and
require `fast / check (push)` and `fast / shell-full (push)` success on that exact
merge commit. `fast.yml` runs the same required jobs after merge; PR-head success
does not stand in for merge health. A missing, pending or failed status skips the
hour with an explanation and no fallback to an older commit. The next hour retries.

macOS beta skips this gate (Oleg 04.11, #2589): it always builds the newest
first-parent main commit, and a pinned run that main has moved past exits without
building. Its hosted compile, sign and notarize run is the gate. Linux and Windows
keep the certification gate.

The current platform beta feed, rather than an archive-only source catalog,
suppresses an unchanged beta. Linux reads the active AppImage feed, not frozen
pacman. If a queued tick captured an older main, it dispatches a new main run with
an expected source. That run revalidates the source before building. Artifacts,
`notes.json` and `linux-release / …`, `macos-release / …`, `windows-release / …`
shipping statuses therefore remain on the beta's actual source commit. Daily
Stable selection can still step back through the per-platform `previous_build` /
`previous_source` chain. Each notes range includes all visible merges since the
previous published beta; intermediate merges do not invent intermediate builds.

Hourly producer groups cancel superseded work. Explicit archive, AppImage reuse
and Windows re-sign runs use independent groups, while protected publication and
Stable keep their existing non-cancellable serialization and monotonic guards.
A cancelled producer cannot authorize publication. A missed attempt retries at the
next tick; feed identity and existing immutable GitHub artifacts prevent duplicates.
GitHub `forgejo-linux` and `forgejo-macos` retain immediate trusted PR refs but drop
main push triggers and use the same hourly green-main admission. macOS shipping
remains dispatch-only, called by the hourly Forgejo producer; it keeps its signing
lock and keychain cleanup. Native nightly AppImage acceptance remains a separate
schedule, consuming the original digest-verified bytes rather than rebuilding.

Automatic Windows builds remain unsigned; explicit daily re-sign is unchanged.
Stable still requires signed Windows metadata and verified signatures. macOS beta
uses hosted shipping only: corporate M4 CI remains prohibited (#2139). Developer
ID, notarization and Sparkle signing remain required. Linux builds the AppImage
once on GitHub; the protected publisher checks source/size/digest/provenance,
signs and publishes it independently of Arch/Windows/Mac. Idle selector and nightly
copy runs do not become ambiguous original AppImage producers. Two actual original
producers remain an error. Beta package checks stay separate from overnight suites.

All publication retains immutable source/build identities and monotonic feeds.
No beta action promotes Stable. Sprint's workflow has no triggers. Historical
Sprint branches must receive the same trigger retirement before another push.
