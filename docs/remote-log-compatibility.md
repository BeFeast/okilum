# Remote Log view: verified compatibility evidence (20 October 2026)

Product implementation is tracked by #1441. #1440 tracks compatibility evidence,
not product or live completeness acceptance. No corporate Mac changes or new
Mac diagnostic run are required for the unchanged capabilities below.

The original actual Mac run, 15:19:33–15:20:35 Israel, observed Integration
Kibana 9.5.4, connected CCS `main` Elasticsearch 9.5.4 with
`skip_unavailable:false`, a data view with 1,035 fields and `@timestamp` mapped
as `date_nanos`. The five-record sample retained all fields. Three PIT pages of
20 rows produced 60 distinct document identities, no duplicates and an unchanged
exact cursor. Four nonempty KQL/DSL ordered 20-row samples agreed. Totals reported
`gte 1000`: these samples establish no general KQL parity or exhaustive counts.
The original outcome remains BLOCKED solely by unconfirmed cleanup; the receipt
and historical verdict are immutable. Its six ascending oldest-page polls over
a fixed range did not exercise an active tail.

A **new**, actual targeted cleanup run at 15:50:24.290–15:50:29.566 Israel took
5.277 seconds and three requests. All outer and backend statuses were 200;
the PIT open reported 273 successful shards and one successful CCS cluster;
the search returned one row; closing the latest scalar PIT ID returned
`succeeded:true,num_freed:273`. Failures were empty and cleanup CONFIRMED.
This closes the new diagnostic's resource lifecycle only. It does not rewrite
the original receipt or establish live or general KQL acceptance.

Transport for the researched version uses `/internal/search/es`, DSL parameters,
top-level `params.pit` with `index` omitted for PIT search, and async IDs in the
route path. Headers are ApiKey, `kbn-xsrf`,
`x-elastic-internal-origin: Kibana` and API version 1. The console proxy provides
PIT open/close; outer HTTP 200 can wrap an ES failure, so
`x-console-proxy-status-code` is checked independently. PIT close submits the
latest **scalar** ID. Partial/running/failed/skipped shard or cluster results,
unknown completion metadata and unconfirmed cleanup remain visible failures.
Changing Kibana version requires a new compatibility decision; the initial
product slice refuses versions other than 9.5.4.

Candidate fields `@tsm.es_ts`, `@tsm.lgs_ts` and `@tsm.pp_ts` are mapped dates.
The sampled `es_ts` offsets were 27.919–188.779 seconds after event time for
20/20 records. Their authoritative ingest semantics are **UNKNOWN**. No timestamp
watermark or short overlap may claim to include arrivals an hour late. The
five-second target starts at server-searchable visibility; upstream ingest lag
is a separate measurement and no broad latency bound has been established.

Production environments us2/eu2/apj1 returned 401 using their existing keys;
they remain UNVERIFIED and do not block Integration history implementation.
No new keys are requested. There is no accessible workstation ES endpoint;
the actual corporate source requires the operator's Mac VPN. Product transport
tests use loopback HTTP and synthetic data, not an invented live connection.

Evidence supplied and inspected by the manager:

- `mac-run-20261020-151933-receipt.json`
- `mac-pit-cleanup-20261020-155029-b86279-receipt.json`
- Research report `2026-10-20_concur-kibana/REPORT.md`

The receipts stay in the existing research evidence directory outside this
public repository. No credentials, event payloads or research scripts are copied.
