# Run a commit in CI

Heavy PR checks run on standard GitHub-hosted runners in the public
`BeFeast/okilum` mirror. Executors do not need to compile on maestro. Source
and logs must be suitable for that public repository; no signing or private
cache credentials go to GitHub.

1. Rebase on current Forgejo main (the commit must contain these workflows).
2. Push your exact commit to Forgejo as `ci/<issue>-<purpose>`. The `commit-ci`
   workflow automatically runs the full Linux gate and reports its result on
   that commit. Alternatively push a normal branch and open a draft PR: the
   existing PR gates run, including path-selected platform release checks.
3. For Windows tests or unsigned packaging, manually run Forgejo **commit-ci**
   on the same branch, selecting `native-core` (core `windows_` tests),
   `native-sync` (sync-controller `sidecar::` tests), `windows-release`, `arch`,
   `brain`, or `inbox`. `linux` is the default. Native tests use default features,
   `--nocapture --test-threads=1` on Windows 2022. For a different package/filter,
   owners use exact-source `windows-native.yml` dispatch; CI owner handles lane failures.

For Linux QA, select `lane=linux-binary`; `baseline=true` separately pins and
resolves main. It waits for the existing required producer or reuses its accepted
artifact. It cannot compile or dispatch a replacement for a missing payload. The
Forgejo job saves the exact executable, production receipt, JSONL, and (while
hosted) checksummed tar as `linux-production-receipt-<job>`. Native QA still uses
the existing owner/lock; downloading is not QA completion.

To resolve from an exact checked-out source on a QA machine, provide the existing
mirror credential as `MIRROR_TOKEN` and run:

```sh
PR_HEAD_SHA="$(git rev-parse HEAD)" python3 scripts/ci/github-linux.py resolve
```

This discovers the full SHA/config identity, verifies the successful full gate,
producer/run attempt, artifact lifetime/linkage, immutable ZIP digest, outer tar
checksum, complete internal checksums and production manifest before extracting.
The bridge validates ELF identity and the producer's executed build-info receipt
without launching the app. Arch repeats build-info/ELF/runtime validation before
packaging. Run native QA on the verified `dist/linux-production/payload/okilum`
through the existing `/tmp/okilum-gui-qa.lock`, preserving time spent waiting.
The old `forgejo-linux-binary` workflow is a compatibility consumer with no Cargo.

The identity is full source SHA + `x86_64-unknown-linux-gnu` + release/default
`brain` + no harness/test-support + a configuration digest covering the pinned
inputs and producer code. The app-only Cargo invocation follows the unchanged
full gate. Build version is **5000 + originating Forgejo run number**, carried in
the temporary ref; GitHub run numbers and later packaging runs never renumber it.
PR-head bytes cannot be relabelled as merge-SHA bytes. Repeated accepted requests
reuse evidence; active requests wait. Simultaneous cold requests are serialized
by the existing workflow's SHA lock and recheck acceptance before compiling.
If an attempted producer has no valid artifact, recovery requires an explicit
investigation; there is no cache-miss compiler fallback. A cold source must be
less than three days old (and not future-dated by more than five minutes). Older
cold identities are unavailable rather than rebuilt after CI-history retention;
accepted hosted or signed archive payloads are resolved before this age check. GitHub can cancel excess
pending concurrency requests; such a request fails, never counts as success.

Hosted production artifacts expire after three days. Once packaged, the existing
signed Arch archive retains the exact executable and original producer receipt;
the resolver can verify/reuse those bytes after hosted expiry. Legacy archives
remain valid for their existing promotion path; they cannot masquerade as the new
full-source schema. Unarchived expired PR artifacts are unavailable.

`delivery.jsonl` and `receipt.json` are outside Cargo/index caches and saved as
workflow evidence. They retain request/observed queue times, API creation/job/step
times, production build start/end, verification and the original build identity.
QA and Stable availability remain null until their owners measure them. Append
measured QA or availability facts through the same helper, for example:

```sh
PR_HEAD_SHA=<full-sha> OKILUM_ARTIFACT_DIR=<existing-evidence-directory> \
  python3 scripts/ci/github-linux.py record --event qa_waiting \
  --facts '{"occurred_at":"<actual-UTC-time>","reason":"existing native QA slot"}'
```

Use `task_started`, `forgejo_queued`, `qa_requested`, `qa_waiting`, `qa_started`,
`qa_completed`, `promotion_waiting_for_go`, and `available_verified` as applicable;
record known actual timestamps in `--facts`, retaining null for unknowns. Upload
is not Stable availability. The approximate 60-minute delivery target includes
all queue/QA/GO waiting and has not been established by these implementation tests.

4. Read the linked GitHub run in the Forgejo job log. Check the run SHA and
   named tests; a green job with zero matches is not acceptance evidence.
   Unsigned release artifacts are available on that GitHub run for three days.
   A newer source commit needs its own evidence when the tested crate,
   dependencies, lockfile, toolchain or workflow changed.

The bridge pushes an isolated temporary ref and accepts only that exact SHA,
workflow, job and executed acceptance step. Missing mirror source, skipped
steps, capacity timeouts and network errors fail closed. The ref is deleted
when the bridge finishes; an interrupted coordinator can leave a ref for CI
owner cleanup. There is no automatic dispatch retry. Branch CI is evidence,
not a replacement for the unchanged required `ci/check` PR aggregate.

Trusted same-repository PRs and actual main merge commits now use the same hosted
Linux producer. `OKILUM_LINUX_LANE` no longer sends trusted shipping identities to
a second compiler lane. Required `ci/linux` and `ci/check` names are unchanged;
main-health follows the Linux aggregate and reads failed hosted/local lane logs.
Forks keep the isolated local full gate, never receive a mirror credential and
cannot produce trusted publication artifacts. `OKILUM_PR_LANE=local` still selects
local supplemental jobs; trusted Arch jobs resolve the same production bytes.
Signing, publication locks and promotion entrypoints are unchanged.

The organization budget is up to 20 simultaneous standard jobs, shared across
repositories and macOS/Windows, with a separate macOS cap. This is a capacity
limit, not a promise of immediate starts. Windows packaging uses a short Linux
icon job followed by one `windows-latest` compiler job. Other selected lanes
use one job each. Branch pushes run Linux only; request additional lanes when
needed. Linux/Brain consume the existing main cache; new lanes do not save
large per-branch target caches into the shared 10 GiB quota. Missing dependency caches are normal for the canonical producer; missing
production artifacts do not permit a consumer build. Capacity incidents are reported, not retried in a loop.

## Logs of failed jobs

Logs of this repository's jobs are public; no token is needed. The usual trap is
the run number: the number in a run's web URL (`/actions/runs/6123`) is not the
API run id, so `GET /actions/runs/6123/jobs` reads another run or nothing.

- **One run:** `python3 scripts/ci/ci-logs.py get <run URL or number> [job]`
  prints the failed jobs' logs (or the named job). It resolves the number first.
- **Every failure, already saved (maestro):** `~/.cache/okilum-qa/ci-logs/`.
  A user timer (`okilum-ci-logs.timer`, every 5 minutes) runs
  `ci-logs.py collect` into it from `~/.cache/okilum-qa/tools/ci-logs.py`:
  - `<run>-<job>.log` is the Forgejo job log;
  - `<run>-<job>.github.log` holds the failed steps of a hosted lane's GitHub run
    (`gh run view --log-failed`);
  - `index.tsv` has one line per failed job: time, run, job, workflow, event,
    branch, sha, summary, links. The summary names failing tests or the first
    compiler error. It also says when the job only aggregates others (`linux`,
    `macos`, `check`), ran on GitHub, or produced no build or test output
    (likely infrastructure).

  Files are kept for 30 days. Start with `column -t -s$'\t' index.tsv | tail`.
