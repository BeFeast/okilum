# Remote Log view: accepted requirements and compatibility boundary

Issue: [#1440](https://git.oklabs.uk/BeFeast/okilum/issues/1440).
Accepted requirements recorded on 2026-10-20. This document records the product
contract; the compatibility runner does not implement these native features.

## Sources, credentials, and imported queries

- Sources belong to the current vault. Credentials belong in OS credential
  storage, scoped to the vault and source, not in Markdown, URLs, or exports.
- Import the complete Discover `_a` and `_g` state: data view, query language and
  query, enabled/disabled/negated/pinned filters, columns, sort, time and refresh
  state. Surface unsupported state explicitly. Do not silently drop filters or
  replace KQL with Lucene `query_string`.
- KQL and filters are edited together and take effect through Apply. Invalid or
  unsupported semantics must produce an explicit error while preserving the
  previous applied query.
- T3 opens an Okilum deep link in the right vault window, carrying the fixed
  query and absolute time interval. Merely opening a Discover URL is insufficient.
  This requirement does not authorize edits to the existing deep-link work.

## History, following, and session data

- Load bounded history before following; Start is an explicit user action.
- An absolute time interval stays fixed. Ask before switching it to a rolling
  interval for live following.
- Preserve imported sort, support reverse sort and lazy paging, and expose
  reconnecting and caught-up states. A background tab continues until closed.
- Include late arrivals whose event time falls inside the selected period.
  Timestamp watermarking alone cannot satisfy this requirement.
- Keep loaded events in session memory only, including all fields. JSONL export
  includes only loaded records, all their fields and document identities; it must
  not imply an exhaustive server export.
- Deduplicate by `(_index, _id)`. Use PIT plus the exact `date_nanos` timestamp
  and `_shard_doc` cursor for stable paging; preserve cursor JSON values verbatim.
- The five-second target begins at server-searchable visibility. Query latency,
  upstream ingest delay, polling delay and UI delay are separate measurements.
  Event timestamps or overlapping ranges alone cannot establish this target.

## Bounded compatibility evidence

The runner is an Integration-only diagnostic artifact. It uses the already
installed Python and curl and sources an existing local API-key reference. It
does not install software, change credentials or VPN, inject events, probe
Production, or export historical user data. Event payload stays on the machine
running the checks. Publish only the nonsecret runner/tests and a separately
reviewed schema/status/timing receipt.

Checks cover data-view field types (including candidate ingest clocks), one
five-record current-time all-fields search, shard/cluster/partial/async states,
remote version where authorized, a server-side KQL candidate against known DSL,
at most three twenty-record PIT pages over a fixed five-minute range, and at most
six polling requests within sixty seconds. Cleanup closes the latest scalar ID
for the one logical PIT and outstanding async IDs even after a failed search.
Rotated IDs are superseded handles, not separate logical PITs. Failures remain visible;
PIT keep-alive expiration is a fallback, not proof of successful cleanup.

KQL comparisons cover empty, keyword correlation, conjunction/negation and
phrase cases when suitable retained samples and mappings exist. Bounded ordered
sample agreement and exact totals where available do not prove general KQL
parity. Unsupported candidate syntax remains explicit; ES|QL is not implicitly
certified. A frozen PIT snapshot prevents changing data from masquerading as a
KQL mismatch. No completeness claim is permitted without an independent oracle.

`HTTP 200` is insufficient: console proxy preserves Elasticsearch status in
`x-console-proxy-status-code`, which must be checked independently. Partial CCS,
skipped/failed clusters, failed shards,
running/partial results, timeouts, unknown status metadata and failed cleanup
must remain visible. `401`, a blocked internal route `400`, ordinary request
validation `400`, and network timeout have distinct outcomes.

Kibana internal routes are version-specific. For the researched 9.5.4 route,
PIT belongs in `params.pit` so the strategy selects PIT defaults; `index` is
omitted for PIT searches. Async polling takes its ID from the URL route, not
solely from a body `id`. Re-check compatibility on version changes.

Synthetic HTTP controls validate runner behavior and are labeled synthetic.
They never establish live Integration compatibility. If no authorized local Mac
execution interface exists, deliver the exact public download-and-run command
for Terminal on the Mac and keep live acceptance explicitly unexecuted.

The [20 October actual Mac receipt and cleanup follow-up](remote-log-compatibility.md)
records observed capabilities, two proven instrumentation/request faults and the
remaining UNKNOWN historical cleanup cause. That original receipt remains
blocked solely on unconfirmed cleanup; the new targeted diagnostic cannot
rewrite its verdict or establish general KQL/late-event/five-second acceptance.

## First implementation slice (#1441)

Settings → Logs → Kibana sources opens a vault-scoped remote source window
using the existing Log view rows, details, columns, histogram and selection.
Imported sources are saved under the existing application state directory, in
`remote-log-sources/<canonical-vault-path-hash>.json`. A moved vault path currently
has a separate local source scope. Keys use GPUI's OS credential API with a
vault/source/endpoint identity; there is no file fallback.

History loads 200 rows per requested page, up to 10,000 identities or 16 MiB of
original serialized events. Responses are capped at 8 MiB; normalized display
snapshots at 64 MiB. Original event objects are shared across page snapshots.
The server sort, including descending order, and exact cursor values are retained.
A snapshot closes when exhausted, limited, idle for 45 seconds, replaced or the
window closes. An expired or failed snapshot requires Apply; it does not silently
change the previously applied range or pretend paging continued.

API requests run on a dedicated worker; HTTP connect/total deadlines are 3/10
seconds and async search polling is capped at two additional requests. Closing the
window cancels subsequent work and closes owned handles after any in-flight
request finishes. Cleanup failures are shown while the window is open.

The server-side KQL route is used directly, without a Lucene substitute; general
Discover parity is explicitly unverified. Live Start is visibly unavailable until
a late-arrival-safe strategy is established. Event-time watermarks and a short
overlap are not treated as that strategy. T3 remote payload encoding/parsing is an
independent tested seam; production vault routing waits on the owner of #1414.
No deep-link source from PR #1438 is modified by this slice.


## Live reconciliation slice (#1441)

Start is explicit and applies to the last successfully applied query. A fixed
historical range first offers a rolling window of the same exact duration or
continued reconciliation of the fixed range. Import refresh/pause never starts
network following. The session continues independently of window focus; closing
its window cancels the request and worker. Apply, including an invalid draft,
stops the old generation while retaining the previous applied query and rows.

Each cycle opens a **new PIT**, freezes that cycle's absolute range, and traverses
ALL pages in the imported sort using verbatim nanos/search_after values and the
PIT tie-breaker. The cursor is never reused across cycles. A document becoming
searchable behind the previous cycle's cursor is included by a later complete
cycle if its event time remains inside that cycle's selected range. No ingest
clock or positive timestamp offset is assumed. This relies on the already
researched PIT snapshot/paging contract; synthetic real-HTTP controls exercise
the worker against an independent frozen-snapshot server, not corporate latency
or universal KQL parity.

Coverage is a completed PIT snapshot, not continuous present-time completeness.
The UI shows Catching up and scanned pages/events, cycle start/range/duration,
reconnecting, incomplete, stopped and terminal errors. A complete cycle atomically
replaces the displayed window; a failed/partial/bounded cycle retains the previous
snapshot and does not silently evict unseen older pages. Sort, identities, details,
selection and marks follow document identity within the same source/endpoint/data
view. Rolling windows deliberately expire events outside their stated interval.
Offline intervals outside the current selected window are not recovered.

Polling waits two seconds AFTER completion, with only one request in flight.
Outages before resource-producing search reconnect with bounded backoff and full-window paginated reconciliation;
no missed-interval shortcut or timestamp watermark is used. Known PIT/async IDs
remain owned until successful cleanup; reconnect retries cleanup before acquiring
another PIT. Failure to receive an opened PIT ID stops automatic retry with an
explicit unconfirmed-cleanup error. Handle acquisition is shielded from local
cancellation for its bounded request deadline, so a returned stale handle can be
closed. Ordinary search requests are actually aborted when the generation is
cancelled. Cleanup has its own bounded deadline and remains possible after cancel.

The existing 10,000-event / 16 MiB serialized-event / 64 MiB display limits remain.
A full cycle exceeding them, or 60 seconds, stops following with **Incomplete** and
asks for a narrower range. Even an exact limit is treated conservatively as
incomplete until exhaustion is established. No lower page is silently dropped to
claim coverage. There is one candidate plus the previous displayed snapshot and a
bounded latest-command/latest-update mailbox, not an accumulating poll queue.
Large 24-hour windows can take many pages or exceed these bounds; this design
makes **no five-second guarantee**. Visibility-to-UI latency, native keychain,
corporate-source UI acceptance, T3 routing and delivery remain separate evidence.

An interrupted search can leave its newest PIT/async ID unreceived. The existing
live receipts do not establish that closing an older acknowledged ID frees all
possibly rotated contexts. The client attempts known cleanup, reports cleanup
unconfirmed, retains the previous rows, and blocks new PIT acquisition in that
session. It does not claim unrestricted mid-search reconnect or certified cleanup
for unreceived handles. This is a concrete compatibility gap tracked with #1440,
not a successful cancellation-cleanup claim or a reason to substitute an event
watermark. Normal complete cycles and safe pre-search reconnect remain available.
