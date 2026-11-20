# Okilum merge queue

Forgejo has no native merge queue. This repository's queue is
`scripts/ci/merge-queue.py`. It is the only landing mechanism for Okilum
`main`. dev-pulse issue #32 describes a landing batch for Toroid; Okilum does
not keep a second batch beside this script.

## When a pull request enters

All of the following, on the current head:

- the pull request is open and not a draft;
- the latest trusted comment (`oklabs-bot`, `oleg`, or `BeFeast`) is
  `REVIEW PASS <sha>` or `CRITICAL-REVIEW PASS <sha>` for this head;
- the newest full `agent-checks` run for this head (branch
  `agent-checks/…`, not `agent-checks-fast/` or `agent-checks-benchmark/`)
  completed with success. That run's `linux` job executed
  `scripts/ci/pool-linux-gate.sh` on the build-slot pool.

A pull request labelled `critical` is taken before the others. Within that,
the oldest PASS goes first. Up to four clean pulls are merged into one tip.

## What a cycle does

1. If the only selected head already contains `main`, fast-forward `main` to
   that head. The green full run is the exact tree.
2. Otherwise create merge commits of `main` plus the selected heads, run
   `scripts/ci/pool-linux-gate.sh` on that tip through `build-slot`, and
   fast-forward `main` to the tip only when the suite exits 0 and `main` is
   still the tested base.
3. A non-zero suite ejects the last pull request in the batch, comments the
   failing phase, and does not retry that head against that base. Exit 75 or
   124 retries next cycle. A pull request that does not merge cleanly is left
   for the existing rebase action.

The push is `git push --force-with-lease=refs/heads/main:<tested-base>`. A
moved `main` refuses the push; the next cycle rebuilds.

## How it is started

A user timer on baldr runs one cycle at a time (flock). The unit files are
`scripts/ci/systemd/okilum-merge-queue.service` and `.timer`. The cycle checks
out this script from `origin/main` in a dedicated clone and calls `build-slot`
from maestro, which does not compile locally. While
`~/.local/state/okilum-merge-queue/enabled` exists, devctl does not merge
Okilum on its own; it leaves the pull request for this queue.

```bash
python3 scripts/ci/merge-queue.py once
```

Rollback: stop and disable the timer, remove the `enabled` file, and devctl's
critical merge path merges Okilum again. Ordinary pulls then wait for a person
to merge them, which is the previous behaviour.
