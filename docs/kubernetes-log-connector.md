# Kubernetes log connector (#1384)

`okilum_core::log::kubernetes::Request` opens one explicitly selected
namespace/pod/container using the installed `kubectl` and its existing environment
and kubeconfig. An optional context is passed to this invocation only; omission
uses kubectl's current context. Nothing edits credentials, kubeconfig, contexts,
pods or other cluster resources. There is no pod discovery or implicit container
selection. Existing kubeconfig exec credential plugins remain kubectl's behavior.

The only cluster command is `kubectl logs pod/<pod> --container=<container>`.
Requests include an explicit namespace, tail count (0–100000), previous-instance
choice and follow choice. Arguments are passed directly, with no shell. Missing
or invalid targets fail before spawning. There is no automatic retry or fallback
to another pod/container/context. A previous instance that does not exist remains
a visible kubectl failure.

`Session::next(timeout)` yields exact stdout chunks, separate stderr diagnostic
chunks, read errors, and the final exit status after both pipes drain. Chunks may
split UTF-8, lines and multiline records: consumers must preserve partial bytes
and use the existing log parser rather than treat chunks as records. This is a
connector, not a CRI parser. kubectl timestamps are not injected into log content.

The queue holds at most 16 × 16 KiB chunks, with one further chunk per pipe;
backpressure pauses reads rather than dropping records. Consumers must retain a
bounded window and surface exclusions using the existing Log view budget rules.
The tail limit bounds history lines, not their byte size. Session cancellation
and Drop kill and reap the direct kubectl child and release blocked senders.
They do not manage subprocess trees created by external credential plugins.

## Integration boundary

This packet implements the core connector only. The desktop target picker and
Log view adapter are not wired here. That integration must own a Session, poll it
off the render path, preserve record boundaries across Data events, feed the
existing bounded log storage, show Diagnostic/ReadError/nonzero Exited outcomes,
and cancel on Stop, target replacement and window close. Do not make stderr into
log rows or claim live Kubernetes GUI acceptance from the fixture tests.

## Offline verification

The std-only module can be checked without building the application:

```sh
rustc --edition=2021 -C linker=/usr/bin/gcc --test \
  crates/okilum-core/src/log/kubernetes.rs -o /tmp/okilum-kubernetes-tests
/tmp/okilum-kubernetes-tests
```

Unix tests run a local synthetic process, passing the production argument vector.
They cover exact bytes (including multiline and unfinished records), literal
context arguments, previous/follow flags, explicit target validation, separate
permission diagnostics/nonzero exit, spawn failure, and positive-control live
output followed by cancellation under queue backpressure. No cluster access is
required. Full core/application checks and native acceptance are not established
by this isolated test command.
