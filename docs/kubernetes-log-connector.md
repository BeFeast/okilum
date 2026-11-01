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

The desktop Logs section offers **Kubernetes logs…**. Its vault-scoped window
requires an explicit namespace, pod and container; context is optional and the
initial tail defaults to 1000 lines. Follow and Previous instance are explicit
choices. Nothing runs on opening the window. Start reads this target; Stop and
window close cancel the selected session. Target edits are disabled until it
finishes stopping. Inputs and loaded rows are session-only.

The worker polls off the render path and feeds the shared leased stream assembler
and ordinary LogView. Split UTF-8 and CRLF remain intact. Only complete UTF-8
lines enter the view; trailing bytes without a newline are reported explicitly.
The current session stops at 8 MiB, 20,000 lines or a 64 KiB line. Existing rows
remain readable on a limit, read error or failed exit. Stderr has a separate
16 KiB bounded diagnostic area and never becomes a log record. The UI receives
coalesced immutable snapshots through a one-update channel; stale generations
cannot replace current state. No live Kubernetes GUI acceptance is claimed from
source, fake transport or headless layout tests.

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
