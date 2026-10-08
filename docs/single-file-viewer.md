# Single-file quick viewer (#560)

Opening a Markdown file without an explicitly selected vault opens a quick viewer.
An `.obsidian` ancestor is not authorization to enumerate or index it. OS delivery
of a file inside an already-open full vault focuses that window instead.

The first document is read and prepared independently of folder enumeration.
After its publication, a background task lists the containing directory once.
Expanding a directory lists only that directory. Source bodies of siblings are
not read for inventory; there is no search index, warm snapshot, cache lease,
recursive watcher, backlinks reconciliation, or Inbox computation in quick mode.
Session and source-edit history remain durable application state outside the
notes, and session restore preserves quick mode. Rescan and source saves do not
upgrade the viewer. Full-text search and vault-wide backlinks require the explicit
**Open folder as vault** header action.

Known folder names support ordinary wikilink resolution, including visible
ambiguity. Explicit Markdown paths and adjacent images can resolve without a
recursive inventory; document links remain confined to the opened folder root.
External-file actions retain the existing containment policy. Symlink directories
are not followed by lazy folder enumeration.

The same Reader rendering, source editor, safe-save checks, and navigation history
are used in both modes. The upgrade saves or refuses a dirty document through the
existing source transition before starting vault preparation.

Linux advertises `text/markdown` with `tessera %f`; macOS advertises both Markdown
UTIs. Windows Velopack install/update registers a per-user Open With ProgID, and
uninstall removes it. These registrations do not change the user's default app.

Native cold-start acceptance remains a measurement on the owner's Mac: less than
0.5 seconds to rendered content. Headless tests validate the absence of index
creation with a full-vault positive control; they do not claim native timing.

Vault attachment previews for `.txt` and `.csv` show literal, selectable
UTF-8 text. Reading and preparation run in the background, bounded to the first
64 KiB with an explicit partial-preview notice and an external-open action.
An empty file is identified as empty; unreadable or unsupported text has a clear
fallback with an external-open action. These previews never edit or index the file.
The structured log viewer (#602) opens `.log`, `.jsonl`, `.ndjson` and `.logfmt`
in both vault and quick-file mode, including vault Quick Open and tree selection.
Find/filter within a log is a separate #602 S3 slice. Only macOS offers
Tessera in Open With for logs (rank Alternate); see `docs/log-viewer.md` and
the slice 9 notes in `docs/research/602-hl-log-viewer.md`.
