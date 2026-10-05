# AI Brain POC local service

The optional `tessera-cored brain` entrypoint serves `ai-brain/v1` JSON-lines over
an explicitly selected loopback TCP address. Existing cored JSON-lines and MCP
stdio commands remain unchanged. The process owns the runner; closing one client
connection does not cancel work or shut down the listener.

The library contains a single-goal, single-stage runner and an `Adapter` boundary.
Without `--config` the CLI registers no provider adapters. Explicit configuration
adds CLIProxyAPI chat, Todoist tasks and a dedicated T3 stage. Configuration and
additive application commands are documented in the
[application API](ai-brain-application-api.md). Tests use local mocked providers.

## Start against an isolated fixture

```bash
mkdir -p fixture/brain/records fixture/runtime
cargo run -p tessera-cored -- brain \
  --brain-id 01000000-0000-4000-8000-000000000001 \
  --vault fixture/brain --operational-dir fixture/runtime \
  --records-dir records --listen 127.0.0.1:0 --managed-brain
```

The first stdout line reports `{schema, ready, listen, adapters}`. Port zero asks
the OS for an available port; subsequent connections use the reported address.
No service is installed. All paths are explicit. The operational directory must
exist outside the brain and must not be deleted when rebuilding an index.
`--managed-brain` asserts the cooperating-writer boundary from the
[source contract](ai-brain-contracts.md#2-exact-source-boundary). Without it,
source writes retain proposals instead of replacing canonical files.

## Requests and replies

Each connection accepts multiple newline-terminated JSON requests containing
`schema: "ai-brain/v1"`, a caller-selected `id`, and an `op`. Replies echo the
schema/id and contain `ok` plus either `data` or `error`. Source errors preserve
`code`, `message` and conflict identity; runtime errors use `runtime_error`.
Malformed requests and unsupported schema versions do not dispatch work.

| op | Additional fields | Result |
|---|---|---|
| `snapshot` | none | Goal/stage, frozen dispatch, binding, phase, attention and pending-write count |
| `goal_source` | none | Exact current goal source snapshot |
| `source_read` | `path` | Exact source snapshot |
| `source_write` | `request` (the SourceWrite envelope) | Source receipt or structured conflict |
| `create_goal` | `goal`, `body` | Initial durable goal and snapshot |
| `prepare_stage` | `stage`, `packet`, `operation_id`, `target` | Frozen context and durable dispatch intent |
| `start` | none | Start only a prepared or proven-not-started operation |
| `reconcile` | none | Inspect known work; unknown never permits another start |
| `poll` | none | Observe the bound engine without starting work |
| `ingest` | `event` | Correlated observation/result with ordering and deduplication |
| `result` | `result_id` | A saved result with evidence and verification |
| `accept_human` | `criterion_id`, `actor`, `observed_at`, `source` | Canonical human receipt and re-evaluation of an existing result |

Record/envelope fields are defined by [the foundation contract](ai-brain-contracts.md)
and [the shared Rust types](../crates/tessera-brain/src/types.rs). `Goal` and `Stage`
include required empty lists/null fields, rather than silently inventing missing
criteria or task state. The initial API accepts one goal and one stage, not a
workflow graph. `accept_human` is an explicit operator action; imported engine
claims of human acceptance cannot satisfy it. It is separate from a general
nonhuman `criterion_evaluate` operation. A human criterion requires its own actual
human receipt; artifact review cannot replace it.

## Continuity and authority

- The operational journal persists the exact envelope, binding, scoped event
  identities/cursors and pending source operations. On restart, submitting/running
  work reconciles before observation; it never starts from scratch automatically.
- Valid outcome intake and its result/stage/goal source requests are persisted
  together before source writes. If interrupted after the result file but before
  projections, restart replays the same source operation IDs and finishes those
  projections without another result or engine dispatch.
- Rejected stale/unbound events remain in operational history. Terminal stage
  status cannot regress from a later progress/blocked/cancelled event. Successful
  engine status is not criterion evidence; empty or changed criteria cannot pass.
  If later source edits invalidate a recorded completion, snapshots show the goal
  as blocked pending re-evaluation while retaining the historical source receipt.
- Human acceptance lives in a canonical evidence Markdown record; the journal's
  lookup is operational state. Completion reads that canonical receipt and the
  retained criterion definitions, then updates the same saved result.
- Goal/stage projections preserve unknown YAML fields and existing body newline
  bytes. Generated result/context/conversation bodies render their canonical typed
  content so preview readers can see the saved knowledge. The source API itself
  never normalizes source bytes.
- One runner process holds the operational lock. Missing/corrupt journal state
  alongside owned goal files is a recovery error, not permission to repeat work.
- A configured adapter driver observes active work once per second independently
  of client connections. No adapter/network calls occur for an idle goal. Provider
  credentials remain runtime-only. Non-secret provider identity is retained in the
  operational journal to reject accidental rerouting of unfinished work.

This is a local trusted-client POC transport, not an authenticated remote API.
The build/tests prove fixture behavior only; account access, live provider behavior,
installation and the interactive POC acceptance remain separate checks.
