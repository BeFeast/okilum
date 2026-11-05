# Fast runner admission

`fast / check (pull_request)` runs on baldr's
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
`okilum-slot` build slot; `fast-local.sh` no longer has a shell-full mode.

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
Job ceilings are now check 55 min and sync-source-sharing 30 min on
`fast-linux`; the shell-full waiter has 60 min on `bridge` and its hosted job 45 min (#2589).
These are ceilings for slow progressing work; a hung phase is still killed by
its own original deadline plus at most 30 s cleanup:

| Scope | Limit | Basis / margin |
| --- | ---: | --- |
| CI tooling Python discovery | 180 s | Recent green baldr runs: 45.595–81.114 s; current main run 21861 was still progressing when its old 120 s step expired. All three Python step ceilings are 5 min (#2536), including the 30 s cleanup grace. |
| Releases / updater Python discovery | 90 s each | Recent green runs: at most 4.802 / 0.107 s; #2536's 5 min step ceilings retained. |
| Entire fast-local check / locked fast.sh | 2100 / 2040 s | 300 + 540 + 540 + 120 + 120 + optional 360 = 1980 s; locked gate adds 60 s, entrypoint adds 60 s including the default 20 s slot wait. Optional Live Preview is included conservatively. |
| Check build / workspace test compilation | 540 s each | Loaded build 300 s in 20289; workspace check 324.35 s in the main probe above. Margins 240 / 215 s. Both phases may consume their full limits. |
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

`test_fast_budgets.py` parses the actual workflow and scripts, follows both
fast-local modes and the sync wrapper, includes the sync lock wait and optional
Live Preview, and enforces each aggregate >= sum(children) + 60 s. Step ceilings
include the supervisor's 30 s cleanup; job ceilings include all bounded steps
and another 90 s for setup/final cleanup. Mutation controls reject the old
aggregate limits, growth of every child phase/lock, and insufficient step/job
ceilings. The invariant test fails against the pre-fix limits.

The check job retains #2536's three 5 min Python step ceilings and adds a
2 min certification step: 900 + 120 + 2100 + 30 + 90 = 3240 s <= 3300 s.
The shell-full waiter on `bridge` adds the same 2 min step to its 56 min hosted
bridge step: 120 + 3360 + 90 = 3570 s <= 60 min (#2589). The invariant conservatively includes certification even on ordinary main/nightly
runs, where that step is skipped.

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
checkout, then exercises successful, failing and hung discovery through the
staged supervisor. Phase supervision inside the product scripts applies when
those scripts contain it.

The owner reports baldr now allows a 60 min runner ceiling. Both fast-linux job
ceilings (55/30 min) fit it; this change does not modify any host settings.

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
