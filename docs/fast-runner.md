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
