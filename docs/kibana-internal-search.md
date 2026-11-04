# Kibana internal-search sources (#2388)

This is the current connector contract. It supersedes the PIT, console-proxy,
exact-version and resource-cleanup behavior described in the historical #1441
sections of [Remote Log view](remote-log-view.md). The reader remains read-only.

## Protocol and bounded paging

History, live reconciliation and field values share `remote_log::transport::Kibana`.
They POST to `/internal/search/es`, with `Authorization: ApiKey`, `kbn-xsrf: true`,
`Content-Type: application/json`, `elastic-api-version: 1`, and
`x-elastic-internal-origin: Kibana`. Search keys are directly inside `params`,
including `index`, `size`, `track_total_hits`, `query` and `sort`.
There is no nested `params.body` and no console-proxy or PIT acquisition.

When `isRunning` is true, or `isPartial` is true with an ID, polling POSTs to the
same route with the returned `id` and unchanged `params`. Poll intervals are 50 ms
and the overall search deadline is ten seconds, including individual HTTP waits.
Stop, Apply and window close cancel active HTTP and subsequent polls; cancellation
is checked at most every ten milliseconds. Without PIT there is no cleanup route.
An HTTP timeout is typed `SearchTimeout`; async deadlines and server `timed_out`
also appear in `ResultStatus`, retaining any received rows.

`/api/status` is optional. A readable version is displayed, never compared against
one exact version. Protocol capability is checked from the actual response shape.
Malformed envelopes, hits, totals and cursors produce a clear error. Responses
remain bounded to 8 MiB and never enter diagnostic text.

History requests 200 rows per page, bounds unique documents to 10,000 and original
serialized events to 16 MiB. It freezes an absolute half-open time range (`gte`,
`lt`) and pages with `search_after`, the time field first, and `_doc` last.
A fixed `preference` keeps shard selection stable during a traversal. `_doc` alone
is not globally unique: before advancing beyond a boundary time, history replays
all documents at that exact inclusive time using bounded offsets. This recovers
sort ties across shards; rows are deduplicated by `(_index, _id)`, never timestamp.
Boundary replay is subject to the same row cap and marks truncation explicitly.
JSON nanosecond integers are forwarded exactly, never through floating point.

This is a bounded traversal of changing data, not an immutable snapshot. Live
reconciles every page of the whole selected window on each cycle, without carrying
a cursor or timestamp watermark between cycles. New and late documents still in
the window appear once. The completed window replaces the previous window;
partial or limited results remain visible with their quality labels and stop
following. Transport outages retry with bounded backoff and retain previous rows.
Selected rows, marks and details keep their existing document-identity behavior.
The five-second live visibility target remains unproven on a real deployment.

## Public API for the next UI

- `Source::direct(name, endpoint, index_pattern, time_field)` adds a source without
  Discover. An empty time field defaults to `@timestamp`. `Source::import` retains
  Discover support; `Kibana::resolve_source` resolves legacy data-view identities.
  `IndexPatternRequired` tells the panel to ask for an explicit pattern without
  discarding imported query, filters, columns or source ID.
- Existing saved JSON gains optional `index_pattern` and a defaulted `time_field`.
  Legacy records deserialize without loss and save in the expanded format using
  the existing durable, locked, atomic source storage. Credential identities keep
  their existing source/vault/endpoint scope.
- `result::ResultStatus` is exposed on `transport::Page`, `live::Loaded::quality`,
  `live::Status::History` and `live::Status::Complete`. It carries
  `HitTotal { value, relation: TotalRelation::{Eq,Gte} }`, partial/truncated/timeout,
  failed shards and cluster counts, unknown metadata and optional version.
  `label(loaded)` renders honest totals and quality beside the existing table.
- Both integer and object totals are accepted. An integer is exact with
  `track_total_hits: true`; for a numeric cap, an integer at/above the cap is a
  lower bound. Skipped shards are included among successful shards and do not
  themselves indicate partial results. Skipped clusters do indicate missing data.
- `result::AuthError { status }` represents HTTP 401/403 and is surfaced as
  `live::Status::AuthError`. `Worker::replace_key` re-runs the failed query using
  the existing OS credential path, retaining the previous rows until success.
  Replacing a key refreshes the previously loaded extent of the same query,
  including investigations that already loaded multiple pages.
  The panel's replacement action preserves draft inputs, filters and columns.
  Stale callbacks and keys entered for another draft source cannot replace the
  failed source's key; live authentication challenges retain their source identity.
- Clients created by `Kibana::new` bind to the source ID and endpoint. Recreate
  the client when switching sources, rather than reusing another source's key.
- `Kibana::field_values(&Source, &FieldValuesRequest)` returns `FieldValues`:
  top-N `FieldBucket { value: FieldValue, count }`, `missing_count`,
  `sum_other_doc_count`, optional `doc_count_error_upper_bound` and `status`.
  The request declares the scalar `FieldType` (including boolean mappings whose
  terms use numeric keys plus `key_as_string`), and a limit between 1 and 1,000.
  It uses the selected source, time range and enabled server filters.
- `FieldValue::{String,Number,Boolean,Missing}` keeps scalar types and missing
  distinct. Aggregation keys are exactly the server's keys, including lowercase
  keyword normalization. `FieldValue::filter(field)` creates a `term` filter
  against that same field, or a missing-value exists exclusion. Do not match
  normalized picker keys against original `_source` casing.
- `EventWindow::fields` and `result::describe_fields` expose flattened source paths,
  observed type sets, present counts and missing counts **over loaded rows**.
  These are source observations, not index mapping or whole-index count claims.
  Message-field selection belongs to the guided UI; source objects remain intact.

## Synthetic server and GUI QA

The shared debug/test-only `remote_log::fake` server checks all required headers,
rejects nested bodies and unrecognized routes, and serves the same id-in-body
polling contract used by the real connector. It includes cross-cluster identities,
large skipped-shard counts, remote cluster details, mixed source casing, nested and
dotted objects, missing numeric values and nanosecond cursors above 2^53.
Records deliberately omit message and use a long details field; choose details
in the existing column picker. Synthetic filters and index patterns are evaluated;
non-empty KQL is rejected explicitly rather than returning misleading empty rows. It reads
no files or customer data. It is excluded from release app builds.

Build only on an assigned build host through `okilum-slot`:

    cargo build --offline --locked -p okilum-core --example fake_kibana
    cargo build --offline --locked -p okilum-shell

Against a debug GUI build on the same machine, run the already-built binary:

    ./target/debug/examples/fake_kibana 127.0.0.1:5609 async

Use the actual Cargo target directory in place of `target` when using a slot.
The binary may also be copied alongside a matching-platform debug GUI build.
Choose Settings → Logs → Kibana sources, enter a name, endpoint
`http://127.0.0.1:5609`, pattern `remote-*:events`, time field `@timestamp`, then
Add source / import. Enter API key `test-secret` and the fixed range
`2026-11-01T00:00:00Z` to `2026-11-05T00:00:00Z`. Apply and load more pages.
The server starts with 603 events and adds one current and one late event every
two seconds. Start the applied query and choose **Follow fixed range**.

Scenarios: `sync`, `async`, `partial-id`, `partial`, `timeout`, `legacy`,
`lower-bound`. A wrong key produces 401; enter `test-secret` and choose
Replace API key. Use Stop during `timeout` to observe cancellation.
Debug/test sources permit HTTP only to literal loopback IP addresses; release
sources continue to require HTTPS. No fake-server controls ship in the app UI.

## Validation changes from #1441

PIT rotation/deletion and unreceived-PIT cleanup tests are replaced because those
resources no longer exist in the supported protocol. Their query retention,
identity, boundedness, cancellation, live reconciliation and outage-recovery
assertions are retained in shared-server transport/worker tests. Existing Discover,
source storage, immutable export and native panel recovery/selection/Start-consent
tests remain in place, with quality fields added to their fixtures.

### Live fixture synchronization (#2565)

Core live fixtures await the worker's update channel, rather than polling it
against a wall-clock deadline. An unexpected terminal update fails immediately;
process-level test watchdogs bound a genuinely stuck worker. Cancellation waits
for fake-server poll notifications, a Stop acknowledgement and worker sender
closure. The server holds each observed response until the test releases it,
proving Stop/Drop abort in-flight HTTP and preventing another poll from racing
with the observer. Server shutdown joins outstanding handlers before the final
poll-count assertion. These signals cover actual work and stale-generation rejection.

The private cycle clock is injectable in test builds. Ordinary fixtures freeze
it so host scheduling cannot accidentally turn a successful cycle into a timeout.
The deadline fixture first completes below budget, then advances to the exact
deadline after a page and checks that timed-out rows stay visible and following
stops. Production uses the same monotonic `Instant` clock and existing budgets.
No row-loss or stalled-cycle product race was established by this repair.

The synthetic server borrows matching records until serializing the final page,
and memoizes timestamp parsing in a bounded response-thread cache. Boundary replay
otherwise clones the whole matching window and parses the same timestamp/bounds
thousands of times per request; loaded debug fixtures can exhaust the real HTTP
budget before reaching their worker-completion checks. These changes preserve
query, ordering, totals and response data while keeping HTTP deadlines intact.
