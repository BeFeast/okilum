# Log names and compressed logs (#1164)

Packed by the system `gzip -9 -n`, `zstd -19` and `bzip2 -9`, so the crates
that read them shared no code with the writer.

- `access.log.1.gz`: `samples/native-formats/apache-access-combined.log`
- `ordgw.log.2026-10-10.zst`: `samples/native-formats/cpp/ordgw.spdlog-default.log`
- `syslog.log-20261010.bz2`: `../syslog.log`
- `two-members.log.gz`: `../syslog.log` as two gzip members (lines 1–4, 5–8)
- `ordgw.trade-gw-01.god.log.INFO.20261010-060400.4411`: the pack's glog
  file under the name glog gives it.
