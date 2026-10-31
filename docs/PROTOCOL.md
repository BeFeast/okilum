# okilum-cored JSON-lines protocol — v0.3

One JSON object per line on stdin; one JSON object per line on stdout. On start
the daemon prints `{"ready":true,"notes":N}` once, then answers requests in
order. Every response echoes `id` and carries `ok`.

The vault comes from `--vault` or `OKILUM_VAULT`, the index from `--index-dir`
or `OKILUM_INDEX_DIR` (default `<vault>/.okilum-index`). There is no default
vault: guessing one is a good way to index the wrong directory.

`okilum-cored index --vault <path>` rebuilds the persistent tantivy index and
exits. The index is derived data — deleting it is always safe.

`okilum-cored mcp` serves the same operations over MCP (see below), and
`okilum-cored brain …` serves the separate `ai-brain/v1` transport (see the end
of this document).

These are invocation details, not part of the frozen wire contract below. The
spike named them `OKILUM_CORPUS`/`OKILUM_INDEX` and defaulted to a path inside
the sandbox.

## Ops

| Request | Response payload |
|---|---|
| `{"id":1,"op":"list"}` | `notes: [{path, title}]` |
| `{"id":2,"op":"render","path":"Dev/x.md"}` | `html` (full document body HTML), `title` |
| `{"id":3,"op":"doc","path":"Dev/x.md"}` | `doc` (block IR, see `crates/okilum-core/src/ir.rs`) |
| `{"id":4,"op":"backlinks","path":"Dev/x.md"}` | `backlinks: [{path, title, context}]` |
| `{"id":5,"op":"search","q":"maestro proxy"}` | `hits: [{path, title, score, snippet_html}]` |
| `{"id":6,"op":"resolve","target":"Some Note"}` | `path` (or `null`) and `candidates: [path]` |
| `{"id":7,"op":"explain","q":"maestro proxy","path":"Dev/x.md"}` | `explanation` (scoring tree as JSON text) or `null` when the note is not a hit |

## Link conventions

- Wikilinks render as `<a href="okilum://open/<rel-path>">` (IR: `Link.href`).
- Unresolved wikilinks: `okilum://unresolved/<name>` — style dimmed/red, no-op or message on click.
- Local images become absolute `file://` URLs (IR `Image.path`: absolute filesystem path).
- Snippet highlight in search hits: `<b>term</b>`.

## Acceptance subset (what any client must render)

headings, wikilinks (click → navigate), callouts (`> [!note]` GitHub-alert
style), tables, task lists, fenced code with syntax highlighting, local images.
Plus: backlinks panel, search with snippets + jump-to-match.

## Contract status

**v0.3 (2026-09-04).** Adds `explain` — purely additive, nothing existing changes shape. A v0.2 client that never sends `explain` sees no difference.

**v0.2 (2026-09-02).** v0.1 was frozen on 2026-08-31; the protocol outlives the
spike by design, so the MCP adapter around `okilum-cored` and any future shell
or client consume it as-is, and changes require a version bump. This is that
bump, and the only one so far.

### v0.1 → v0.2: `resolve` can say "several"

Note identity is the path from the vault root, and a link is a suffix of one, so
a link can name more than one note. v0.1 had nowhere to put that: `path` was the
answer or `null`, which forced the daemon to pick a winner the caller could not
see. `resolve` now also returns `candidates`.

```json
{"id":6,"ok":true,"path":"notes/alpha.md","candidates":[]}
{"id":6,"ok":true,"path":null,"candidates":["dup/alpha.md","notes/alpha.md"]}
{"id":6,"ok":true,"path":null,"candidates":[]}
```

resolved · ambiguous · unresolved. `candidates` is sorted by path and stable
across runs.

**`path` keeps its v0.1 meaning exactly**, which is what makes the change safe:
an ambiguous target reports `path: null`, so a v0.1 client reads it as
unresolved — wrong, but visibly so, and it renders a dead link rather than a
confident wrong one. Returning one candidate as `path` would have kept old
clients silently mistaken, which is the failure this whole change exists to
remove.

The wire contract is the ops table and the link conventions above. How the
process is invoked — flags, environment variables, defaults — is not part of it
and has already changed once, at the port out of the spike.

## MCP — the same core, a second wire

`okilum-cored mcp` serves the Model Context Protocol (JSON-RPC 2.0 over
stdio, protocol revision `2025-06-18`) instead of JSON-lines. Every tool maps
one-to-one onto an op above, through the same functions — the point is that
agent tooling and the JSON-lines client cannot disagree about the vault.

| Tool | JSON-lines op |
|---|---|
| `list_notes` | `list` |
| `search` | `search` |
| `read_note` | (frontmatter removed and links rewritten — rendering input, not lossless source) |
| `render_note` | `render` |
| `note_blocks` | `doc` |
| `backlinks` | `backlinks` |
| `resolve_link` | `resolve` — returns `kind: resolved | ambiguous | unresolved` |
| `explain_hit` | `explain` |

Only `initialize`, `ping`, `tools/list`, `tools/call` and the
`notifications/initialized` no-op are implemented. Other methods return
`-32601` rather than pretending. The MCP wire is versioned by the MCP spec
revision, not by this protocol's v0.x; the two evolve independently.

## Read-only boundary

Every op in the table above and every MCP tool only reads the vault. None of them
creates, edits, renames or deletes a note, and adding one that does is not a v0.x
bump: it is out of scope for this protocol. Writing in the desktop app (source
editing, rename/move, Tasks, Board management) is implemented in the app itself,
with revision-checked atomic saves, and is governed by the contracts listed in [PRD §2](PRD.md#2-canonical-data), not
by this wire.

## AI Brain — a separate transport, not reader operations

The approved [AI Brain POC](ai-brain-poc.md) uses a separate
[ai-brain/v1 contract](ai-brain-contracts.md) for exact source read/write and
goal/stage orchestration. It is implemented and served by its own entrypoint,
`okilum-cored brain`, as `ai-brain/v1` JSON-lines over an explicitly selected
loopback TCP address; see [the local service](ai-brain-service.md) and the
[application API](ai-brain-application-api.md). Those requests carry
`schema: "ai-brain/v1"` and are not commands in the ops table, MCP tools, or a
v0.3 wire extension; existing reader requests and responses remain unchanged.

In particular, MCP `read_note` strips frontmatter and rewrites wikilinks. Never
feed it back into a source save or exact export. Rendering and exact source access
are separate capabilities.
