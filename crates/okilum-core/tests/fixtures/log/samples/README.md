# Log viewer sample pack (fixtures)

Synthetic logs from the owner's sample pack attached to #1155
(`okilum-log-samples.zip`, `showcase/`), copied unchanged. The pack's
`generate.py` regenerates them with a fixed seed; IPs are documentation
ranges and hosts and users are made up.

`tests/log_samples.rs` runs the pack README's "Try" queries over these files.
Its expected counts come from a separate Python evaluation of the same
queries, not from the Rust evaluator.

`native-formats/` holds the pack's native-format files, also unchanged:
spdlog default pattern, glog and Apache combined access log for the text
formats of #1162; journald, macOS `log show` and Windows `Get-WinEvent` JSON
for the field mapping of #1163. `../syslog.log` is hand-written: the
pack has syslog only as rsyslog JSON.
