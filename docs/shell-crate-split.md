# okilum-shell crate split: budget, baseline and golden lists

Epic #3241 splits `okilum-shell` into `crates/okilum-shell-<area>` crates,
because the test harness of the `okilum` bin is a single `rustc` whose peak
memory grows with the crate and no longer fits an 8 GiB build slot. This page
holds the guard that stops the app crate from growing back, the recipe for the
heavy measurement, and the golden lists every split PR compares against.

## Size budget (fast gate)

`scripts/ci/shell-crate-budget.py` counts the physical lines of every `*.rs`
file under `crates/okilum-shell/src` (production and in-crate tests; both
compile into the harness) and fails when the count is above `BUDGET`. It runs
as the `Check okilum-shell size budget` phase of the pre-merge gate
(`scripts/ci/pool-pre-merge.sh`, `H=github scripts/agent/gh-check.sh --fast`).

- PR0 (#3232) set `BUDGET = 460_000` against 441 056 lines on main e81e9ba60:
  about 4 % headroom for the lanes that keep landing code until split PR1.
- Every split PR lowers `BUDGET` to the app crate's new size plus headroom.
- New code for an area that already has its own crate goes into that crate.
  Raising `BUDGET` needs the split lane's agreement.

## Baseline measurement (build queue, heavy run)

`scripts/ci/shell-split-baseline.sh OUT` measures, on Linux default features:

| Name | Command | What the peak is |
|---|---|---|
| `test-harness` | `cargo test -p okilum-shell --bin okilum --no-run --timings` | the `okilum` bin test harness `rustc`; dependencies and the lib harness are built before, unmeasured |
| `build` | `cargo build -p okilum-shell --timings` | the `okilum` bin `rustc` (and any dependency the non-test profile adds) |

Peak RSS is the largest `ru_maxrss` in cargo's process
tree (`getrusage(RUSAGE_CHILDREN)`, the figure `/usr/bin/time -f %M` gives),
so it sees `rustc` only when `rustc` is cargo's child. Run it with the
shared compile cache off:

```bash
mkdir -p "$qa/out"   # build-slot ships back only directories that exist
OKILUM_SOURCE_COMMIT=$(git rev-parse HEAD) \
okilum-slot run --label shell-split-baseline-<sha8> --mem 9 \
  --env BUILD_SLOT_SCCACHE=off --require-host okilum-build-nas \
  --source-git "$PWD" --receipt "$qa/receipt.json" \
  -- bash scripts/ci/shell-split-baseline.sh "$qa/out"
```

The script also writes `tests.txt` (every test of the lib and `okilum` bin
harnesses from `--list`, ignored ones included, sorted with duplicates kept)
and `actions.txt` (every name in the linked action registry, written by the
ignored `action_inventory_tests::dump_registered_action_names`; the
non-ignored control in the same module checks that an app, a feature and a
gpui-component action are registered).

## Golden lists

- `crates/okilum-shell/tests/fixtures/tests.txt`
- `crates/okilum-shell/tests/fixtures/actions.txt`

Both are Linux default-feature snapshots of their source commit (named in the
results below). Other lanes keep adding tests and actions, so a split PR does
not diff against the committed file directly: it runs the baseline on its own
base and on its head, checks that the head lists every test and action of the
base (module paths do not change in a move), and commits the head's lists.

## Results

### 2026-11-23, PR0 (#3232)

Source `58e0d2082` (main e81e9ba60 plus this PR's test-only action dump),
streamed with `--source-git`; build-queue run `issue-3232-baseline-58e0d208`
on `okilum-build-nas` (12 CPU, 16 GiB cgroup), `CARGO_BUILD_JOBS=4`, rustc
1.100.0, mold, `CARGO_PROFILE_{DEV,TEST}_DEBUG=line-tables-only` (the slot's
setting), `CARGO_INCREMENTAL=0`, sccache off, no OOM kill. 441 056 lines in
`crates/okilum-shell/src`.

| Measured | Peak RSS | Wall | Largest unit |
|---|---:|---:|---|
| `cargo test -p okilum-shell --bin okilum --no-run` | 8.77 GiB (9 197 032 KiB) | 177 s | `okilum` bin test harness 174.1 s |
| `cargo build -p okilum-shell` | 4.64 GiB (4 865 860 KiB) | 237 s | gpui-pre 106.8 s, `okilum` bin 73.0 s |

- The test-harness command compiled only the harness (dependencies and the lib
  harness were built before), so its 8.77 GiB is that single `rustc`. It sits
  above the 8 GiB slot cap and within ±5 % of the design model (8.5 GiB for
  441k lines) and of the 9.08 GiB peak seen on 23.11.
- The build command also compiled the non-test dependency graph (245 units);
  its peak is the larger of those and the `okilum` bin `rustc`.
- Golden lists: 3 499 test names (3 498 unique;
  `diagnostic_io::tests::closed_pipe_output_is_best_effort` is in both the lib
  and the bin harness) and 295 registered action names.
