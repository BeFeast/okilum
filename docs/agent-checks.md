# Agent build/test checks on GitHub

## When which check runs (Oleg 13.11 ~14:20, ~15:10)

- **Before merge:** formatting, script checks and a Linux workspace compile,
  about 10–15 minutes:

  ```bash
  H=github scripts/agent/gh-check.sh --fast
  ```

  It pushes to `agent-checks-fast/<branch>`, where only the `pre-merge` job
  runs: pinned vendor/comrak verification, `cargo fmt --check`, the helper,
  CI, releases, updater, Syncthing and Sprint Python suites, Sync Help and
  brand, and `cargo clippy --workspace --all-targets --locked -- -D warnings`
  with its own warm cache. No Rust test suites, no desktop build, no Windows
  or macOS job. A pre-merge verdict never counts as a full one.
- **Beta** is built hourly from the newest main as it is; no check gates it.
  If main does not build, that hour's beta is skipped and the offending PR is
  reverted or fixed within the hour.
- **The full set** (`gh-check.sh` without `--fast`, plus the Forgejo full
  gate) runs once a day before Stable is built, and gates Stable only.
- A compile or test run may still be started in the background for
  information; nothing waits for it.
- **Caches** (#2880, #2881): only runs on `main` save Cargo caches; branch
  runs restore main's and save nothing, so they cannot evict main's caches
  from the 10 GB repository quota. A daily scheduled `agent-checks` run on
  `main` (22:30 UTC) runs `pre-merge`, `linux` and `windows-recovery` only to
  warm those caches; it gates nothing.

## The full set

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
and open its PR there as usual. Before merge, require `gh-check.sh --fast`
green on this exact HEAD; who may merge which PR is in `AGENTS.md`.
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

The canonical local Linux and shared gates use the same dev/test debug-info
setting (`0`) as these required hosted checks. Run `26004` on `67e1d82b`
exhausted the local-writer group's 900 s budget after roughly 880 s of cold
compilation with `line-tables-only`; its last review group was interrupted.
Omitting debug info reduces compiler/linker work without disabling debug
assertions or changing the test selection. Panic assertion locations remain;
CI backtraces no longer include DWARF line tables. The existing 900 s budget
still includes compilation and every admission/review group. No prewarming,
separate compile allowance, release-profile change or deadline increase is used.

The full hosted native Mac gate runs its production Reader/Sparkle lifecycle
and workspace release tests in independent jobs on the same source. Both jobs
are mandatory for workflow success. Cold native run `42965153887` passed in
67 minutes, but its canonical Forgejo wrapper had already been cancelled at
the runner's 60-minute maximum. With no native cache, controller compilation
used 4m26s, the production app 17m35s and workspace test compilation 28m34s.
Overlapping production and test work removes that serial dependency; neither
the release profile, production build, Sparkle controls, native test selection
nor existing deadlines change. `check-macos.sh` still defaults to the complete
serial gate for local/other callers. Its orchestration control executes all
three modes with synthetic tools and requires the split jobs to retain every
original command; the old script fails the split-mode assertions.

Canonical run `26270` on `f3d8876b1` exposed the same Log memory-pool
contention in the shared gate: its Follow backlog control correctly refused
admission while other GUI fixtures retained the pool. `check-shared.sh` keeps
its full workspace selection and existing `platform::` exclusion, but runs
ordinary cases and the complete `reader_log` selection in separate processes.
Log runs serially; neither its production memory budget nor its assertions
change. Both processes must pass and later independent suites still run after
either fails.

The compile-inclusive local-writer admission keeps all seven original groups
and the existing 900-second deadline. Every invocation now selects the same
four packages (write-authority, core, shell and T3), instead of changing the
package feature closure for each group. Run `26270` interrupted the initial
shell test build after earlier single-package builds; subsequent 120/300s
filters repeatedly started the unfinished build until the runner cap. There
is no prewarming or separate compile allowance. Each group also requires a
nonzero passing *parent* footer from its original Cargo target: another
package's successful footer, a child fixture's earlier result, a missing
target or an empty parent cannot satisfy it. All original filters, integration
targets, test bodies and profile/deadline settings remain enabled.

Later default-feature Linux filters keep these same four roots alongside their
original selected package and target. Canonical26469 passed cold admission in
509 seconds, then rebuilt the core dependency graph for the single-package
Sync controller and rebuilt the GUI for single-package Settings/Feedback,
exhausting the existing120s/300s groups before their tests. Keeping admission's
feature union avoids that repeated compilation without adding a precompile or
moving work outside the original deadlines. AppImage all-features, HTTP updates,
Sprint and portable/no-default checks retain their separate root/feature policy.

The write-authority and T3 admission queries use `--lib`, matching their
original target scope. Selecting four packages without that flag also asks
Cargo to compile unrelated core integration harnesses, even though their
filters run zero cases. The explicit core integration group and shell binary
groups remain separate and mandatory.

Canonical `shared-local` and `linux-local` compilation uses the existing
`heavy` runner label. The generic `ubuntu-latest` label also matches light and
interactive pools, so it does not specify sufficient compile resources.
The cold diagnostic reproduced the original 900-second timeout on the same
light runner as failed canonical main: a 5 GiB memory limit, shell test rustc
at the limit, repeated memory-limit events and substantial I/O stalls. A
same-job pair on the registered heavy runner completed all seven original
groups from separate cold targets on both baseline and candidate. This is
evidence for explicit workload routing, not a reason to relax deadlines or
change host configuration. A scheduler eligibility regression rejects either
compile job or the diagnostic becoming eligible for light/interactive pools.

`cold-writer-diagnostic.yml` is a manual, source-pinned measurement on the
same `heavy` runner class as canonical Linux. It checks out diagnostic
tools and the measured source separately. The measured checkout begins without
a target cache and reproduces the observed unavailable compiler wrapper. Its
original `check-linux.sh` prelude runs unchanged; a source-directory-qualified
shell boundary replaces only the transition to `linux-rust-tests.sh` with
that source's original first local-writer group under the existing 900-second
supervisor. A guarded `BASH_ENV` function exists only in the top-level script;
PATH still resolves the real Bash executable, preserving the original restart
fixture's executable identity. Child shells retain ordinary Bash dispatch.
Subsequent filters do not repeatedly restart a timed-out build.
No product compilation is inserted outside its original prelude or admission.
A missing boundary invocation fails even if the prelude exits successfully.

Five-second owned-process command lines and CPU/RSS/I/O plus cgroup
quota, throttling, memory and pressure samples go directly to the job log,
including a terminal snapshot. Cargo keeps its original verbosity and target
headers so the measured source's strict parent-target guards remain effective.
Compiler/linker versions, source identities,
affinity and configured jobs are recorded; unset jobs is not labelled as a
measured concurrency count. This is a diagnostic, not full Linux acceptance,
and neither a warmed hosted pass nor a successful probe establishes canonical
main GREEN. The original baseline source can be measured while tools contain
a candidate scope correction; receipts distinguish those two commits.
An optional exact `candidate` SHA runs after the baseline on the same job's
runner, with a separate checkout and cold target directory. Each source runs
its own unchanged prelude and compile-inclusive 900-second admission. Baseline
failure does not prevent observing the candidate, but the final diagnostic
retains both outcomes and fails if either measurement failed. The job's
60-minute cap stays unchanged. Large process samples use ordered JSON chunks
to survive the runner's log-line limit without discarding compiler arguments.

For runtime diagnosis, `runtime: 'true'` (without a paired candidate) runs the
two warm-reader fixtures, Live Preview, and the complete workspace after a
successful original cold admission. Target fixtures retain positive parent
receipts; the original 120/1320-second group budgets stay in effect. Every
runtime stage runs even after an earlier runtime failure, and the final verdict
retains that failure. This targeted diagnostic is not full canonical acceptance.
`runtime: 'shared'` instead executes the complete original `check-shared.sh`
entrypoint, including formatting, lint, portable reader, Python and manifest
gates. The diagnostic's existing job cap remains in effect.

The heavy Linux workspace selects four exact ordinary shards with two test
threads each via `OKILUM_WORKSPACE_ORDINARY_SHARDS=4`; hosted keeps two shards.
Shared compilation uses the same exact-shard census with the full `--workspace`
package selection and original `platform::`/Log exclusions. Its entire serial
Log selection still runs afterwards, separately from all ordinary processes.
Serial Log I/O overlaps those shards in its own process, and private reveal
runs alone afterwards. Duplicate/prefix names, nonempty census counts and
failure propagation remain mandatory. The standalone Live Preview group caps
native fixture concurrency at four instead of inheriting twelve host CPUs.
These scheduling choices require actual heavy-runner timing; a hosted pass
alone does not establish their canonical deadline result.

Warm-reader fixture operation timers start after the coordinator reports
`Checking notes`, so metadata/authority preparation does not consume the
cancellation or priority-read deadline. Preparation remains bounded by the
outer suite supervisor. Failed priority assertions release and join readers
before removing the vault; a cancellation worker owns its temporary vault.

Runtime shards and the standalone Live Preview group use private CI writer
accounts. `test-group.py --isolated-writer-account` allocates a fresh
`XDG_STATE_HOME` and keeps it until every descendant has been reaped. All
participants within that group inherit the same account, so actual cross-window
and cross-process writer fences remain exercised. The ambient account and its
durable records are untouched. Serial shared Log uses its own group account.
Shards retain the original outer 1320-second Linux / 60-minute shared caps;
nested supervision never extends those deadlines.

The shared census also discovers AppImage unit-case names from Cargo's ordered
target headers and runs that exact partition in a separate serial process.
Every invocation still selects the complete workspace. Identical full names
in other targets stay in the same partition, so no occurrence is omitted or
run twice; prefix-related names remain distinct. This preserves the standalone
AppImage fixture policy: another concurrent test's fork must not inherit an
installation-lock FD before exec. Canonical run26452 reproduced that lock
failure while all5518 shared cases were accounted for. Serial execution does
not weaken lock, cancellation, cleanup or retry assertions.

Synthetic two-shard controls explicitly select their own fixture policy instead
of inheriting the heavy workflow's four-shard environment. The four-shard
controls retain their independent overlap and failure checks. Linux lifecycle
fixtures resolve coreutils aliases before supplying executables to the actual
managed-launch NOFOLLOW guard; production still refuses substituted symlinks.
The same canonical run exposed these two environment-dependent fixture errors,
while its Mac, Windows and supplementary perf jobs passed. Earlier perf
NO_EVIDENCE remains a failed observation, not an accepted latency measurement.

Canonical26469 subsequently passed all49 serial AppImage cases and the lifecycle
fixtures. Its shared census accounted for5518 cases; later Sync UI assertions
and one Linux frame-timing assertion remained failures. Mounted Sync fixtures
explicitly draw after draining tasks, before reading hitboxes or simulating
input, as GPUI's event-simulation API requires. No callback is invoked directly
and no click/state assertion is removed. The native cached-frame measurement
runs after the concurrent workspace processes finish, retaining its ten frames, cache
invalidation controls and300ms assertion inside the same outer group deadline.
Its census partition and any failure are mandatory; Linux private reveal still
runs afterwards. These changes require a new exact-source native result.

Run26486 confirmed the corrected Linux workspace: all4573 cases and its separate
native-frame assertion passed within960.380s/1320s. Its standalone issue2486
group compiled in0.52s but reached120s during the ten serial runtime cases;
earlier groups had accumulated the ambient writer registry. That group now
gets its own inherited CI account, using the same after-reap cleanup as Live
Preview and workspace partitions. All ten cases and their five-second operation
assertions remain in the original120s group. The fixture draw helper lives under
plain `cfg(test)` in the parent module so Windows first-sync tests do not depend
on the Unix-only test module; the native Windows gate caught that portability
error before merge. Neither correction has a native acceptance verdict yet.

Run26495 accounted for all5518 shared cases and passed its native/perf bridge
jobs, but both workspace gates still failed the cached-frame300ms assertion.
Linux also missed two mounted Sync clicks. The fixture helper now drains/draws
twice: SyncSettings can mount a Root dialog while rendering, so a single frame
can describe the previous layer. The destination test waits for actual snapshot
and destination readiness before drawing; backend/error/consent assertions still
require real clicks. Cached-frame evidence now records existing native span
counts/times and individual frame durations, printed only after the unchanged
ten-frame measurement. Instrumentation is not a performance fix or acceptance.

This prevents unrelated ephemeral vault fixtures from contending on or scanning
one accumulating account-wide authority registry. The first heavy runtime
probe confirmed both corrected warm-reader fixtures, but four-way scheduling
alone still missed Live Preview120s and workspace1320s; isolation efficacy must
therefore be checked on the actual runner before a success claim. The supervisor
control checks inherited parent/child identity, preservation of nonzero exits,
account availability during descendant termination and cleanup only after reap.

Native Mac Sync admission also executes the complete supervisor `serve`
integration target with nonempty discovery. Its restart controls require a new
IPC generation with unchanged enrollment after verified tree cleanup, reject a
concurrent helper at the lifetime lease without changing the journal, and prove
that an unscoped scratch-state binary cannot bypass the isolated QA launch guard
even when its journal is Idle. Production launch/signature guards are unchanged.

The supplementary perf workflow keeps shipped input A/B in its `perf-linux`
job and runs measurement-only scroll in the independent `scroll-report` job.
Both retain their 60-minute ceilings. Run `42945853529` spent 36 minutes building
the unpatched A/B pair and another 18 minutes building report-only scroll before
starting editor measurements; its bridge/runner deadline interrupted those
measurements. The jobs retain both measurements, all calibration modes, the
600-second editor bound and source/hardware receipts. Input A/B still compares
both unpatched binaries on one runner. Artifacts are `perf-linux-<sha>` for A/B
and `perf-scroll-<sha>` for scroll, with the trace binary/checksum under `trace/`.

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
same four packages. Its four disjoint selections must report exactly that
inventory in their completed/ignored counts. Every Cargo invocation keeps all
four packages for the same feature unification and test environment. One
1320 s deadline covers the complete census and all four selections together.
After inventory compilation, two ordinary shards and the serial Log cases
execute concurrently in separate processes. Stable SHA-256 assignment of full
test names and libtest `--exact` filters keep both ordinary shards disjoint,
including identical names in different targets and prefix-related names.
Each shard has two test threads, retaining four ordinary threads in total.
Every invocation still selects the same four packages. The reveal process
starts only after all three finish. Run `26071` exhausted the same deadline
after its ordinary census passed 4342/4342 and the serial Log process had
started. Worker `42973322089` proved actual ordinary/Log runtime overlap and
Log 124/124, but still exhausted the overall deadline after the shell target
passed and before the final ordinary targets finished. All selected cases
still require individual census receipts. The orchestration control requires
both ordinary shards and Log to rendezvous, makes reveal wait, and retains
failures from every process. Exact-selection controls retain duplicate names,
reject prefix leakage and require four ordinary threads in total. These
controls reject the previous single ordinary process without reducing the
inventory, assertions or overall deadline.
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
When it is `0`, the same locked pass also removes Cargo's empty incremental
directory scaffold. A populated cache below the cap stays intact; disabling
compilation does not silently discard a previous branch's warm cache.
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


Canonical26633 on d88 retained two real shared timing failures: first text for
3.5MB code exceeded1000ms while ordinary fixtures competed, and cached editor
frames took381.629ms/300ms even after ordinary partitions. The first-text case
now has its own serial, counted process after ordinary/AppImage/Log work, just
before the existing isolated frame process. Both original assertions and source
sizes remain unchanged, and every workspace case still runs exactly once.
Native-frame evidence now filters existing spans by the measured document and
records Linux thread schedstat outside the timed interval; no CPU-time waiver
replaces the wall assertion. Efficacy requires fresh source-pinned hardware proof.

The same Linux run compiled network-save controls in2.08s but exhausted their
120s runtime group while sharing the accumulated ambient writer registry. That
standalone group now inherits its own private CI account with after-reap cleanup,
using the same tested supervisor as issue2486/Live Preview. All nine cases,
publication fences and original120s deadline remain. Regression controls exercise
the real shell entrypoint and require the private-account option for this group.


Canonical26859 proved the two heavy jobs actually overlapped on the SAME runner
UUID: Linux began08:13UTC and shared08:24UTC. Linux later exhausted HTTP180s and
agent-import120s groups while shared compilation/runtime was active. The local
shared job now waits for linux-local, retaining its own60min cap and every suite;
Linux keeps80min. always()+!cancelled() preserves independent shared evidence
after a Linux failure or lane skip. Hosted/platform lanes remain independent.
This removes that observed collision without changing runner/host configuration
or any operation deadline; its measured effect still needs exact-source proof.
The standalone issue1588 reader fixtures also receive a private inherited CI
account, as do the other independent reader groups.

The empty remote-log async deadline control previously spent its120ms before a
single poll reached the loopback server. It now uses the existing cfg(test)
HTTP clock: observe the real held empty poll, expire exactly120ms, require the
actual timer to fire, then retain all timeout/partial/empty-result assertions.
The production transport and its real timers are unchanged.
