# Linux editor latency guard (#1639)

The existing `perf-linux` job reuses its trace binary and Xvfb display. The
editor collector has a 300-second deadline and adds no runner or second build.
The existing Reader scroll growth guard and its thresholds remain required.

`fixtures.py` generates original prose, with pinned SHA-256 hashes: 3 KiB,
130,000 bytes with 460 heading sections, exactly 1 MiB, and 1,200,000 characters.
It needs no network or downloaded book. Each run uses a fresh, isolated vault,
application state and recovery directory; installed applications are not used.

## What is measured

Each Source scenario and the small Live Preview scenario has one warmup followed
by five independent launches. The report gives the median and nearest-rank p90
of the five measured repetitions. With five observations p90 is their maximum.

- `open_first_ms`: process launch to the first Source editor content frame.
  This includes startup and opening the note, rather than ending at loading UI.
- `input_frame_ms`: immediately before injecting the sparse key to the
  post-frame diagnostic for its exact document and source generation. This
  includes injection and OS queue delay. A changed clipboard buffer and window
  pixels independently prove the input was exercised.
- `open_full_ms`: small fixture only, process launch to a frame after the
  matching complete Live Preview provider was accepted. A limited provider is
  never a successful sample. Fixtures have no asynchronous media.
- `scroll_frame_ms`: each repetition's p90 CPU `draw` cost during its wheel
  burst, using the existing trace fields. Software-rasterizer `present` cost
  is excluded. At least five redraws, changed window pixels and a nonzero
  editor scroll offset are mandatory.

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

Exit status: 0 passes; 1 is a budget regression or rejected relaxation;
2 is invalid/missing evidence. The job uploads raw frame traces, application
diagnostics, per-repetition controls, samples, aggregate report and runtime.
