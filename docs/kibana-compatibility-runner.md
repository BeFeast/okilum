# Kibana compatibility runner (#1440)

The runner checks the current Remote Log connector ([internal-search sources](kibana-internal-search.md))
against real Kibana and Elasticsearch releases. It runs on GitHub-hosted
runners only and starts its own containers. It is a compatibility check for
the protocol, not product acceptance. It never contacts a real, corporate or
homelab deployment.

## What runs

`.github/workflows/kibana-compat.yml` builds the debug-only example
`crates/okilum-core/examples/kibana_compat.rs` once. Each version in the
matrix then runs on its own hosted runner, where
`scripts/ci/kibana-compat.py`:

1. refuses to start unless `RUNNER_ENVIRONMENT=github-hosted`;
2. starts one single-node Elasticsearch and one Kibana from images pinned by
   both tag and index digest (table `IMAGES`), with security enabled and
   loopback-only published ports;
3. generates random passwords for `elastic` and `kibana_system`, then mints a
   one-hour API key restricted to `read`/`view_index_metadata` on the
   synthetic indices and Discover read. Values go through the environment,
   are masked in logs, and disappear with the runner;
4. creates the data view `okilum-compat` and runs the example, which seeds
   600 synthetic documents and drives the **product** code
   (`remote_log::transport::{Kibana, History}`, `Kibana::field_values`,
   `live::Worker`);
5. writes a per-version JSON report and a job-summary table, then removes the
   containers.

The lane is opt-in and never blocks a merge. It runs weekly (Sunday 01:35
UTC), on `workflow_dispatch` once merged, or when a `kibana-compat/**` branch
is pushed to the GitHub mirror. It uses no repository secrets and saves no
caches.

Versions: 9.5.4 (the Integration version recorded in
[remote-log-compatibility.md](remote-log-compatibility.md)), 9.5.5 and 8.19.25
(the newest 9.x and 8.x releases), and 8.17.10 (an older maintained 8.x line).
To add a version, add its digests to `IMAGES` and its tag to the workflow
matrix. `scripts/ci/test_kibana_compat.py` checks that the two lists match.

## Bounds

| Resource | Bound |
|---|---|
| Elasticsearch | 1.5 GiB container, 512 MiB heap, 2 CPUs, 3 min to become healthy |
| Kibana | 2 GiB container, 1 GiB Node heap, 2 CPUs, 5 min to become available |
| Runner | 4 min self-imposed (exit 3), 5 min hard limit; live phase 60 s |
| Data | 600 documents plus 6 conflict rows and 1 late event |
| Jobs | build 30 min, each version 20 min |

## Checks

Each check reports `pass`, `fail`, `unsupported` or `info`. `unsupported`
means the server rejected the capability explicitly and the connector
surfaced the error. That is the product contract for unsupported KQL, not a
failure. Only `fail` fails the job.

| Check | Evidence |
|---|---|
| `data_view_resolution` | `/api/data_views/data_view/{id}` resolves to the index pattern and time field |
| `kibana_version` | the optional `/api/status` version equals the image tag; unreadable is `info` |
| `history_ascending`, `history_descending` | the full traversal returns exactly the 600 identities: no duplicates or gaps, in server order, with an exact `eq` total and every shard successful. 250 rows share one nanosecond timestamp across five shards and the 200-row page boundary, which exercises boundary replay. Nanosecond `date_nanos` values are preserved |
| `kql_keyword`, `kql_conjunction_negation`, `kql_phrase` | server-side KQL (`{"kql":…}` in the DSL) returns the same set as the equivalent DSL filters and the synthetic expectation. If the server rejects it explicitly, the outcome is `unsupported` |
| `kql_invalid` | malformed KQL is an explicit error, never rows |
| `shard_failure` | a term query that fails the `long` shard of a keyword/long conflict is reported as partial (failed shards, incomplete) or as an explicit error, never as a complete result |
| `field_values` | top-N buckets and missing counts for keyword, `long` (with missing values) and boolean fields match the seed |
| `unauthorized` | an unknown API key surfaces as the typed `AuthError` 401 |
| `late_arrival` | Apply, then follow a fixed absolute range. A document inserted with `refresh=wait_for` has an event time inside the window and before rows already loaded. It must appear once, with no duplicates, while following continues. The time from searchable to visible is reported |

The live latency is measured on loopback with 600 rows. It is not evidence for
the five-second production target, which stays unproven on a real
deployment. Cross-cluster search, very large windows and real ingest lag are
not covered here. The synthetic `remote_log::fake` tests cover the CCS response
shapes.

OpenSearch is out of scope: OpenSearch Dashboards does not serve Kibana's
`/internal/search/es` route, so the connector does not support it.

## Running it

After merge: Actions → `kibana-compat` → Run workflow. Before merge, push the
commit to a `kibana-compat/<topic>` branch on the GitHub mirror. Reports are
kept as `kibana-compat-report-<version>` artifacts for 30 days.

## Results

See the results section added with the implementing PR.
