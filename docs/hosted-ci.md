# Run a commit in CI

Heavy PR checks run on standard GitHub-hosted runners in the public
`BeFeast/tessera` mirror. Executors do not need to compile on maestro. Source
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

For a runnable Linux QA build, dispatch `commit-ci` with `lane=linux-binary` (`baseline=true` also builds a pinned main snapshot); on muninn download its `linux-binary-<sha>` artifact with `gh run download <run> -R BeFeast/tessera -n linux-binary-<sha>`, verify `sha256sum -c *.tar.zst.sha256`, unpack with `tar --zstd -xf <archive>`, enter the payload directory, verify `sha256sum -c SHA256SUMS` and `SOURCE_SHA`, then run `(flock -w 2700 9 && xvfb-run -a ./tessera --vault /path/to/qa-vault 9>&-) 9>/tmp/tessera-gui-qa.lock` (omit `xvfb-run -a` for a visible session); artifacts expire after three days and require compatible Linux x86_64 runtime libraries.

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

Linux PRs use `TESSERA_LINUX_LANE=hosted` permanently. To explicitly fall back,
set that variable to `local`; set `TESSERA_PR_LANE=local` for the supplemental
Brain/Inbox/Arch/Windows PR jobs. Neither changes running jobs. Same-repository
PRs use hosted bridges; untrusted forks keep the isolated local fallback and
never execute on the bridge host with a mirror token. Existing local main,
nightly, manual release, publish and signing workflows remain unchanged.

The organization budget is up to 20 simultaneous standard jobs, shared across
repositories and macOS/Windows, with a separate macOS cap. This is a capacity
limit, not a promise of immediate starts. Windows packaging uses a short Linux
icon job followed by one `windows-latest` compiler job. Other selected lanes
use one job each. Branch pushes run Linux only; request additional lanes when
needed. Linux/Brain consume the existing main cache; new lanes do not save
large per-branch target caches into the shared 10 GiB quota. Missing caches
fall back to cold builds. Capacity incidents are reported, not retried in a loop.

Bridge cancellation: SIGINT/SIGTERM (including a superseded Forgejo job) requests
cancellation of only that invocation's GitHub run. The bridge rechecks the run ID,
unique ref, workflow and source SHA before POSTing; a replacement invocation is
never selected by PR number alone. Cleanup uses one-second HTTP timeouts, no
mutation retries, and at most three discovery reads if cancellation races with
push/run discovery. Logs distinguish accepted cancellation, already-completed
runs and unconfirmed cleanup. SIGKILL, host loss, delayed run discovery or an API
outage can still leave an orphan; report its run URL to the CI owner rather than
retrying the build. Ordinary queue timeout/cancel refusal retains the existing
wait-for-the-same-run behavior. Rollback: revert the bridge cancellation commit;
required gates and exact-source validation remain unchanged.
