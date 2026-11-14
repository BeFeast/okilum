# Fast runner admission

Since #2589 neither certification job compiles on baldr: `check` and
`shell-full` wait on `bridge` while GitHub's dedicated runners execute them
(see [Hosted certification check](#hosted-certification-check-2589) and
[Hosted certification shell-full](#hosted-certification-shell-full-2589)).
Since 14.11 (Okilum left baldr) nothing uses `fast-linux`: nightly/manual
`sync-source-sharing` waits on `bridge` while GitHub runs it
(`.github/workflows/forgejo-sync-source-sharing.yml`, same group limits, test
targets built first outside the timed groups). The slot pool
(`fast-local.sh`, `fast-source.sh`) is removed; the sections below are the
history behind the unchanged phase deadlines.

`fast / check (pull_request)` ran on baldr's
`forgejo-runner-fast.service`, label `fast-linux`. Its two independently locked
persistent source/target pairs use three compiler workers each. The admission job
keeps the complete desktop/supervisor build, workspace test compilation, appimage
channel tests and parsing tests. Its workflow deadline is
fifteen minutes (#2378); the runner ceiling is twenty minutes in
`/var/lib/forgejo-runner-fast/config.yml`.

The runner has `capacity: 2` and the service sets `OKILUM_FAST_SLOTS=2`; the
script also defaults to two slots. Additional jobs queue before execution starts.

The service needs `CPUWeight=1000` relative to other `system.slice` services,
including release and Windows diagnostic build containers. The default weight
of 100 let concurrent builds consume the CPU needed by PR admission. A warm
Cargo target cannot cache the shell executable across every PR: its build script
embeds `OKILUM_SOURCE_COMMIT`, so changing the source SHA rebuilds it.

Apply the setting with the existing systemd helper, without restarting any
service:

```sh
ssh baldr 'bash -s' < scripts/ci/fast-runner-priority.sh
```

The operator script refuses other hosts, requires an active runner, applies
`systemctl set-property` and verifies both the weight and unchanged runner PID.
systemd persists the setting in
`/etc/systemd/system.control/forgejo-runner-fast.service.d/50-CPUWeight.conf`.
No job edits host configuration. To restore the previous scheduling weight:

```sh
ssh baldr 'sudo -n systemctl set-property forgejo-runner-fast.service CPUWeight=100'
```

This is a relative scheduling preference, not exclusive cores or a CPU quota;
other workloads continue running and may use all idle CPU. Fast logs include
cgroup CPU weight and CPU/memory pressure alongside the slot and phase timings.
If contention returns, inspect those measurements before changing deadlines or
discarding persistent targets.

## Incident evidence, 2 November 2026

- Web run 20153 (API 26563), attempts 1 and 2, died at six minutes in `build`.
  Attempt 2 reused slot 1's `target-stable-source-v1` and compiled only the shell;
  it did not wait on a Cargo target lock.
- Web run 20183 (API 26594), attempt 1, built in 244 seconds, then hit the same
  deadline during workspace test compilation. There was no test failure.
- baldr had 12 CPUs, load 17–20, concurrent release/Windows builds, and default
  CPU weight on the fast runner. Its ten-minute runner ceiling was not reached.

See issues #2378 and #2377 for validation logs and the same-host comparison.

## Base-crate rebuild budget, reopened #2378

The six-minute workflow budget remained too short after CPUWeight=1000 and
capacity=2. PR #2386 web run 20289 attempt 1 built in 300 seconds, then died
45 seconds into workspace test compilation. Attempt 2 built in 164 seconds and
finished the workspace check in 176 seconds immediately before cancellation.
Attempt 3 passed with build=2s and check=3s: a warm retry, not a core-rebuild
reliability result. Independently, #2387 web 20327 and #2365 web 20328 also reached
the six-minute deadline during workspace test compilation with capacity=2.

The same-host probe on 2 November used inactive legacy fast slot 2 (under its
exclusive lock), main f78a1f458, the workflow's exact debug/link flags,
three compiler workers and verified vendor patches. It ran direct rustc to avoid
compiler-cache hits masking invalidation. A transient system.slice scope with
CPUWeight=500 modelled one of two jobs sharing the fast service's weight of 1000.
Three agent slots and release containers kept running; this was an idle **target**,
not an idle host. Raw phase logs, /usr/bin/time CPU totals, load/process snapshots
and Cargo --timings are retained in `~/work/qa-evidence/issue-2378/` on baldr and
maestro, with results linked in #2378.

The initial preparation took 573.93s, including 126.62s for the previously cold
appimage-only dependency variant. Desktop/supervisor compilation took 250.30s and
workspace test compilation 176.79s. The immediate unchanged-source control took
14.13s total (build 2.48s, check 2.05s), proving the target was warm before each
subsequent deliberate core invalidation. The priming run is excluded from the
core-only rebuild distribution.

Three deliberate core invalidations on the already-warm target completed:

| Trial | Desktop build | Workspace test check | Full gate |
| --- | ---: | ---: | ---: |
| 1 | 237.87s | 324.35s | 570.45s |
| 2 | 208.20s | 179.52s | 397.44s |
| 3 | 202.12s | 222.63s | 433.66s |

The nearest-rank empirical p95 of these three runs is **570.45s**. The initial
cache preparation and unchanged-source control are excluded. In the first run, Cargo timings attributed
39.05s to core compilation followed by 197.15s for the shell binary. The whole
run consumed 765.96s user + 48.25s system CPU time. Both core compilation and
core test compilation appeared in the log; 11 channel and 15 parsing tests ran.
The unchanged-source control above is therefore a cache control, not the proof.
The independent fresh core-touching PR and original #2386 rerun are recorded in
#2378.

A ten-minute workflow would leave less than 30 seconds for setup and tail
variation after that measured core rebuild. The **fifteen-minute** workflow
budget leaves approximately 5m30s of headroom before CI checkout/cache-probe
overhead. Sizing uses 1.5 times the measured empirical p95 plus 30s for setup,
rounded up to whole minutes: ceil((1.5 * 570.45 + 30) / 60) = 15. This is a small controlled sample,
not an estimate of a long-term fleet p95; the issue records its sample count and
nearest-rank empirical p95. Every compiler/test command in fast.sh is retained.
The slot default now matches the already deployed capacity=2 and
OKILUM_FAST_SLOTS=2. The manager's three agent slots, slice layout and CPU shares
are unchanged; no CPU is reserved from agent builds.

### Runner ceiling and rollback

The runner's previous `runner.timeout: 10m` would override the longer workflow.
Set only that value to `20m` in `/var/lib/forgejo-runner-fast/config.yml` on baldr.
Check Forgejo runner 21's task list and its cgroup before stopping it;
there must be no active job, checkout, compiler, test or job shell. The idle
cache daemon may remain. Stop/start only `forgejo-runner-fast.service`, preserving
capacity=2, OKILUM_FAST_SLOTS=2 and CPUWeight=1000. A guarded check refuses a busy
runner and freezes it to verify the process snapshot. It signals only the idle
MainPID for graceful shutdown, then thaws and waits for the daemon to exit before
stopping the unit and editing the config. This protects an in-flight assignment
racing the snapshot without signalling job processes. This host refuses stop on
a frozen unit. No other service or slice is changed.

The before/after PID, idle evidence and applied change are recorded in #2378.
The config backup is
`~/work/qa-evidence/issue-2378/fast-runner-config.before.yml` on baldr. To roll back,
wait for that runner to be idle, stop only that service, restore the backup over
config.yml, then start only that service. No daemon-reload is required because
the unit file is unchanged. Revert this repository change to restore the old
six-minute workflow budget. Reverting the host ceiling alone while the new
workflow remains would restore the earlier effective ten-minute ceiling.

## Independent full shell suite (#2415 / #2434)

Superseded for execution by #2589: the suite now runs on GitHub's dedicated
runner, see [Hosted certification shell-full](#hosted-certification-shell-full-2589).
The history below explains its phase deadlines, which are unchanged.

Run [20932](https://git.oklabs.uk/BeFeast/okilum/actions/runs/20932/jobs/0)
compiled the shell test binary in 335s after desktop build=88s and workspace
check=75s. The suite then ran for more than 394s with all reported tests passing
before the fifteen-minute `check` deadline killed it. The hosted 258.19s runtime
is from different hardware and cannot size this runner's budget.

`check` retains every pre-#2434 command. The initial fifteen-minute check
and twenty-minute shell budgets described here are superseded by the additive
watchdog budgets below. `shell-full` queues after `check`, owns a separate execution
budget, and uses the existing fast runner, slot locks, stable sources, compiler
flags and three compiler workers. Its locked `cargo test --no-run` command resolves
test dependencies even if it gets a different slot. Inventory and the unfiltered
test run execute that exact reported artifact directly, with four test workers
and Cargo's package working directory, and reject an empty inventory. Failures propagate to its
own status. No host setting or nightly/native suite changes.

The initial budget uses the existing twenty-minute runner ceiling, leaving
471s beyond the observed 335s compilation + 394s incomplete execution. This is
provisional until two complete normal-load measurements establish the runtime
and margin. Keep the new status non-required until those successful PR runs;
record the final measurements and branch-protection decision in PR #2434 and
issue #2415 before re-enabling auto-merge.


## Hosted certification shell-full (#2589)

Main certifications 30614, 30687, 30746 and 30808 (sources 5607374a, b7f5a18e,
5fed3511, c79bd92f) all failed `shell-full` on baldr with
`TEST_GROUP_TIMEOUT group=shell-binary-tests limit=990s`. Every finished test
passed and none hung; they had completed 1930/2486, 1674/2486, 1684/2492 and
1452/2500 tests. 30808 ran 1036 tests in the first 240 s and 416 in the
remaining 750 s. Raising the deadline (#2628) was rejected: run 30808 projects
past 2100 s, a baldr slot rerun stopped at 2085/2492 inside 2100 s, the suite
grows daily, and the 60 min runner ceiling caps execution at about 2310 s. The
cause is co-tenant load on a 6-core/12-thread host shared by the fast, sprint,
bridge and org runners and the build-slot pool, not the suite.

The same unfiltered binary takes 213.73-236.82 s on GitHub's dedicated 4 vCPU
`ubuntu-24.04` runner. `fast.yml`'s `shell-full` job therefore stays the
certification job and reporter, but no longer compiles on baldr:

1. It runs on `bridge`, stages `github-pr.py` and its transport from the workflow
   revision, then validates and checks out the requested source exactly as
   before (`hourly_beta.py certify-source`).
2. A source without `.github/workflows/forgejo-shell-full.yml` fails at once
   (`predates hosted shell-full`); certify a newer main commit.
3. It connects the shallow checkout to history (a shallow commit pushes only if
   GitHub already mirrors it), pushes the exact source to a unique
   `forgejo-shell-full-pr/<run>-<sha>-<run id>-<attempt>-<uuid>` ref and waits
   for that run. Success requires the exact SHA, ref, workflow, job `shell-full`
   and an executed `Run the complete Linux shell binary suite` step; GitHub
   being unavailable is a failure, never a fallback pass. The ref is deleted.
4. The hosted job installs the hosted apt set, then runs the source's own
   `bash scripts/ci/test-group.sh shell-full.sh 1920 bash scripts/ci/shell-full.sh`:
   vendor setup/verify, compile 840 s, list 30 s, unfiltered
   `--test-threads=4` execution 990 s. Compiler flags match the former job
   (`CARGO_INCREMENTAL=0`, line-tables-only debug info, mold); no cache.
5. `shell-full-status` reports `fast / shell-full (workflow_dispatch)` from
   that job's result, unchanged; `daily-release` and `hourly_beta.py` need no change.

Budget chain, enforced by `test_fast_budgets.py`:

| Scope | Limit | Basis |
| --- | ---: | --- |
| Hosted job | 45 min | 10 min apt step + 33 min suite step (1920 s group + 30 s grace) + 90 s setup/cleanup = 2670 s <= 2700 s. |
| Bridge run timeout | 47 min | Hosted job + 120 s, so GitHub's own deadline fires and is observed first. |
| Shared bridge budget | 53 min | Push, queue and run share one deadline (`BRIDGE_BUDGETS`); a late start gets only the remainder. |
| Bridge step | 56 min | 60 s history fetch + 3180 s budget + 120 s poll/cancel/cleanup = 3360 s. |
| Waiter job | 60 min | 120 s validation + 3360 s + 90 s = 3570 s <= the primary bridge runner's `runner.timeout: 1h`. |

The shared deadline matters because hosted queueing is bursty: for 77 recent
first jobs of `agent-checks`/hosted lanes, push-to-start was 3 s median, 71 s p90
and 980 s maximum (agent-checks 41999797379, 5 November).

Pre-merge exercises, 5 November 2026, each through the exact bridge step
(shallow checkout, 5-6 s history fetch, staged `github-pr.py`), cold, no cache:

| Hosted run (branch head) | Start | Compile / 840 s | Execution / 990 s | `shell-full.sh` / 1920 s | Job / 45 min | Bridge / 3180 s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| [42049574081](https://github.com/BeFeast/okilum/actions/runs/42049574081) (4c34dc0f) | 3 s | 490.37 s | 390.09 s | 888.75 s | 15 min 35 s | 981 s |
| [42052787792](https://github.com/BeFeast/okilum/actions/runs/42052787792) (44496ab9) | 2 s | 530.26 s | 422.20 s | 960.72 s | 16 min 44 s | 1050 s |

Both ran 2506 tests: `2493 passed; 0 failed; 13 ignored`. Apt took 26-28 s,
the toolchain 9-10 s; disk use rose from 59 to 67 GB of 145 GB. The broader
`agent-checks` invocation of the same binary finished in 213.73 s and 236.82 s
(runs 42026610742, 42019928094). Two samples are not a hosted p95.
`test_fast_budgets.py` pins compile >= 1.5x and execution >= 2x the slower
measured phase, and keeps twice the slowest observed queue inside the bridge
budget after the slower measured bridge wait.

A slow or failing hosted run is diagnosed from its GitHub log (linked in the
Forgejo job). For a local reproduction run `scripts/ci/shell-full.sh` in a baldr
`okilum-slot` build slot.

## Hosted certification check (#2589)

Certifications 31035 and 31053 (sources 3752a757, d859fd59) then failed `check`
on baldr with `TEST_GROUP_TIMEOUT group=build limit=540s`, status 124 and no
failed test; `shell-full` was skipped, so neither Beta nor Stable could advance.
In 31053 sccache was unavailable and the slot target rebuilt from its first
crate: three compiler workers on the loaded host compiled 440 crates in 540 s
and had not reached any `okilum-*` crate. The certifications that passed on 5 November already sat
near every deadline: build 109-290 s, workspace check 69-314 s and
`ci-python` 79-168 s of its 180 s limit (30614-30925).

`fast.yml`'s `check` job stays the job whose result `certification-status`
reports as `fast / check (workflow_dispatch)` (Beta's gate, which also
dispatches the source-pinned beta builds), but it runs nothing on baldr:

1. It runs on `bridge`, stages the transport from the workflow revision,
   validates and checks out the requested source (`hourly_beta.py certify-source`),
   exactly as `shell-full` does.
2. A source without `.github/workflows/forgejo-check.yml` fails at once
   (`predates hosted check`); certify a newer main commit.
3. It pushes the exact source to a unique
   `forgejo-check-pr/<run>-<sha>-<run id>-<attempt>-<uuid>` ref and waits for
   that run. Success requires the exact SHA, ref and workflow, a successful run,
   and every gate step: job `check` with an executed
   `Build Linux desktop and run fast unit tests`, and job `python` with the
   CI tooling, releases and updater suite steps (`ALSO_REQUIRED` in
   `github-pr.py`, proven by the jobs API only; job-log evidence never stands
   in for them). GitHub being unavailable is a failure, never a fallback pass.
   The ref is deleted.
4. The hosted run has two parallel `ubuntu-24.04` jobs, both with the former
   job's apt set and the pinned toolchain:
   - `python`: `setup-python-tests.sh`, then the former commands
     `ci-python 180`, `releases-python 90` and `updater-python 90` in 5 min
     steps, supervised by the product's own `test-group.sh`;
   - `check`: `bash scripts/ci/test-group.sh fast.sh 2040 bash scripts/ci/fast.sh`,
     the former locked-gate limit, with the product's unchanged phases (channel
     tests 300 s, build 540 s, workspace test check 540 s, paste inventory and
     tests 120 s each). Compiler settings match the former job
     (`CARGO_INCREMENTAL=0`, line-tables-only debug info, mold) with four
     compiler workers. There is no cache: every certification compiles cold.

   One sequential job does not fit: 10 min apt + three 5 min steps + the 35 min
   gate step + 90 s is 62 min, above the 60 min waiter on `bridge`.
5. `certification-status` and `shell-full` (which still `needs: [check]`) are
   unchanged; `hourly_beta.py` and `daily-release` need no change.

Nightly and ordinary manual runs use the same hosted check. `check` is one job
for every event, so its status context keeps one meaning; nightly would hit the
same baldr load (it already runs `shell-full` on GitHub), and keeping a second
baldr-only variant would have kept the slot pool, sccache wiring and its tests
alive for a run nobody gates on. `fast-local.sh`, `fast-source.sh` and their
tests are removed. `sync-source-sharing` stays on `fast-linux`.

Budget chain, enforced by `test_fast_budgets.py`:

| Scope | Limit | Basis |
| --- | ---: | --- |
| Hosted `python` job | 27 min | 10 min apt step + three 5 min suite steps + 90 s setup/cleanup = 1590 s <= 1620 s. |
| Hosted `check` job | 47 min | 10 min apt step + 35 min gate step (2040 s group + 30 s grace) + 90 s setup/cleanup = 2790 s <= 2820 s. |
| Bridge run timeout | 49 min | Longer hosted job + 120 s, so GitHub's own deadline fires and is observed first. |
| Shared bridge budget | 53 min | Push, queue and run share one deadline (`BRIDGE_BUDGETS`), as for shell-full. |
| Bridge step | 56 min | 60 s history fetch + 3180 s budget + 120 s poll/cancel/cleanup = 3360 s. |
| Waiter job | 60 min | 120 s validation + 3360 s + 90 s = 3570 s <= the primary bridge runner's `runner.timeout: 1h`. |

Pre-merge exercises, 5 November 2026, each through the exact bridge step from
maestro (shallow checkout, 5-6 s history fetch, staged `github-pr.py`), cold,
no cache. Runs 2 and 3 ran in parallel; run 1's `python` job failed on that
draft's own then-unupdated budget tests, its `check` job is a valid sample:

| Hosted run (branch head) | Start | Channel / 300 s | Build / 540 s | Workspace check / 540 s | `fast.sh` / 2040 s | `ci-python` / 180 s | `check` job | Bridge / 3180 s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| [42070990602](https://github.com/BeFeast/okilum/actions/runs/42070990602) (a039ecd1) | 3 s | 25.40 s | 467.95 s | 280.47 s | 793.58 s | (failed) | 14 min 3 s | 871 s |
| [42073735620](https://github.com/BeFeast/okilum/actions/runs/42073735620) (d797e484) | 3 s | 25.40 s | 446.51 s | 272.56 s | 763.98 s | 59.94 s | 13 min 22 s | 853 s |
| [42073756719](https://github.com/BeFeast/okilum/actions/runs/42073756719) (d797e484) | 2 s | 24.84 s | 444.09 s | 270.59 s | 758.59 s | 63.81 s | 13 min 12 s | 845 s |

Green runs ran 340 CI tooling, 244 releases and 7 updater tests, 15 channel and
15 paste tests; `releases-python` took at most 1.95 s, `updater-python` 0.18 s,
the `python` job 1 min 39-43 s. Apt took 18-32 s, the toolchain 8-9 s; disk
use rose from 59 to 69 GB of 145 GB. Three samples are not a hosted p95.

The cold build is the thin phase: 467.95 s against its unchanged 540 s deadline
leaves 72 s (1.15x), below the 1.5x used elsewhere; every other phase keeps at
least 1.9x. `test_fast_budgets.py` pins 1.1x for build and 1.5x for the rest,
plus twice the slowest observed queue inside the bridge budget after the
slowest bridge wait. No cache is used because a run on a unique
`forgejo-check-pr/...` ref can restore only its own or the default branch's
caches, nothing writes such a cache on GitHub's `main` today, and a cache could
not be exercised before merge. If the hosted build grows toward 540 s, the
options are a dependency cache written from GitHub's default branch or an
explicit resize of the build phase; neither is part of this change.

A slow or failing hosted run is diagnosed from its GitHub log (linked in the
Forgejo job). For a local reproduction run `scripts/ci/fast.sh` in a baldr
`okilum-slot` build slot.

## Test-group hard deadlines (#2503)

The Linux fast workflow uses `scripts/ci/test-group.sh GROUP SECONDS COMMAND...`.
The Python supervisor starts a private session and becomes a Linux child
subreaper. It sends TERM to the entire descendant tree at the wall-clock limit,
then KILL and reaps adopted children within a further 30 seconds before returning
124 (reserving up to 0.5 s of that budget for reaping and returning).
This includes double-forked daemons that created another session; a plain
`timeout ... setsid ...` does not cover those. Exact privileged descendants from
`sync-network-policy.sh` are signalled through the runner's existing noninteractive
sudo, without changing its services, watchdog, capacity or labels. Cancellation
also cleans descendants and returns a failure. Normal command status is preserved;
a successful command cannot leave background children running. Logs contain one
`TEST_GROUP_TIMEOUT group=... limit=...s kill_after=30s` line per expired group,
plus start/end lines with actual elapsed seconds. Cleanup that needs KILL after a
parent has already exited emits `TEST_GROUP_KILL` with the group and limit.

Compilation and execution have separate shell budgets. Every enclosing deadline
covers the sum of its children plus 60 s setup/cleanup overhead; job ceilings
cover all bounded steps plus 90 s checkout/toolchain/final-cleanup margin.
Job ceilings are now sync-source-sharing 30 min on `fast-linux`; the check and
shell-full waiters have 60 min each on `bridge`, the hosted check jobs 27 min
(Python) and 47 min (`fast.sh`), the hosted shell-full job 45 min (#2589).
These are ceilings for slow progressing work; a hung phase is still killed by
its own original deadline plus at most 30 s cleanup:

| Scope | Limit | Basis / margin |
| --- | ---: | --- |
| CI tooling Python discovery | 180 s | Recent green baldr runs: 45.595–81.114 s; current main run 21861 was still progressing when its old 120 s step expired; baldr certifications on 5 November took up to 167.813 s (30925). All three Python step ceilings are 5 min (#2536), including the 30 s cleanup grace, now in the hosted check's `python` job (#2589). |
| Releases / updater Python discovery | 90 s each | Recent green runs: at most 4.802 / 0.107 s; #2536's 5 min step ceilings retained. |
| Hosted fast.sh group / gate step | 2040 s / 35 min | 300 + 540 + 540 + 120 + 120 + optional 360 = 1980 s plus 60 s, the former locked-gate limit, now on GitHub (#2589). The step adds the 30 s cleanup grace; the 47 min hosted job adds the 10 min apt step and 90 s setup/cleanup. Optional Live Preview is included conservatively. The baldr-only 2100 s `fast-local-check` entrypoint (slot wait, stable slot source) is removed. |
| Check build / workspace test compilation | 540 s each | Loaded build 300 s in 20289; workspace check 324.35 s in the main probe above. Margins 240 / 215 s. Both phases may consume their full limits. On GitHub, cold (#2589): at most 467.95 / 280.47 s, margins 72 / 260 s. |
| Channel tests (including compilation) | 300 s | Cold appimage dependency variant took 126.62 s in the main probe; recent warm phases 0–4 s. |
| Paste inventory / paste tests | 120 s each | Recent green Cargo compilation/listing at most 1.11 s; parsing execution at most 0.07 s. These small commands retain their existing caps. |
| Hosted shell-full.sh group / suite step | 1920 s / 33 min | 840 + 30 + 990 = 1860 s plus 60 s, the former locked-gate limit, now on GitHub (#2589). The step adds the 30 s cleanup grace; the 45 min hosted job adds the 10 min apt step and 90 s setup/cleanup. |
| Shell compilation only | 840 s | Historical 335 s compile (20932), recent successful compile/list 180–307 s, and normal compilation censored at 360 s by the broken watchdog (21840). Run 21877 completed compilation in 503.870 s, leaving 336 s headroom; no execution timer runs during compilation. |
| Shell inventory only / unfiltered execution | 30 / 990 s | Inventory after Cargo's finished marker: 0.119–0.368 s; successful execution phases 445–805 s. Runtime margin is 185 s above the loaded maximum. Neither command invokes Cargo. |
| Entire sync-local entrypoint | 1500 s | 600 s lock + 120 s setup + 60 s verification + 480 s sharing + 180 s policy = 1440 s, plus 60 s overhead. The 30 min job reserves 300 s beyond the group; 1500 + 30 + 90 = 1620 <= 1800 s. |
| Sync target lock / vendor setup / verification | 600 / 120 / 60 s | Preserve the existing lock wait for concurrent jobs; the outer scope bounds the combined wait/setup/tests. |
| Sync source sharing (two concurrent trials) | 480 s | See same-runner measurements below; nearly 2x the longest complete observed sync step. |
| Sync network policy | 180 s | #2578: 20 full groups + 20 isolated pending-choice runs passed on baldr; loaded full-group empirical p95/max 113.561 s. ceil((113.561 + 60) / 30) * 30 = 180 s. |

Run [21877](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21877/jobs/1/attempt/1)
exposed the old non-additive design: compilation completed in 503.870 s, listing
in 0.246 s, and unfiltered tests were still progressing after ~596 s when the
1110 s aggregate watchdog expired. Their own 990 s deadline had not fired.
That aggregate cap is removed by resizing, rather than tightening any phase.
The slowest measured green test execution remains 805 s (run 21766), so 990 s
retains 185 s (23%) headroom. Compilation retains 336 s above the loaded 504 s
measurement. These observations do not establish a fleet p95.

`test_fast_budgets.py` parses the actual workflows (Forgejo `fast.yml` and both
hosted runs) and scripts, follows the hosted gates and the sync wrapper, includes
the sync lock wait and optional Live Preview, and enforces each aggregate
>= sum(children) + 60 s. Step ceilings
include the supervisor's 30 s cleanup; job ceilings include all bounded steps
and another 90 s for setup/final cleanup. Mutation controls reject the old
aggregate limits, growth of every child phase/lock, and insufficient step/job
ceilings. The invariant test fails against the pre-fix limits.

Both waiters on `bridge` (check and shell-full) add the 2 min certification
step to their 56 min hosted bridge step: 120 + 3360 + 90 = 3570 s <= 60 min
(#2589). #2536's three 5 min Python step ceilings moved with the suites into the
hosted check's `python` job: 600 + 900 + 90 = 1590 s <= 27 min. The invariant
conservatively includes certification even on ordinary main/nightly runs, where
that step is skipped.

Both exact-product validation steps use the same process-tree supervisor with a
90 s limit and a 2 min step ceiling including cleanup. Baldr certification
[22157/check](https://git.oklabs.uk/BeFeast/okilum/actions/runs/22157/jobs/0/attempt/1)
completed that step in 3 s on 4 November 2026, leaving 87 s headroom. This single
measurement is not a tail-latency estimate. Before changing HEAD, the step copies
only the two supervisor files into a job-owned `mktemp` directory under
`RUNNER_TEMP`; subsequent workflow entrypoints use that copy. A requested older
product may predate these files. Its tracked tree stays exact, while its Python
and compiler entrypoints still receive an outer hard deadline. A real Git/HTTP
control certifies such an older tree, checks the unchanged main ref and clean
checkout, then shows that the bridge step rejects a product without
`.github/workflows/forgejo-check.yml` before any push (#2589) and reaches the
bridge once the product contains it. The hosted suites run the product's own
supervisor; successful, failing, erroring and hung discovery are exercised on
the hosted step commands.

The owner reports baldr now allows a 60 min runner ceiling. The remaining
fast-linux job ceiling (30 min) fits it; no change modifies any host settings.

The newer owner-authorized #2581 supersedes #2580's fifteen-minute PR gate:
Forgejo fast.yml has no PR trigger and main has no required status checks.
Workers validate the exact committed HEAD with
`H=github scripts/agent/gh-check.sh`, wait for green, then squash-merge through
Forgejo. The larger additive envelopes here apply to nightly runs, manual dispatches
and source-pinned certification. Newer #2583 also removes heavy main push runs,
gives certification dispatches unique concurrency groups so they always report,
and lets ordinary manual retries supersede only other ordinary manual runs on
the same ref. Its feedback-ci and worker-owned GitHub gate are retained. No PR
or push workloads are restored. The Sync owner closed #2576 as superseded by
#2581 without merging its proposed move off fast-linux; no hosted routing from
that branch is imported here. A future move must carry these wrappers to the
actual execution and recheck additive hosted/bridge budgets; #2589 does this for
shell-full. #2578 is resolved
by measured policy/aggregate limits of 180/1500 s. The Sync owner's
[measurement series](https://git.oklabs.uk/BeFeast/okilum/pulls/2510#issuecomment-49613)
passed 20 full groups and 20 isolated pending-choice runs on baldr, including
15 loaded pairs and an eight-worker CPU-load window. Loaded full-group empirical
p95/max was 113.561 s (all-group p95 111.341 s); isolated loaded p95/max was
35.502 s. Reconciliation itself took 94/127 ms baseline/loaded; no product stall
was reproduced. The small sample is not a fleet p95. The 180 s group provides
66.439 s headroom above the loaded observed maximum; its additive parent is
600 + 120 + 60 + 480 + 180 + 60 = 1500 s. Raising the policy cap without its
parent is correctly rejected by the invariant.

### Resizing evidence, PR #2510

Raw logs/API snapshots are retained at
`~/work/qa-evidence/issue-2510-limits/` on maestro. All current timing samples
below come from baldr's `fast-linux` runner on 3 November 2026. Compile/list is
the old combined phase; runtime includes the old second Cargo invocation.

| Green web run (PR) | Compile/list | Execution | Slot admission to tests complete |
| --- | ---: | ---: | ---: |
| [21604 attempt 2](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21604/jobs/1/attempt/2) (#2499) | 275 s | 566 s | 847.17 s |
| [21635](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21635/jobs/1/attempt/1) (#2488) | 185 s | 698 s | 887.62 s |
| [21638](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21638/jobs/1/attempt/1) (#2485) | 302 s | 565 s | 876.60 s |
| [21641](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21641/jobs/1/attempt/1) (#2491) | 228 s | 585 s | 819.22 s |
| [21647](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21647/jobs/1/attempt/1) (#2495) | 295 s | 481 s | 786.08 s |
| [21670](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21670/jobs/1/attempt/1) (#2501) | 202 s | 611 s | 819.98 s |
| [21686](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21686/jobs/1/attempt/1) (#2510) | 256 s | 599 s | 861.57 s |
| [21692](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21692/jobs/1/attempt/1) (#2509) | 180 s | 445 s | 632.55 s |
| [21766](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21766/jobs/1/attempt/1) (#2501) | 236 s | 805 s | 1044.19 s |
| [21802](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21802/jobs/1/attempt/1) (#2514) | 294 s | 642 s | 945.58 s |
| [21809](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21809/jobs/1/attempt/1) (#2511) | 307 s | 647 s | 961.44 s |
| [21829](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21829/jobs/1/attempt/1) (#2515) | 213 s | 508 s | 725.62 s |

These include loaded runs: 21802 check admission reported CPU pressure
`some avg10=19.79 avg60=13.84 avg300=10.83`. The failing 21840 shell compilation
reported `29.86 / 30.11 / 25.39` and was still compiling at the 360 s cutoff;
that is a lower bound, not a complete compile measurement.

Main was checked explicitly: its newest certification
[21861](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21861/jobs/0/attempt/1)
hit the existing 2 min Python step ceiling while making progress, before Rust.
The latest successful main fast run in the retrieved inventory was
[17317](https://git.oklabs.uk/BeFeast/okilum/actions/runs/17317/jobs/0/attempt/1)
(31 October), before the current workspace/full-shell gate; its 85 s build is
not used to size today's gate. There is no recent successful main full-shell
sample in that inventory. The same-host main core-rebuild probe documented
above supplies the additional main compilation evidence; failed/incomplete main
runs are not presented as green measurements.

Read-only measurements on **baldr fast-linux**, 2026-11-03, from successful
Forgejo job details/logs (all before this change, no cross-machine comparison):

- [run 21635, sync job](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21635/jobs/2/attempt/1):
  complete step 244 s; source compilation 41.89 + 9.80 s, two concurrent five-test
  trials ~100.2 s, network-policy compilation 1.45 s and execution 50.80 s.
- [run 21578, sync job](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21578/jobs/2/attempt/1):
  complete step 120 s.
- [run 21482, sync job](https://git.oklabs.uk/BeFeast/okilum/actions/runs/21482/jobs/2/attempt/1):
  complete step 100 s.

`test_test_group.py` is part of required CI discovery. Its real hanging fixture
starts a test, a double-forked detached daemon and another helper, with descendants
ignoring TERM. With a 2 s limit and a 0.3 s test grace it must return 124 around 2.3 s, identify the group
and limit, and leave none of the recorded PIDs in `/proc` (including zombies).
The production grace defaults to 30 s; `--kill-after` accepts only (0, 30] s.
Independent subprocess deadlines and fixture cleanup turn a broken watchdog into
a failing test. Passing, failing, signal cancellation and successful-parent orphan
controls exercise the same production entrypoint. Existing inventory checks and
full shell/source-sharing/network-policy selections are unchanged.

The shell orchestration control substitutes a progressing compiler that writes
30 progress records over 3 s. With scaled compilation/list/runtime caps of
10 / 1 / 2 s, compilation must complete past the listing deadline and only one
Cargo invocation is allowed. Direct inventory and ordinary execution succeed;
a hanging binary then returns 124 under its own execution deadline, with both
compiler and test PIDs absent. The existing detached, TERM-resistant tree control
still checks KILL/reaping; a separate production-grace run returned 124 in
31.844 s at limit=2 s with every recorded PID absent.

After merge, use `scripts/ci/fast-runtime-report.py PR_NUMBER` to inspect the next
24 h of `fast.yml`. It prints the longest started-to-stopped runtime and the run
URL against the incident's 2700 s (45 min). Creation-to-stop is printed separately
because queue delay is not execution time. Running workflows are lower bounds;
include failed/cancelled runs and do not claim a full-day observation early.
Record the result in #2503 with the observation window and raw JSON evidence.
