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
suites run nightly and on workflow_dispatch; pre-Stable certification remains
source-pinned and on demand. Main pushes only mirror source and build the beta of
that exact commit for every OS.
Ordinary fast manual retries on one ref supersede older manual runs; each
certification dispatch has its own concurrency group and is never cancelled.

The standard public `ubuntu-24.04` runner has 4 vCPUs. This worker lane omits
dev/test debug symbols to keep the complete desktop and test targets within the
hosted runner's disk budget; assertions, lint and every suite remain enabled.
Release and native acceptance profiles are independent. The job records its CPU,
memory and exact source SHA, verifies the pinned Rust toolchain and the complete
gpui-kit/comrak patch trees, fetches public dependencies with the workspace lock,
then runs the scheduled shared gate's exact lint command,
`cargo clippy --workspace --all-targets --locked -- -D warnings`, builds the
Linux desktop/sync supervisor and runs the full `okilum-core`
suite and `scripts/ci/linux-rust-tests.sh` with Cargo offline. These are the same
Rust suites called by full Linux acceptance (`scripts/ci/check-linux.sh`); the
nightly acceptance packaging checks retain their existing scope. The agent job
also runs complete discovery of `scripts/ci`, `scripts/releases` and
`scripts/updater`, and the helper requires that step to pass. AppImage process fixtures run serially so a
concurrent fork cannot temporarily retain another fixture's installation lock
before exec; all lifecycle assertions and test cases remain enabled. Both test shells raise their soft nofile limit via `test-nofile.sh`.
The Linux Rust suites retain their existing conditional vendor workspace tests.

The complete workspace invocation isolates Reader Log and private reveal-bus cases
in separate serial processes; ordinary cases run with four test threads.
They share the product's process-wide log memory pool and async runtime: an
unrelated case retaining snapshots can correctly cause another small Follow
fixture to fail admission, and resource contention can leave the D-Bus timeout
control without its required receipt. This isolates scenarios without changing
the production pool, test inventory, assertions, operation deadlines or 1320 s
group deadline. Explicit concurrency within individual tests and the separate
20-run reveal stress/negative controls remain enabled. Hosted attempts of
`42373775429` recorded these two failures; the exact cause of the missing D-Bus
receipt was not established. Neither failed control was accepted as a passing
verdict.

`workspace-rust-tests.py` first lists every unit/integration/doc case for the
same four packages. Its three disjoint selections must report exactly that
inventory in their completed/ignored counts. Every Cargo invocation keeps all
four packages for the same feature unification and test environment. One
1320 s deadline covers the complete census and all three selections together.
This replaces whole-workspace serialization after run `42390453208` exhausted
that deadline; the deadline and every runtime assertion remain unchanged.

Every worker run also requires `windows-recovery` on a GitHub-hosted Windows
runner (#2551, #2700). It runs the existing native `okilum-core` `windows_` unit
and integration suites, including file publication, editor recovery and
protected history. Discovery must contain both the unacknowledged-save and
changed-parent queued-draft regressions before execution, so missing Windows
coverage cannot pass with zero tests. The helper requires both
jobs and their required steps; native test failures remain check failures.
Forgejo PR CI and native release/signing policy are unchanged.

The `forgejo-native-core-pr/**` Windows lane also runs the complete deleted-note
Reader group. Its #2700 controls reproduce the three old test failures at
runtime, restore the current tests and require a nonempty pass for each.
Recovery refusal uses a directory at the journal JSON destination while keeping
the held lock and its parent in place. The quick-file changed-parent test
requires authority refusal for the replacement and recovers the exact queued
draft after the original folder returns; it never implicitly re-attaches a
different folder.

Every job that discovers these Python suites, including local/hosted full gates
and the fast and Sync fixture lanes, runs `scripts/ci/setup-python-tests.sh`
first. It creates a fresh venv, installs `scripts/ci/requirements.txt` and adds
its executables to the Actions `GITHUB_PATH`. `PyYAML==6.0.3` is explicit;
host site-packages and pip configuration are excluded. Keeping the full YAML
parser preserves workflow expressions, flow collections, block scalars and
the existing BaseLoader/safe_load semantics without maintaining a partial
parser in our tests. A guard rejects missing or mismatched dependencies with
the installation command. Setup failure stops the job before discovery.

For local Python-only checks, start with a fresh venv (no host PyYAML), install
the same requirements and run all three suites:

```bash
test_env=$(mktemp -d)
python3 -m venv "$test_env"
"$test_env/bin/python3" -I -m pip --isolated install -r scripts/ci/requirements.txt
"$test_env/bin/python3" -I scripts/ci/python_dependencies.py
for suite in ci releases updater; do
  "$test_env/bin/python3" -I -m unittest discover -s "scripts/$suite" -p 'test_*.py'
done
```

Debian/Ubuntu jobs install `python3-venv` explicitly. Job-created environments
are temporary; Actions owns their lifetime.

Rust downloads, verified generated vendor trees and `target/` are cached with the toolchain, dependency/manifests,
vendor patches and workflow configuration in the key. Each source SHA can save a
fresh immutable archive; later commits restore a compatible archive from the same
branch. This namespace is separate from release caches. GitHub's repository cache
quota/eviction policy still applies; a missing cache is an ordinary cold build.
Job credentials have `contents: read` only, checkout does not persist credentials,
and no repository/organization secrets or private cache services are referenced.
Concurrent checks of a branch cancel the older run.

The Linux Sync fixture archive and Manager panel native gate's Sparkle archive
can fall back from a failed GitHub release download to the same release's
official asset API. Both paths must match the existing locked SHA256 before
staging; an incorrect checksum, API metadata response or missing primary URL
never becomes a passing dependency. This changes only CI archive transport.

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

Reveal's five-second D-Bus deadlines cover the entire connection/authentication
and ShowItems operations. zbus's `method_timeout` alone starts after a call is
sent and does not cover the authentication handshake. The private test bus has
the same connection deadline, a five-second whole-test bound, and bounded startup
address collection. Its startup address goes to a temporary regular file: pipe
EOF or a reader-thread join cannot retain the test when another process fixture
inherits a writer before exec. The child guard is installed before startup
checks and always kills/waits the daemon, including assertion/timeout unwinding.
The missing-manager assertion requires ServiceUnknown, so a timeout cannot
masquerade as the expected no-manager error.

Real-socket controls cover an unanswered AUTH and a manager that receives the
exact item URI but withholds its reply. Hosted run `41845622687` verifies that
removing only the shared deadline causes both controls to hang until the external
supervisor returns 124; with it, both return in five seconds. The isolated original
modules passed 50 hosted repetitions (`41844867137`), so lack of an installed
manager alone does not identify the historical full-suite stall. Keep that
historical trigger distinct from the controlled, reproduced indefinite waits.

Hosted run `41845308559` exhausted the stock image's disk during shell-test
compilation, before any stress verdict (helper 75). The lane now records free
space and removes only the image's unused Android/.NET/CodeQL SDKs before cache
restoration. Rust, system libraries, fixtures and test inventories are retained.

The full-shell baseline trace (`41844245238`) also completed every reveal stage:
missing-manager error at 14:40:37.142Z, fake-manager receipt at 14:40:37.147Z and
normal test completion at 14:40:37.150Z. The diagnostic run was then cancelled
intentionally to collect its trace. The historical trigger was not reproduced;
only the indefinite authentication/reply waits were reproduced by controlled
stalls. This distinction is retained in the evidence rather than attributing the
old failure to a file manager that was absent from the isolated bus.

Main's #2595 isolated Python setup and complete CI/releases/updater discoveries
remain required. Their groups reuse the same supervisor at 300/90/90 s; the
observed local complete CI discovery was 136 s, and the releases/updater samples
were about 3 s/0.04 s. These limits retain setup/scheduling margin rather than
asserting a cross-host percentile. Main's #2589 first-computer and direct-LAN
commands are retained as separately supervised 180/120 s groups, including
compilation headroom. Final hosted group receipts provide their runner timings.

Integrated gate validation also keeps the #2595 dependency guard aware of the
#2510 Sync wrapper. Sync staging's concurrent exec probes use independent
publisher processes, matching the production CLI path: a ThreadPool fork can
inherit a sibling's temporary writable FD until exec and cause ETXTBSY after the
parent has closed and renamed it. A dedicated positive control demonstrates that
kernel behavior. Eight cold publishers and four live-daemon publishers still
must all publish/exec successfully; checksum, inode, cleanup and old-tar negative
controls remain. Twenty repeated eight-process cold controls passed locally.

Mac Sync source changes also use the same exact-source helper with native
GitHub-hosted macOS compilation and non-empty source/Settings, signed-peer,
exit-watch and native connection controls:

```bash
H=github scripts/agent/gh-check.sh --dispatch --macos-sync
```

This requires the `macos-sync` job and every named native step in addition to
Linux and Windows. Missing/skipped native work cannot pass a requested Mac run.
Without that option the optional Mac job stays skipped. The lane installs no
app and registers no Sync helper. It ad-hoc signs only disposable native peer
fixtures; it performs no release signing/publication and carries no secrets.
Native compile/tests do not certify first pairing through the signed beta UI;
record that installation/build, both-direction file evidence and the short video
separately for #2489.

Sprint cache controls run with `python3 scripts/dev/test-sprint-report.py` in
the hosted Python gate. They execute the embedded CLI on disposable trees,
including symlink/outside-path and build-lock controls. The separate
`sprint-cache-native/**` refs repeat them on hosted macOS, with a real allocated
`du` control; they never build, sign or install an app.

The local Mac Sprint script defaults to a 20 GiB target cap, configurable with
`OKILUM_SPRINT_TARGET_CAP_GIB`. Under its existing `build.lock`, it removes the
Mac Sprint incremental cache first, then that profile if needed, before Cargo
and after bundle staging. Other target profiles remain untouched; excess there
stops the build. It reports target size and volume free space, warns below
50 GiB, and preserves a caller's `CARGO_INCREMENTAL` (default `1`).
Existing build-script cache repair compares canonical paths, so macOS aliases
such as `/var` and `/private/var` preserve valid warm outputs.

The required `Verify per-user AppImage runtime` step rebuilds checksum-pinned
upstream type2-runtime source in its static Alpine Docker environment, then
applies `scripts/appimage/runtime-per-user.diff`. Real SquashFS process controls
run as two different Linux UIDs. The unchanged runtime must reproduce the
foreign-cache extraction failure; the patched runtime must launch, re-exec and
restart both copies through both extraction entry forms, clean up only its own
cache and preserve the other user's tree. This runs before Rust compilation and
does not install an app or certify the official beta GUI flow.
