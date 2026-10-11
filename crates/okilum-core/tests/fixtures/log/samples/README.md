# Log viewer sample pack (fixtures)

Synthetic logs from the owner's sample pack attached to #1155
(`okilum-log-samples.zip`, `showcase/`), copied unchanged. The pack's
`generate.py` regenerates them with a fixed seed; IPs are documentation
ranges and hosts and users are made up.

`tests/log_samples.rs` runs the pack README's "Try" queries over these files.
Its expected counts come from a separate Python evaluation of the same
queries, not from the Rust evaluator.

`native-formats/` holds the pack's text-format files (spdlog default
pattern, glog, Apache combined access log), also unchanged; they exercise
the built-in text formats of #1162. `../syslog.log` is hand-written: the
pack has syslog only as rsyslog JSON.
