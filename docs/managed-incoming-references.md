# Managed incoming references

Approved contract for [issue297](https://git.oklabs.uk/BeFeast/okilum/issues/297). Incoming references are a derived, read-only view of the current managed workspace. Engineering budgets below are explicit bounds, not measured performance claims.

## User behavior and coverage

Current saved managed Source/Live Preview → explicit Load incoming references → project-visible saved sources/body-wikilink rows → explicit Open source passage → existing Back. Collapsed outer Source panel; no note-body injection, per-render request, automatic rescan or polling. Refresh first page and Next page are explicit. Title/path/original excerpt and “May refer to this note” distinguish ambiguous rows. Before Load/Open, existing dirty/conflict/find/recovery/composition/navigation-owner guards apply; refuse with reason rather than queuing hidden work. An unsupported older backend shows capability unavailable.

Body wikilinks include display aliases, embeds and heading/block-qualified note targets; aliases are displayed labels, not a new frontmatter alias resolver. Skip code through existing prose_spans; exclude frontmatter relationships and Markdown links explicitly. Exact root path, shortest unique suffix, source-relative path and all-candidate ambiguity reuse the existing resolver. Self-links excluded. One row per(target,source,line); a precise link on that line wins over ambiguous occurrences and supplies displayed link. Stable path then one-based line order. The same SearchScope/record_visible logic as retrieval limits source rows; ordinary shared Markdown and permitted saved knowledge only, no raw context/session/dispatch/proposal record visibility expansion. Resolver identity inventory is unchanged; no hidden-candidate path list is returned to the UI.

## Wire identity

Existing workspace-v1 envelope and transport id/expected_workspace are mandatory; selected scope.goal_id must exist under Backend lock. Clone Arc<BrainIndex>, release Runner lock, then query/read sources. Advertise source_backlinks capability.

```json
{"schema":"ai-brain/workspace-v1","id":"request-id","expected_workspace":{"brain_id":"...","root":"...","records_dir":"records","managed":true},"op":"source_backlinks","path":"notes/current.md","expected_revision":"revision-from-source-read","scope":{"goal_id":"uuid","mode":"project"},"limit":10,"cursor":null}
```

Reuse revision strings from SourceSnapshot verbatim; no new revision format. Scope is existing SearchScope (including its validated include/exclude/prefix fields); first UI fixes mode=project and labels that coverage.

```json
{"target":{"path":"notes/current.md","revision":"..."},"index":{"status":"ready","generation":"...","observed_at":"..."},"freshness":"current_at_read","rows":[{"path":"notes/referrer.md","title":"Referencing note","revision":"...","start_line":17,"end_line":17,"excerpt":"Original source text containing [[current|label]]","excerpt_truncated":false,"link":"current","link_truncated":false,"ambiguous":false}],"next_cursor":null,"warnings":[]}
```

`index` uses existing IndexStatus, with its other existing fields preserved. Cursor is opaque, <=4096 bytes, binds target path/revision, complete scope, immutable generation and stable last row key. Validate the key against that generation; malformed or mismatched cursor refuses. Limit default10/range1..20. No global total or false completeness claim. Next page replaces the current bounded page only with unchanged response/request owner+target+revision+scope+generation; Refresh first page starts again. Pages are never accumulated into an unbounded UI list. A stale page preserves the current note and requests an explicit reload.

## Source and generation freshness

Extract occurrences from full original bounded SourceSnapshots during existing BrainIndex refresh BEFORE chunking; chunk exclusions/long-line limits cannot silently delete references. Build path lookup from that same inventory. Persist derived occurrences in its immutable cache generation and bump disposable cache schema. No second filesystem scanner, Reader Vault::scan path, per-query embeddings or alternate search engine.

Only ready, complete backlinks generation may answer. Validate target path is indexed and its indexed revision equals expected_revision; read target and returned sources with read-only SourceStore and exact revisions, then recheck ready status and the same generation. Abort the whole page on source disappearance/change, target mismatch or generation transition. Empty rows are authoritative only for a successful complete ready generation and allowed scope. Use existing index_not_ready/index_stale/source_stale/index_unavailable errors; cursor errors and backlinks_budget_exceeded remain explicit unavailable states. `current_at_read` is not an atomic filesystem snapshot and does not promise immediate refresh; existing background embedding publication delay remains visible.

## Bounds and budget choice

Reuse existing10,000 documents /1MiB each /64MiB total inventory. Page<=20 rows, excerpt<=1024 UTF-8 bytes around the actual occurrence on its original line, title<=256 UTF-8 bytes, displayed authored target<=512 bytes, brain-relative path<=4096 bytes. Set excerpt/link truncation flags; never derive navigation offsets from truncated display strings. Titles reuse first H1/path fallback. Target plus at most20 distinct source revalidations read<=21MiB canonical bytes under these existing source caps; that is a cap calculation, not a latency claim.

Set backlinks-derived budget<=100,000 expanded(target,source,line) edges and<=32MiB serialized occurrence data. These are conservative first-slice engineering limits, not measured capacity claims: 10 edges per max-size inventory document on average and at most half the existing64MiB source-inventory byte cap. Bound candidate expansion/count/storage as produced, not only after allocating a complete graph. If exceeded, store explicit backlinks-unavailable state for that generation; do not expose a silently truncated tail, and do not break otherwise usable lexical/semantic search. This separation is required because adding backlinks must not poison the existing index service.

## Known source passage navigation

Open uses the row's exact known source path, revision and one-based start/end lines. It never resolves the ambiguous displayed target. Existing source_read returns original bytes; verify revision and line bounds BEFORE installing the note/Back visit. Compute byte offset at start_line from exact LF/CRLF source, not excerpt text. Revision mismatch shows stale evidence and preserves the prior note/Back; user may explicitly reload references. Use existing SourceNavigation flight generation/owner/selection/scroll guards and after-paint landing. Preserve Source/LP mode where the existing source engine supports it; any existing over-limit fallback/refusal stays explicit. Back retains the original current note position and Context.

## Validation and activation

Extraction/resolution, visibility, bounds, pagination and revision/generation freshness require focused backend tests. The existing Source widget must prove explicit Load, honest ambiguity, revision-bound passage opening and Back, including stale/out-of-order ownership refusal. A new backend package is required for this API; an older backend must show capability unavailable. Matched source/backend artifacts and bounded native verification precede activation. Reading references performs no canonical writes or provider operations.
