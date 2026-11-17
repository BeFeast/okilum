# Stable speed budget (#2094)

Oleg's budget of 31.10: **cold start < 300 ms, typing < 16 ms, open a 10 MB note
< 1 s, search 10,000 notes < 100 ms**. This directory measures all four on one
exact source and reports them. It is **report-only**: nothing here blocks
Stable promotion. The `enforce` switch exists, is off by default, and turning
it on for Stable is Oleg's decision.

## Run it on a Stable candidate

```sh
gh workflow run stable-speed.yml -R BeFeast/okilum --ref main -f source=<candidate sha>
```

`.github/workflows/stable-speed.yml` checks out the candidate, builds the shipped
release binary (`cargo build --release --locked -p okilum-shell --bin okilum`,
no measurement patch), runs the search probe, then the app probe under Xvfb,
and writes the verdict table to the job summary. Artifacts (summary, verdict,
per-launch samples, diagnostic logs) are kept 30 days. A source that predates
this harness fails at once with `predates the speed budget harness`.
Agents can measure a branch head by pushing it to `stable-speed/<name>` on the
GitHub mirror. `-f enforce=true` makes a hosted-ceiling FAIL exit 1.

## What each number is

All app metrics reuse the #1639/#1662 editor probe (`../editor/probe.py`,
`run_once`): an isolated home, state and vault per launch, the product's
post-frame diagnostic stamped with Linux `CLOCK_MONOTONIC`, which Python's
`monotonic_ns` shares. Six interleaved rounds; round 0 is a warmup; the median
of the other five is judged.

| metric | boundary | positive controls |
|---|---|---|
| `cold_start_ms` | process launch → first painted Source content frame of `tiny.md` (3 KiB), fresh profile, OS file cache warm | painted frame of the exact document; one visible window |
| `typing_ms` | one key (`x` at the start) → post-frame of the matching document generation; includes X11 injection and queue time | the copied buffer holds the edit; window pixels changed (checked outside the interval) |
| `open_10mib_ms` | launch → first painted frame of `ten-mib.md` (10,485,760 bytes, 10,240 sections), **minus** the same round's tiny launch, so the shared startup cancels | painted frame of the exact document; one visible window |
| `search_10k_ms` | slowest of five queries' median over 30 timed calls: the palette's `search_in_paths` over all 10,000 owned notes, limit 100, rows with snippets | exact hit counts, every row has a snippet, `path:` rows in scope, unique term finds its note |

`search_10k_ms` comes from `synthetic_10k_query_latency` in
`crates/okilum-core/tests/search_bench.rs` (release profile): a 10,000-note,
~33 MB vault written to disk, scanned and indexed through the product path.
No UI paint is included. The absolute 10 MiB open (launch → frame) is kept in
the summary as `open_10mib_ms.absolute`.

`notes.py` reuses the editor generator; `manifest.json` pins both notes and the
collector refuses drift. The 10 MiB launch may take up to 60 s to its first
frame (the editor probe's default wait is 12 s), so a slow open is a measured
over-budget value, not missing evidence. A failed control makes that metric `NO_EVIDENCE`
(exit 2 in every mode, never a pass); the other metrics are still measured.

## Two verdicts per metric

- **product**: the strict 31.10 budget. It belongs to the reference machines
  (Linux muninn, Mac M4, Windows VM 150). A 4-vCPU hosted runner drawing with
  Xvfb's software rasteriser is not one, so here it is shown and never enforced.
- **hosted ceiling** (`budgets.json`): a regression ceiling for the
  `ubuntu-24.04` hosted runner, calibrated from measured runs (numbers and
  margins in the PR that set them). Only this can make the result `FAIL`.

The boundary is the Xvfb software post-frame: neither GPU completion nor
scanout. Reference-machine series (muninn, M4 night window, VM 150, and the
Hedva slow-machine series queued on #2094) are follow-up work on the same
collector; the Reader-mode 10 MiB open lands with #653's Reader probe.

## Files

| file | what |
|---|---|
| `collect.py` | runs the app rounds, reads the search JSON, writes `summary.json` |
| `judge.py` | verdict, Markdown table, exit code (`--enforce`) |
| `budgets.json` | product budgets and hosted ceilings |
| `notes.py`, `manifest.json` | the pinned notes |
| `test_speed.py` | verdict, pairing, controls and pin tests |
