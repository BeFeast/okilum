# Same-runner input A/B (#1662)

The report-only, scope-aware `perf-linux` job pins main once and builds main and
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

Only input medians determine the A/B result. A scenario fails when the PR is
worse than main **both by more than 15% and by more than 3 ms**. These are
**configurable defaults**, stored once in `ab-policy.json`; no environment
variable silently changes them. p90 and all raw samples remain in the report.
A valid regression permits one complete paired remeasurement of the same two
SHA-verified binaries. Infrastructure/missing evidence never retries. Both
attempts share the configured 590-second deadline.

There is **no absolute CI latency ceiling**. The product's app-key-event to
post-frame limits (33 ms Linux / 16 ms M4) belong in acceptance on real hardware,
not on bimodal hosted CPUs. The old static baseline is retained as historical
evidence; A/B does not rewrite or relax it.

Scroll is excluded from A/B. The legacy scroll diagnostic continues separately
with a **measurement build, not shipped**, using the existing trace patch. Its
result and provenance remain visible and report-only. Neither an absent scroll
trace nor a scroll failure is represented as input regression.

Required activation is **deferred**, and #1662 remains open. EPYC calibration
run `40223917116` produced five valid green invocations, and forced-red run
`40220173487` detected the diagnostic 120 ms delay. However, Xeon run
`40230405638` produced two valid FAIL attempts with identical main/PR product
input hashes. The source of this unchanged-product false rejection is unknown.
Hardware, both failures and all positive controls/provenance remain in reports.

Ordinary CI invokes `ab.py --report-only`: performance FAIL stays FAIL in JSON
and the job summary (including both attempts), while the process exits zero.
Missing controls, collector/harness failures and binary/provenance integrity
errors remain `NO_EVIDENCE` with nonzero exit; they are never hidden as PASS.
`ci / check` does not depend on perf; existing shared/macOS requirements remain.
Multi-pass calibration keeps strict exits and the identical-product-input
hash guard, stopping on the first failed invocation. No automatic calibration
loop or runner selection is performed. Defaults 15% / 3 ms and one paired
remeasurement are unchanged. A future activation needs its own decision and
resolution of the Xeon blocker, not a waiver for future perf failures.

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

Required activation is deferred to [same-runner A/B #1662](https://git.oklabs.uk/BeFeast/okilum/issues/1662),
owned by Editing after #1640: measure main
and PR binaries in one job on the same runner, report CPU model/vCPU count, and
use a relative delta plus an absolute ceiling. Activation requires at least
five green unchanged-code runs and a forced-red commit in that A/B mode.
Draft native checks remain red by the existing repository rule.

Exit status: 0 passes; 1 is a budget regression or rejected relaxation;
2 is invalid/missing evidence. The job uploads raw frame traces, application
diagnostics, per-repetition controls, samples, aggregate report and runtime.
