# Same-runner input A/B (#1662)

The strict, non-required, scope-aware `perf-linux` job pins main once and builds main and
PR release binaries with the same toolchain/flags on the same runner. Both
binaries are **shipped-unpatched** builds: the ordinary release Cargo command,
with no gpui measurement patch. Build receipts, source commits and SHA256 sums
are uploaded for both. CPU model, affinity/vCPU count, kernel, runner/image and
run identity accompany each comparison.

For each of four Source fixtures and two supported Live Preview fixtures,
main and PR each get one warmup and five measured independent launches. The
order alternates main/PR then PR/main to counterbalance temporal drift. The
collector uses the same pinned fixture bytes for both roles. It verifies the
matching document/generation post-frame diagnostic, actual changed clipboard
buffer and changed pixels. Input starts before external key injection and ends
at the product's matching post-frame diagnostic; this includes OS queue time,
and does not establish physical display/scanout latency.

## Verdict (schema 2)

A scenario is a **regression** only when all three hold on this runner:

1. **Effect size.** The PR input median is worse than main's by more than 15%
   **and** by more than 3 ms, or the PR median exceeds the explicit
   **absolute ceiling of 250 ms** while main's median stays within it.
2. **Rank evidence.** An exact one-sided Mann-Whitney test over the five main
   and five PR samples gives p ≤ 0.05 / 6 (family-wise 5% across the six
   scenarios, Bonferroni). With five repetitions this means at most one of the
   25 main/PR sample pairs may have main slower than the PR. The test
   enumerates all 252 relabelings and assumes no distribution.
3. **Confirmation.** A valid regression permits one complete paired
   remeasurement of the same two SHA-verified binaries, and that remeasurement
   must regress **the same scenario**. A finding that moves to another scenario
   is not confirmed; both attempts stay visible with per-scenario verdicts.

All thresholds live once in `ab-policy.json`; no environment variable changes
them. The policy is rejected if its repetitions leave the rank test unable to
reject at `family_alpha`. p90, all raw samples, U (PR-slower pairs out of 25),
p and the per-scenario verdict (`pass`, `not-significant`, `regression`,
`main-over-ceiling`) are in `result.json` and the job summary. Infrastructure
and missing evidence never retry. Both attempts share the 590-second deadline.

If **main's** median exceeds the ceiling on this runner, the ceiling cannot
judge the PR: the run is `NO_EVIDENCE` (infrastructure), never a regression.
The ceiling is about twice the slowest hosted input median observed so far
(132 ms, #1639 run 40159269222; largest single A/B sample 144 ms). The
product's real-hardware app-key-event → post-frame acceptance (33 ms Linux /
16 ms M4) stays outside hosted CI. The old static #1639 baseline is retained as
historical evidence; A/B neither rewrites nor relaxes it.

### Why schema 1 produced false reds

Hosted input samples are **bimodal**: on a given runner they cluster at two
levels about one frame-scheduling step apart (for example ~36 and ~45 ms). A
median of five flips between the levels, so an unchanged binary moves by
+20–60%. Schema 1 (median delta only) reported a regression in at least one
attempt on 9 of 22 hosted report-only runs between 10 and 16 November 2026,
and a final FAIL on 5. Those five were two runs with identical main/PR source,
one test-only Rust change and two non-editor changes; in every case the
failing scenario differed between the two attempts. There is no direction
bias: the median main→PR delta was −0.3% over 192 scenario comparisons, 47%
of them positive.

`hosted-false-reds.json` keeps the samples of all nine runs (the artifacts
expire after seven days) and `test_ab.py` replays them. All nine pass schema
2. The same sessions with **+15 ms** added to the PR samples fail with
confirmation on all nine runs; +10 ms is confirmed on five of nine. Effects
smaller than one frame-scheduling step are not resolvable on hosted software
rendering with five repetitions.

Hosted Windows runners render with WARP (no GPU), whose threads inflate wall
time the same way (#2845). The rule that follows from both: only same-runner
paired evidence judges a change, and only reproduced, rank-significant
differences count.

## Scroll, running modes and activation

Scroll is excluded from A/B. The legacy scroll diagnostic continues separately
with a **measurement build, not shipped**, using the existing trace patch. Its
result and provenance remain visible and report-only. Neither an absent scroll
trace nor a scroll failure is represented as input regression.

Ordinary CI invokes `ab.py` with strict exits (Oleg, 17 November 2026: «strict,
but does not block the PR»): a confirmed regression is `FAIL` and exit 1, so the
job is red. Missing controls, collector/harness failures, main over the ceiling
and binary/provenance integrity errors remain `NO_EVIDENCE` with exit 2; they
are never hidden as PASS. The job is not required anywhere: `ci / check` does
not depend on perf, existing shared/macOS requirements remain, and
`gh-check.sh --fast` judges only its own `agent-checks` run.

It runs on every pre-merge push (`agent-checks-fast/<branch>`) whose diff
against its merge-base with main is native-scoped (`scripts/ci/macos-scope.py`,
the selection the Forgejo `native-scope` job uses), as a separate
`forgejo-perf-linux` run on the PR head; a newer push of the same branch
cancels the older run. The Forgejo bridge (`forgejo-perf-linux-pr/**`, nightly
`ci` and `commit-ci`) runs it as before. Measurement-only scroll runs only
there, not on pre-merge pushes. `ab.py --report-only` remains available for
manual diagnostics.

Multi-pass calibration (`calibration_runs` 2–5) compares the head with its
merge-base on main (so main moving while the job is queued cannot refuse it),
and keeps strict exits and the identical-product-input hash guard (all tracked inputs except explicitly listed
documentation, CI and measurement tooling; vendor setup, assets and unknown
build inputs included), stopping on the first failed invocation. A single
dispatched run (`calibration_runs` 1) is one strict invocation with no source
guard, which also serves as a forced-red control on a throwaway commit. No
automatic calibration loop or runner selection is performed.

Under schema 1, EPYC calibration run `40223917116` produced five valid green
invocations and forced-red run `40220173487` detected a diagnostic 120 ms
delay, but Xeon run `40230405638` produced two valid FAIL attempts with
identical product inputs, in a different scenario per attempt.

Schema 2 met the acceptance bar on GitHub-hosted runners (#3044): 15
consecutive valid green unchanged-code invocations (`43995388136` EPYC 7763,
`44011879910` Xeon 6975P-C, `44011876072` EPYC 7763 A/A of one commit built
twice) and two artificially slowed commits that failed with valid measurements
(+15 ms `43995392282` Xeon 6975P-C, +120 ms `43995396631` EPYC 9V74). Given
that evidence, Oleg chose a strict job that is **not** required (17 November
2026), which #3065 wired as described above. It is not part of `ci / check`.
Promoting it to required is a separate decision. It would add two hosted
release builds (~20 min) to every native-scope admission.

From #3065's merge (17 November 2026, 09:27 UTC) to 20 November 2026, 09:55 UTC,
GitHub recorded 242 `forgejo-perf-linux` runs. The `perf-linux` jobs that
completed by that cutoff break down as 127 success, 17 failure, 1 skipped and
91 cancelled. All 17 failures happened in the build step during one red-main
hour (runs created 17 November, 21:29–22:45 UTC: `PreparedDocument` missing
`outline`), before any measurement. None was a measurement verdict, and none
was a false red.

The sections below document the original static collector and measurement
boundary, retained for diagnostics and historical evidence.

# Linux editor latency guard (#1639)

The existing `perf-linux` job reuses its trace binary and Xvfb display. The
editor gate has a 300-second deadline including its one permitted retry, and
adds no runner or second build. The legacy Reader scroll growth measurements
are informational; their outcome is always shown in the job summary. Only the
new editor gate determines the perf job result. The entire `perf-linux` job is
supplementary: it is not a dependency of required `ci / check` (manager
disposition, 2026-10-24). Budgets and failures remain visible on each scoped PR.

`fixtures.py` generates original prose, with pinned SHA-256 hashes: 3 KiB,
130,000 bytes with 460 heading sections, exactly 1 MiB, and 1,200,000 characters.
An additional 60 KiB fixture holds an unclosed fence through EOF. It needs no
network or downloaded book. Each run uses a fresh, isolated vault,
application state and recovery directory; installed applications are not used.

## What is measured

Each Source scenario and both Live Preview scenarios have one warmup followed
by five independent launches. The report gives the median and nearest-rank p90
of the five measured repetitions. With five observations p90 is their maximum.

- `open_first_ms`: process launch to the first Source editor content frame.
  This includes startup and opening the note, rather than ending at loading UI.
- `input_frame_ms`: immediately before injecting the sparse key to the
  post-frame diagnostic for its exact document and source generation. This
  includes injection and OS queue delay. A changed clipboard buffer and window
  pixels independently prove the input was exercised. Pixel verification may
  wait up to two seconds for X11 to receive the software-rendered image; that
  untimed verification does not move the post-frame measurement endpoint.
- `open_full_ms`: small fixture only, process launch to a frame after the
  matching complete Live Preview provider was accepted. A limited provider is
  never a successful sample. Fixtures have no asynchronous media.
- `scroll_frame_ms`: each repetition's p90 CPU `draw` cost during its wheel
  burst, using the existing trace fields. Software-rasterizer `present` cost
  is excluded. At least five redraws, changed window pixels and a nonzero
  editor scroll offset are mandatory.

`live:raw-tail-60k.md` injects **Enter at EOF inside an unclosed fence** rather
than a character at the start. A Rust regression uses the exact pinned fixture
to prove `remap` refuses this edit and `remap_raw_tail` accepts it. Consequently
the real keyboard-to-frame measurement covers the fallback scan accepted in
the #1654 review, without changing or separately timing the core implementation.

GPUI calls `on_next_frame` **before** rendering. The diagnostic nests two
callbacks: the second runs after the intervening draw/present returned. It
rejects a changed document/generation as `superseded`, and absent layout as
`unpainted`. Sparse typing avoids coalescing; missing or superseded samples
fail as **no evidence**, rather than being replaced with a later caret blink.
Timestamps use Linux `CLOCK_MONOTONIC`, shared with Python's `monotonic_ns`.
Writing the diagnostic file happens asynchronously after the timestamp.

This is the **Xvfb software post-frame boundary**. It proves neither GPU
completion nor physical display scanout, and does not claim the product targets
of 16 ms on M4 or 33 ms on weak Linux. Mac and GPU measurements are later work.
The three large Live Preview scenarios are explicitly **unsupported until
#1640**. They are neither passes nor failures; removing these exclusions and
proving full presentation is part of #1640 acceptance.

## Budgets and baseline updates

For every metric, both median and p90 must be at most
`max(baseline * 1.35, baseline + 25 ms)`. The edit-frame limit also has a
16 ms floor. The committed baseline is a ratcheted Linux CI budget, not a
claim of a timing delta between different physical machines. Before/after
performance claims require the same host and session.

The collector rejects incomplete workloads, empty traces and missing positive
controls. The analyzer rejects missing/extra metrics, wrong repetition counts,
invalid numbers and changed policy. The job compares a proposed baseline with
the protected `origin/main` baseline, so manually increasing values also fails.
Initial calibration evidence belongs in the introducing PR.

```sh
python3 scripts/perf/editor/budget.py check RUN/summary.json scripts/perf/editor/baseline.json --report RUN/report.json
python3 scripts/perf/editor/budget.py update scripts/perf/editor/baseline.json RUN/summary.json --output proposed-baseline.json
python3 scripts/perf/editor/budget.py ratchet scripts/perf/editor/baseline.json proposed-baseline.json
```

`update` refuses any increase, including p90-only relaxation, before touching
the output. It never silently removes a metric or changes the policy. Review
the raw traces and the calibration host before accepting a tighter baseline.
No environment variable overrides these limits.

## Noise protection and calibration

`gate.py` runs the complete collector once. A valid budget failure permits
exactly one fresh measurement of the **same binary**. Only two consecutive
budget failures make the job red. Both attempts and their median/p90 values
are retained in `result.json`, raw artifacts and `GITHUB_STEP_SUMMARY`.
Missing evidence, malformed data, a changed executable or a timeout fails
immediately; those failures never receive a retry or a false green.

Ordinary PR runs perform one gate invocation, with at most two measurements
within the shared 300-second deadline. For one-off calibration the same
workflow can be dispatched with `calibration_runs` between one and five.
These are independent complete gate invocations on one unchanged executable;
the first failed invocation ends the calibration sequence. This longer manual
calibration does not change the runtime of ordinary PR jobs.

The optional `artifact_run_id` reuses the trace executable from a successful
run of the **same exact commit**. The producer's head/conclusion, build receipt
and SHA-256 must agree before reuse. A different commit requires a new build.
This uses the existing job and runner, and makes repeated calibration possible
without rebuilding the executable for each observation.

Five consecutive green gate invocations and a forced regression were proven,
but a later run on unchanged application/benchmark code exceeded the input
budgets on both measurements. The single retry does not establish sufficient
noise protection across hosted runners. Baselines have not been relaxed.

The regression decision moved to the [same-runner A/B (#1662)](https://git.oklabs.uk/BeFeast/okilum/issues/1662) above.
It measures main and PR binaries in one job on the same runner, reports the CPU
model and vCPU count, and applies a relative delta plus an absolute ceiling.
That A/B is strict and non-required. This static guard remains a diagnostic.
Draft native checks remain red by the existing repository rule.

Exit status: 0 passes; 1 is a budget regression or rejected relaxation;
2 is invalid/missing evidence. The job uploads raw frame traces, application
diagnostics, per-repetition controls, samples, aggregate report and runtime.
