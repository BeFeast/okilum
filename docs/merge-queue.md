# Okilum merge queue

Forgejo has no native merge queue. This repository's queue is
`scripts/ci/merge-queue.py`. It is the only landing mechanism for Okilum
`main`. dev-pulse issue #32 describes a landing batch for Toroid; Okilum does
not keep a second batch beside this script.

## When a pull request enters

All of the following, on the current head:

- the pull request is open, not a draft, and its base is `main`;
- the latest trusted comment (`oklabs-bot`, `oleg`, or `BeFeast`) on any
  comment page is `REVIEW PASS <sha>` or `CRITICAL-REVIEW PASS <sha>` for
  this head;
- `scripts/agent/gh_check.py` returns exit 0 for the newest full
  `agent-checks` run of this head (branch `agent-checks/…`, not
  `agent-checks-fast/` or `agent-checks-benchmark/`). The newest run is
  chosen from the list before its jobs are loaded. If that read fails, the
  pull request does not enter and an older success is not used. A skipped
  `linux` job is not green. That job executed
  `scripts/ci/pool-linux-gate.sh` on the build-slot pool.

A pull request labelled `critical` is taken before the others. Within that,
the oldest PASS goes first. Up to four clean pulls are merged into one tip.

## What a cycle does

1. If the only selected head already contains `main`, fast-forward `main` to
   that head. The green full run is the exact tree.
2. Otherwise create merge commits of `main` plus the selected heads, run
   `scripts/ci/pool-linux-gate.sh` on that tip through `build-slot` on baldr
   (the suite builds an AppImage runtime image, and only baldr has Docker), and
   fast-forward `main` to the tip only when the suite exits 0 and `main` is
   still the tested base.
3. A non-zero suite ejects the last pull request in the batch, comments the
   failing phase, and does not retry that head against that base. Exit 75 or
   124 retries next cycle. A pull request that does not merge cleanly is left
   for the existing rebase action.

`main` does not accept a direct push. After the suite is green and the
review is still a cross-family PASS, the queue lands with
`POST /pulls/{n}/merge` and `head_commit_id` (`fast-forward-only` when the
head already contains `main`, otherwise `merge`). It writes the enabled
flag only after that landing. A head already contained in `main` is skipped.
A red batch is run per PR, and only a PR that fails alone is ejected. A
phase exit 124 is a code verdict. build-slot 75/124 with no failing phase,
and `oom_kill > 0`, retry up to 3 times for the same heads and base, then
the PR is skipped with a comment.

The pool source is a `git archive` (no `.git`). Each gate runs
`pool_ensure_git` before `check-vendor-generation.py`, which needs
`git ls-files`. The phase receipt is shipped from
`/home/god/scratch/okilum-pool-receipts`, a path `build-slot` copies back.

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
