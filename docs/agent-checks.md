# Agent build/test checks on GitHub

Start from main containing this workflow and helper; older worker branches must
rebase onto that main once before using GitHub checks. Commit the worktree, then
run from that worktree:

```bash
H=github scripts/agent/gh-check.sh
```

The helper pushes **only committed HEAD** to `agent-checks/<local-branch>` in
the public `BeFeast/okilum` mirror, using `gh auth git-credential`. Install `gh`,
authenticate with write access to the mirror (`repo` and `workflow` scopes for
a classic token), and keep the credential in the host's existing `gh` credential
store/keyring. No token is copied into Git URLs, source, the vault, or Actions.
The push uses an explicit lease so another worker's concurrent update is refused.
Use distinct local branch names for independent workers. The helper rejects a
dirty worktree and detached HEAD. Everything pushed is public: source and tests
must contain only public code and synthetic fixtures, never private/corporate data.

Only `agent-checks.yml` listens to this namespace. It creates no GitHub PR, tags,
release, signing or publication work. Forgejo remains primary; its existing main
and release-tag mirror is unchanged. Push the implementation branch to Forgejo
and open its PR there as usual. Before merge, require gh-check green on this
exact HEAD, then merge the PR yourself. Any later commit needs a new check.
Forgejo CI does not run on PRs; main has no required status checks. Forgejo
runs nightly and on workflow_dispatch; hourly Beta and pre-Stable certification
remain source-pinned. Main pushes only mirror source or publish releases.
Ordinary fast manual retries on one ref supersede older manual runs; each
certification dispatch has its own concurrency group and is never cancelled.

The standard public `ubuntu-24.04` runner has 4 vCPUs. The job records its CPU,
memory and exact source SHA, verifies the pinned Rust toolchain and the complete
gpui-kit/comrak patch trees, fetches public dependencies with the workspace lock,
then runs the scheduled shared gate's exact lint command,
`cargo clippy --workspace --all-targets --locked -- -D warnings`, builds the
Linux desktop/sync supervisor and runs the full `okilum-core`
suite and `scripts/ci/linux-rust-tests.sh` with Cargo offline. These are the same
Rust suites called by full Linux acceptance (`scripts/ci/check-linux.sh`); the
nightly acceptance Python/packaging checks retain their existing scope. AppImage process fixtures run serially so a
concurrent fork cannot temporarily retain another fixture's installation lock
before exec; all lifecycle assertions and test cases remain enabled. Both test shells raise their soft nofile limit via `test-nofile.sh`.
The Linux Rust suites retain their existing conditional vendor workspace tests.

Rust downloads, verified generated vendor trees and `target/` are cached with the toolchain, dependency/manifests,
vendor patches and workflow configuration in the key. Each source SHA can save a
fresh immutable archive; later commits restore a compatible archive from the same
branch. This namespace is separate from release caches. GitHub's repository cache
quota/eviction policy still applies; a missing cache is an ordinary cold build.
Job credentials have `contents: read` only, checkout does not persist credentials,
and no repository/organization secrets or private cache services are referenced.
Concurrent checks of a branch cancel the older run.

The helper blocks on **this SHA, workflow, branch and event**, prints the run URL,
per-step timestamps/conclusions and the last 100 failed log lines. Exit codes:
`0` pass, `1` code/check failure, `75` no verdict (API/auth, cancellation, timeout,
setup failure, missing checks, OOM/disk exhaustion). Retry infrastructure once.
Its bounded discovery waits and `gh run watch` run inside one helper process;
do not add thread polling or launch Cargo on the orchestration host.
Watch refreshes once per minute to leave API quota for concurrent workers.

```bash
# Repeat the identical source to measure warm cache (new attempt, same run).
scripts/agent/gh-check.sh --rerun
# Controlled cold/warm comparison on one runner VM (separate benchmark ref).
scripts/agent/gh-check.sh --benchmark --receipt /path/to/evidence.json
# Explicit dispatch on the pushed branch; workflow must already exist on main.
scripts/agent/gh-check.sh --dispatch
```

The total helper wait ceiling is two hours (`--timeout SECONDS`); the job ceiling
is 100 minutes. For GitHub outages or checks needing local hardware, use the
build-slot fallback recipe in the project operations notes.

`--benchmark` pushes to the sibling `agent-checks-benchmark/<sha>/<branch>` namespace
and repeats the build, core suite and Linux suites using the already warm
Cargo downloads/target on the same 4-vCPU VM. Each pass has separately timed steps;
each new head gets a cold branch cache scope. `--rerun` instead measures a new
hosted job including archive restoration; its VM hardware may differ.

For source-pinned editor publication measurements and a real main negative
control on the same hosted VM, supply the public main SHA explicitly:

```bash
H=github scripts/agent/gh-check.sh --dispatch --publication-baseline <full-main-sha>
```

This also runs the normal complete checks. The helper requires the comparison
step to succeed; missing/skipped comparison work is not a passing verdict.
The baseline compile must pass before its failing regression counts as evidence.

Each Linux Rust group now runs through the same `test-group.py` subreaper as
nightly/manual fast checks (#2510, #2592). A deadline prints
`TEST_GROUP_TIMEOUT group=<name> limit=<seconds>s`, fails the group, and terminates,
kills and reaps its descendants (including private D-Bus daemons) within another
30 s. Independent groups still run after a failure. The hosted Linux step has a
3000 s outer deadline as well; it cannot spend the entire 100-minute job on one
unresponsive test tree. Benchmark passes use the identical wrappers.

Initial sizing uses the timestamps from hosted run `41825902192` (4 vCPU), with
compilation included: core 206 s -> 480 s; local-writer 347 s -> 900 s;
AppImage 26 s -> 120 s; HTTP 32 s -> 180 s; web 3 s -> 60 s;
filtered groups up to 35 s -> 120 s. The full workspace group did **not** finish
in that run, so its last output is not a passing runtime sample. Its 1320 s
policy also reserves compilation/core overhead around the separate shell
execution policy from #2510 (990 s, including margin above its measured baldr
805 s execution). These are conservative deadline policies, not fleet percentiles
or cross-host speed comparisons. Sprint/no-default/portable/vendor groups retain
600/600/300/900 s budgets for their separate compilation/configuration paths.
New runtime evidence must justify adjusting a limit; a hung test does not.

The hosted controls include the real shared Linux entrypoint with a deliberately
hung, detached, TERM-resistant fixture. It must fail its named 2 s group, reap
every descendant within 32 s, and continue to the final independent suite.
`test_test_group.py` also exercises the unchanged #2510 supervisor directly.
`reveal-stress.py` runs the exact compiled shell test at least 20 times, requires
both its missing-manager assertion and fake-manager receipt, and checks that the
private daemon's cleanup ran. Each repetition has its own 20 s process deadline.
