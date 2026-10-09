# Exact brain export

The alpha export is a **knowledge archive**, not an execution backup. Its tar
contains `brain/<original relative path>` for every included saved file and
`manifest.json` beside that directory. Extract the tar with an ordinary archive
manager, then open `brain/` as Markdown and media. No Okilum database, provider
connection or session is needed to inspect the files.

The manifest records `tessera-knowledge-export/v1`, the original relative paths,
byte lengths and SHA-256 revisions, exclusions, and unresolved/ambiguous/external
local dependencies. Canonical goal, conversation, context and result Markdown
retain their original bytes, including historical provider identities. Their
presence is not authority to dispatch, resume work or claim restored sessions;
`execution_restored` is always false. Unsaved editor drafts and operational
journals are not part of this archive.

## Boundary and consistency

The selected root must be managed. Export holds the same process-safe writer lock
as source edits. Its caller must also hold the brain transaction owner while any
pending canonical projections are drained, so an export cannot split a managed
multi-record update. Every path is read through directory descriptors with
`NOFOLLOW`, including intermediate directories. Symlinks are omitted and named in
the manifest; their targets are never read or imported. Unsupported non-regular
files, unreadable files and invalid paths cause failure.

The exporter performs a second inventory of paths and content revisions before
publication. An observed addition, removal, content change or changed exclusion
causes an explicit retry error and discards the temporary archive. This also catches
ordinary accidental external changes, but does not turn unmanaged concurrent
editors into an atomic snapshot boundary. All canonical writers must honor the
managed boundary during export.

Hidden Markdown and adjacent media remain canonical. The explicit reserved names
`.git`, `.obsidian`, `.okilum`, `.tessera-index`, `.env`, `.env.local`,
`.env.production` and `credentials.json` are excluded at every directory level
and listed in the manifest. Okilum connector credentials, configuration, drafts,
indexes and execution journals belong outside the canonical root; these names do
not constitute secret-content scanning. Other regular files are included as local
attachments regardless of file extension or renderer support. Originals are never
rewritten or moved, and missing linked files are identified rather than invented.

Local Markdown links, images and wikilinks are checked against the exported
inventory. Source text and relative paths remain unchanged. Links above the root
or absolute local links remain external; unresolved and ambiguous local targets
are explicit. Remote web/provider URLs are retained in their original Markdown,
not fetched or expanded into the archive. Reference diagnostics follow parsed
Markdown prose, not examples in code blocks; they are not a general HTML/CSS or
attachment dependency crawler.

## Publication and transfer

The core exporter stages outside the selected root, verifies the inventory, syncs
the archive and atomically publishes to a previously nonexistent destination.
An existing destination is never overwritten; even a parent-directory symlink
cannot direct an archive back into its own source root.

The backend download helper retains one temporary archive, exposes its byte length
and SHA-256 revision, and serves repeatable chunks of at most 1 MiB. A new prepared
archive replaces the previous handle. Release, expiry (30 minutes, checked on
access) or backend exit removes staging. These handles are temporary downloads,
not canonical knowledge or provider sessions. A client must assemble into its own
temporary file, verify the full length and SHA-256, then publish at its selected
local destination. A backend filesystem path is not a client download.

## Ordinary desktop flow and API

With a project brain open, choose **Export brain** in the Workspace bar, then a
new local filename in the system save dialog. Okilum prepares saved knowledge on
the backend, transfers bounded chunks and verifies the complete archive before
publishing on this computer. **Show file** reveals the result. The banner reports
file, exclusion and link-dependency counts; inspect `manifest.json` after extraction
for paths and reasons. Existing destination files are never replaced. Save or
otherwise retain any unsaved draft separately; export does not claim to include it.

All download requests use the selected workspace identity guard:

- `export_prepare` returns `export_id`, `bytes`, `revision` and `manifest`.
- `export_chunk` accepts `export_id` and `offset`; returns the same identity/offset,
  `next_offset`, `eof` and `content_base64` (at most 1 MiB before encoding).
- `export_release` accepts `export_id` and removes temporary download staging.

These operations are workspace-wide, independent of current goal selection. Under
the backend owner mutex, preparation drains pending canonical writes before taking
the source writer lock. Neither preparation, transfer nor opening an extracted
Reader invokes a provider. Desktop requests validate chunk identities, positions,
length and final SHA-256 before publishing; failed or interrupted downloads leave
no completed file and can be retried with a new export.

## Lock-test evidence (#203)

The export lock test keeps its immediate `try_lock` negative control between the
first and second inventories: a competing writer must remain excluded. Its before
and after availability controls use blocking acquisition with a five-second test
failure deadline. They do not retry or change the production lock implementation.

On Unix, a concurrent fork can inherit the flock open-file description. `CLOEXEC`
closes it at exec, not at fork; closing the parent's descriptor does not release a
lock still held by that child. A controlled subprocess regression pauses before
exec, observes the original immediate `WouldBlock` result after parent close, then
releases and reaps the child and verifies positive acquisition. The regression
runs in a separate test process so its intentionally retained descriptor cannot
hold locks owned by unrelated parallel tests.

The original #200 parallel-suite failure remains failure evidence. The controlled
case proves this contention mechanism and the corrected positive control; it does
not establish retrospectively that this caused the original failure. The isolated
and serial reruns that passed do not erase that distinction.
