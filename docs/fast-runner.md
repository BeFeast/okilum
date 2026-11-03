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

Run [20932](https://git.oklabs.uk/BeFeast/okilum/actions/runs/20932/jobs/0)
compiled the shell test binary in 335s after desktop build=88s and workspace
check=75s. The suite then ran for more than 394s with all reported tests passing
before the fifteen-minute `check` deadline killed it. The hosted 258.19s runtime
is from different hardware and cannot size this runner's budget.

`check` retains every pre-#2434 command and its fifteen-minute timeout. The new
`shell-full` job queues after `check`, owns a separate twenty-minute execution
budget, and uses the existing fast runner, slot locks, stable sources, compiler
flags and three compiler workers. Its locked inventory command resolves test
dependencies even if it gets a different slot; the unfiltered offline test run
uses four test workers and rejects an empty inventory. Failures propagate to its
own status. No host setting or nightly/native suite changes.

The initial budget uses the existing twenty-minute runner ceiling, leaving
471s beyond the observed 335s compilation + 394s incomplete execution. This is
provisional until two complete normal-load measurements establish the runtime
and margin. Keep the new status non-required until those successful PR runs;
record the final measurements and branch-protection decision in PR #2434 and
issue #2415 before re-enabling auto-merge.


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

Limits include compilation/downloads and leave room below the unchanged job
ceilings (check 15 min, shell-full 20 min, sync-source-sharing 15 min):

| Scope | Limit | Basis / margin |
| --- | ---: | --- |
| CI tooling Python discovery | 90 s | Includes the real hanging control (~2.3 s with a shortened grace); existing step ceiling stays 2 min. |
| Entire fast-local check / locked fast.sh | 540 / 480 s | ~330 s measured check, with 150 s gate margin; outer scope also bounds source/cache setup. |
| Check build / workspace test compilation | 360 s each | Inside the tighter 480 s combined gate. |
| Channel / paste inventory / paste tests | 120 s each | Inside the same combined gate; test selection is unchanged. |
| Entire fast-local shell / locked shell-full.sh | 1110 / 1020 s | ~780 s measured full shell run, 240 s locked gate margin. |
| Shell inventory / unfiltered execution | 360 / 900 s | Compilation allowance and 15 min execution cap inside the combined gate. |
| Entire sync-local entrypoint | 840 s | Bounds lock, vendor preparation and both groups together below the 15 min job ceiling. |
| Sync target lock / vendor setup / verification | 600 / 120 / 60 s | Preserve the existing lock wait for concurrent jobs; the outer scope bounds the combined wait/setup/tests. |
| Sync source sharing (two concurrent trials) | 480 s | See same-runner measurements below; nearly 2x the longest complete observed sync step. |
| Sync network policy | 120 s | ~51 s test execution in the measured loaded run, plus compile/setup margin. |

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

After merge, use `scripts/ci/fast-runtime-report.py PR_NUMBER` to inspect the next
24 h of `fast.yml`. It prints the longest started-to-stopped runtime and the run
URL against the incident's 2700 s (45 min). Creation-to-stop is printed separately
because queue delay is not execution time. Running workflows are lower bounds;
include failed/cancelled runs and do not claim a full-day observation early.
Record the result in #2503 with the observation window and raw JSON evidence.
