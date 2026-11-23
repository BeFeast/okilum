# okilum-cored JSON-lines protocol — v0.3

One JSON object per line on stdin; one JSON object per line on stdout. On start
the daemon prints `{"ready":true,"notes":N}` once, then answers requests in
order. Every response echoes `id` and carries `ok`.

The vault comes from `--vault` or `OKILUM_VAULT`, the index from `--index-dir`
or `OKILUM_INDEX_DIR` (default `<vault>/.okilum-index`). There is no default
vault: guessing one is a good way to index the wrong directory.

`okilum-cored index --vault <path>` rebuilds the persistent tantivy index and
exits. The index is derived data — deleting it is always safe.

`okilum-cored mcp` serves reader operations and declarative views over MCP (see below), and
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
| `{"id":4,"op":"backlinks","path":"Dev/x.md"}` | `backlinks: [{path, title, context, link, ambiguous, property?}]` — see below |
| `{"id":5,"op":"search","q":"maestro proxy"}` | `hits: [{path, title, score, snippet_html}]` |
| `{"id":6,"op":"resolve","target":"Some Note"}` | `path` (or `null`) and `candidates: [path]` |
| `{"id":7,"op":"explain","q":"maestro proxy","path":"Dev/x.md"}` | `explanation` (scoring tree as JSON text) or `null` when the note is not a hit |

### `backlinks` entries

Each entry is the serialized `Backlink` (`crates/okilum-core/src/vault.rs`):

| Field | Meaning |
|---|---|
| `path`, `title` | The note that contains the link. |
| `context` | The source line that contains the link, raw Markdown with surrounding whitespace trimmed and cut to its first 240 characters (`graph_links` in `crates/okilum-core/src/vault.rs`). On a longer line the link itself can fall past the cut, so `context` may not contain it. |
| `link` | The link target as written in that source (`[[target\|alias]]` records `target`), so a client can find it inside `context` without re-resolving, when the cut above kept it. |
| `ambiguous` | `true` when the link named several notes and this note is only one of them. A client must show it as ambiguous, never as an ordinary backlink (PRD §4). |
| `property` | Present only when the link sits in the source's frontmatter (`related: [[x]]`): the property key (#386). Omitted otherwise. |

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

**v0.3 documentation correction (#1943).** `backlinks` entries carry `link`,
`ambiguous` and an optional `property` in addition to `path`, `title` and
`context`. The daemon already serves them; this table previously listed only the
first three. The fields are additive, so a client that reads only
`path`, `title` and `context` keeps working, but it cannot tell an ambiguous
backlink from an ordinary one. No version number was assigned when they shipped;
whether this record warrants a v0.4 bump is the protocol owner's decision.

**v0.3 (2026-09-04).** Adds `explain` — purely additive, nothing existing changes shape. A v0.2 client that never sends `explain` sees no difference.

**v0.2 (2026-09-02).** v0.1 was frozen on 2026-08-31; the protocol outlives the
spike by design, so the MCP adapter around `okilum-cored` and any future shell
or client consume it as-is, and changes require a version bump. This was the
first bump; v0.3 above is the second.

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
stdio, protocol revision `2025-06-18`) instead of JSON-lines. MCP and JSON-lines
use the same reader functions. The view tools use the same functions as the
`view` subcommand.

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
| `run_view` | See [Declarative views](#declarative-views) |
| `validate_view` | See [Declarative views](#declarative-views) |

Only `initialize`, `ping`, `tools/list`, `tools/call` and the
`notifications/initialized` no-op are implemented. Other methods return
`-32601` rather than pretending. The MCP wire is versioned by the MCP spec
revision, not by this protocol's v0.x; the two evolve independently.

## Declarative views

```sh
okilum-cored view @views/questions.md --format json --as-of 2026-11-24T01:30:00+02:00
okilum-cored view @views/domain.md --arg domain=Health --format md
okilum-cored view --validate @views --coverage
```

The CLI and MCP tools take vault-relative paths. Use `--vault <path>` or
`OKILUM_VAULT` to select the vault. Runs default to JSON and the current time.
Pass an RFC 3339 `--as-of` to reproduce time-dependent results, repeat
`--arg KEY=VALUE` to bind parameters, and use `--all-fields` to include all
projected fields. JSON uses the `view_api: 0` envelope. Markdown renders the
same result with links to sources.

Validation accepts a file or recursively checks Markdown files in a directory.
It returns JSON with `valid` and `findings`. Each finding carries a path,
one-based line and column, severity and message. `--coverage` also lists the
note paths missing each `where` field in each evaluable section's source scope,
before the `where` filter and row limit. CLI exit codes are `0` for success,
`1` for error findings, and `2` for usage or I/O errors.

MCP returns these payloads as JSON in its text content:

- `run_view {path, args?, as_of?, all_fields?}` accepts an object of string or
  number arguments, an RFC 3339 timestamp and a boolean. It returns the same
  JSON as the CLI. Errors in individual sections remain in the result.
- `validate_view {path}` returns the validation report. Callers can inspect
  findings even when `valid` is false.

View runs evaluate actions as row metadata. The CLI and view tools only read
the vault and use an in-memory search index when a view uses `match`.

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
