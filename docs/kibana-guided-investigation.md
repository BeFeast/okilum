# Guided Kibana investigation (#2390)

Logs → Kibana sources opens the vault's environment list. Selecting a saved
source connects immediately using its OS-stored key. Add source accepts a name,
endpoint, index pattern and time field (default `@timestamp`); Discover import
remains available. A new source needs a key and Refresh before it is saved.
Keys are scoped to the vault, source identity and endpoint. Event data stays in
session memory.

The six presets select rolling intervals of 5 minutes, 15 minutes, 1 hour,
4 hours, 24 hours or 7 days. Each query freezes an absolute half-open window.
Refresh resolves a new window for a rolling selection; custom ranges remain
fixed. Custom uses calendar and hour/minute pickers in UTC. Advanced retains
KQL, complete Discover filter JSON, exact RFC3339/date-math inputs, source
settings, sort reversal and loaded JSONL export.

Pin scope field chooses scalar aggregatable fields discovered from Kibana's
`/internal/data_views/_fields_for_wildcard` catalog for the selected pattern.
Text fields require an advertised keyword multifield; the app never invents a
`.keyword` suffix. Missing/denied/conflicting mappings are surfaced. The pinned
field list is saved per source; selected values are session investigation state.
Values come from the field-values API over the applied absolute window and its
server filters, with typed keys, Missing, document counts, top-N remainder,
count-error bound and result quality. At most 1,000 values are returned, in a
virtualized list; omitted values are explicitly counted. All values removes
that scope restriction. Selecting another source clears session scope and
cannot reuse the previous source's keyed client.

The status reports the applied environment, absolute window, scope, connection,
loaded/total counts, version if readable and all result quality flags. Previous
rows survive failed queries and mid-session 401/403. Replace API key retries the
failed request while keeping the user's settings. Refresh cancels prior work;
stale value/catalog callbacks cannot replace a newer query's state.

Synthetic evidence establishes the loopback protocol and GPUI behavior only.
Native macOS OS-credential storage, corporate compatibility and the Mac beta
built from main remain delivery checks outside a Linux GPUI test result.

The field-catalog route and version `1` are checked against the same pinned
Kibana source used by the search transport:
[route and response schema](https://github.com/elastic/kibana/blob/abf4eb613d64cdb3a6290671ce1cb67399677ab3/src/platform/plugins/shared/data_views/server/rest_api_routes/internal/fields_for.ts),
[path constant](https://github.com/elastic/kibana/blob/abf4eb613d64cdb3a6290671ce1cb67399677ab3/src/platform/plugins/shared/data_views/common/constants.ts#L74).
The catalog may be denied independently of history. The picker reports that
failure; it does not guess a field mapping. Nested mappings require nested query
semantics and are excluded from these scalar pickers.

## Investigation chips and counts

Cell Include/Exclude, details and header value filters default to **Kibana** in
this panel. Their chips compile to typed Elasticsearch term/exists clauses and
re-query the whole selected window. Multivalue Include is OR; Exclude negates the
whole group. Disabled and Loaded rows chips never enter a server request.
Keyword normalization stays on the server. In particular, a lowercase key from
an aggregation is not compared locally with differently cased `_source` text.
Ambiguous keyword multifields, incompatible scalar types and unsupported fields
show an error and retain the previous rows; select an explicit field or switch
that chip to Loaded rows.

Each chip's Kibana / Loaded rows badge switches execution. A header or chip
picker separately switches **Kibana counts** / **Loaded row counts**. The first
covers the applied absolute window and enabled server filters; the second counts
only downloaded records, preserving their original values. Labels include the
window, top-N remainder and aggregation error bound. The existing advanced Log
query, text, level and histogram controls remain local to loaded rows.

## Synthetic demo script

Use a debug build and the shared fake server documented in
[the transport contract](kibana-internal-search.md#synthetic-server-and-gui-qa).
No corporate endpoint or key is needed.

1. Open the app menu → Logs → Kibana sources → Add source.
2. Enter name `Synthetic`, endpoint `http://127.0.0.1:5609`, index pattern
   `remote-*:events`, time field `@timestamp`. Click Add source / import.
3. Enter key `test-secret`. Click Last 24 h (while the fixed synthetic events
   fall in that window), or Custom and select 1 November 2026 00:00 through
   5 November 2026 00:00, UTC, then Apply range.
4. Click Pin scope field → `service.keyword` → `"checkout"`. The values/counts
   come from the server; source rows still contain `Checkout`.
5. Open Columns and select `service`, then `duration`. Click the `service`
   header to compare Kibana counts with Loaded row counts. Select a value and
   Apply filter; close the picker with Escape.
6. Right-click a `duration` cell → Include this value. The server total and
   window counts narrow with the query. Right-click the remaining value →
   Exclude this value to demonstrate an empty intersection.
7. Click a chip to edit its values, Include/Exclude or Enabled state. Use its
   Kibana / Loaded rows badge to change execution, × to remove, or Clear all
   to clear investigation chips. The pinned scope remains a separate control.
8. Refresh re-queries the current selection. A 401/403 exposes Replace API key;
   entering a replacement and clicking it retains columns, scope and chips and
   retries the failed request.

Custom controls have minute precision; Advanced accepts exact RFC3339 timestamps
and explicit offsets. Date, nested, array/object and null server chip semantics
are not inferred from display values: unsupported mappings require Loaded rows.
The #2398 investigation stack is integrated with these controls. Follow this
request clears the parent table chips into Back, keeps opaque base filters and
pinned scope, and queries the same applied absolute window in chronological order.
The selected rolling preset remains visible; an explicit Refresh resolves it again.
Back (Cmd+[ on macOS, Alt+Left elsewhere) restores the exact parent snapshot,
chips, columns, fields, selection and viewport without querying the server.

Saved views retain both the applied remote query and a separate guided context:
the credential-free source definition, original time preset and typed selected
scope values. Scope/chip/request DSL is never copied into the Advanced base
filters. Restoring a rolling view resolves a new window; a fixed view keeps its
exact bounds. Older #2398 views without guided context retain their original DSL.
Navigation owns a single worker command; internal chip restoration cannot race it
with a second Apply. Counts remain bound to the restored dataset and window.

To continue the synthetic demo after Include/Exclude:

9. Clear the table chips, select a row and open its details → Follow this request,
   or right-click a request-id cell → Follow this request. Back restores the parent;
   repeat and use Cmd+[ on macOS (Alt+Left elsewhere).
10. Message: Choose message field → details; the row stays single-line and details
    retain wrapped full text. Use as correlation field in a cell menu remembers
    the chosen field per source.
11. Start applied query → Follow fixed range (or explicitly choose rolling) → Stop.
    Chips, columns and request scope remain in force; new identities appear once.
12. Saved views (star) → Save current view… → enter a name → Save. Choose the name
    from the star menu to restore source, time preset, pinned scope, chips, columns,
    message/correlation fields and request identity.

## Search-only field discovery and diagnostics (#2437)

When the field catalog is denied, absent, malformed or empty, field pickers use
only POST /internal/search/es. The selected window supplies at most 128 sampled
_source documents. Object paths are flattened and up to 256 array/type observations retained.
Up to 24 scalar paths are checked in batches of eight aggregation candidates;
strings test both the base and .keyword field. Failed combined requests are
bisected to isolate unsupported fields. At most 64 probe requests share an eight
second sample/probe budget. Only nonempty, successful terms buckets without explicit partial failures or timeouts confirm a mapping. Unconfirmed fields are explicitly limited to loaded
row filtering. The sample can miss rare fields; widening the window can help.
Confirmed fallback discoveries are cached per source/dataset for the window
session, with at most 16 cache entries. API key replacement clears this cache.
An empty sample is not cached so a later wider window can discover fields.

Scope and server chips use the confirmed field and the exact typed aggregation
key, including normalized string keys. Missing uses exists; no source-text case
conversion is used to apply a server chip locally.

Open Logs, choose the saved environment, then click Connection details beside
Advanced. Copy connection details copies the last connection attempt's route
outcomes, safe Kibana version, total relation, shard/cluster counts and timeouts.
The endpoint and pattern are redacted by default. Document values, cluster names,
credentials and raw server errors never enter this diagnostic text. Cached
fallback discovery is marked explicitly; routes not used in this attempt say
not attempted. Catalog permission denial alone does not require replacing a key
when the search route still works.

The debug fake_kibana example accepts catalog-403, catalog-404, catalog-empty and
catalog-malformed modes. Its synthetic fixtures include a base text field whose
keyword multifield succeeds with lowercase keys, a text-only field, nested
objects, arrays, booleans and a numeric field missing in half the documents.
Synthetic tests do not certify a private network, scoped key or native macOS
credential storage; Connection details is the evidence to collect there.
