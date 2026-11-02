# Fast runner CPU admission

`fast / check (pull_request)` runs on baldr's
`forgejo-runner-fast.service`, label `fast-linux`. Its four independently locked
persistent source/target pairs use three compiler workers each. The complete
desktop/supervisor build, workspace test compilation and parsing tests retain
their six-minute workflow deadline. The runner ceiling is ten minutes in
`/var/lib/forgejo-runner-fast/config.yml`.

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
