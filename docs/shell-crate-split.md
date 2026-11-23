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
