# Native Windows source filesystem (#765)

The backend slice (#776) added NTFS primitives in `okilum_core::windows_files`.
The desktop integration (#765) adapts the shared editor, drafts/history,
conflicts, creation and rename flows to those primitives. The Reader/MCP
protocol remains read-only.

A `Directory` pins every ancestor without DELETE sharing and refuses UNC,
mapped network drives and non-NTFS volumes. Symlinks, junctions, unknown reparse
tags, hard-linked sources and readonly notes fail closed. Resident CLOUD-family
files are accepted because their tags do not redirect names; offline/recall
sources require an external download. Real OneDrive save behavior still needs
owner QA.
Ancestor handles request `FILE_LIST_DIRECTORY` as well as attributes: a
metadata-only handle does not participate in Windows sharing checks. Native
controls require sharing violation (32) for renaming both the folder and its
parent; rename must work after the guard is released. READ and WRITE sharing
allow the writable parent opens used by our own publication operations. A native
positive control opens both ancestors for write before creating the note.

`prepare_replace` reads the checked file while excluding in-place writers,
compares exact bytes, and creates a complete same-folder prepared file. Its DACL
comes from the checked source handle at creation, before any proposed bytes are
written. An encrypted source requires an encrypted prepared file, verified before
writing any proposed bytes. The file uses write-through and is flushed. The returned plan exposes
the note, prepared and preimage paths: the caller must persist its draft/history
record before calling `commit`. Dropping a plan never deletes recovery bytes.

Commit uses `ReplaceFileW` with a retained preimage and no ignore-ACL/merge flags.
The displaced bytes and file identity must equal the checked source. A racing
replacement causes a reversal which also retains the displaced proposed/late
version. Errors preserve all surviving prepared/preimage/recovery files. Other
writers can briefly observe the proposed file during that reversal, just as on
Unix. The final bytes are checked under a writer-excluding handle and flushed;
a failed durability check never reports Saved. `REPLACEFILE_WRITE_THROUGH` is
unsupported by Windows and is deliberately not used. Cleanup/retention belongs
to the durable recovery owner, not a path-based destructor.

Create and rename use `MoveFileExW` with WRITE_THROUGH, without REPLACE_EXISTING
or COPY_ALLOWED. Destination collisions never overwrite. Rename checks source
bytes and file identity again after the move; a detected race is reported for
inspection, never rolled back over a concurrent new source. Names reject ADS,
Win32 aliases, reserved devices and multiple path components.

Sharing/lock violations receive bounded retries totaling 300 ms before failure.
These APIs perform blocking I/O and belong on a worker, never the UI thread.
They provide filesystem primitives, not an independent editor or journal format.

## Verification

`windows_files::tests::windows_save_*` must run in the Windows-native workflow
on the exact backend SHA, using package `okilum-core` and test filter
`windows_save_`. Acceptance requires all eleven named tests to execute; zero
matched tests or skipped symlink/junction controls are not passing evidence.
They cover lossless BOM/CRLF/ru/he/en, displaced-source races, sharing failures
and retry, a persisted draft plus preimage surviving acknowledgement loss,
no-overwrite create/rename, parent identity/hardlinks/readonly, hidden attributes,
a custom protected DACL, reparse points, cloud-tag classification, and a
last-moment symlink target race. Symlink controls require the runner's developer
or administrator privileges. Cloud-tag classification does not simulate a real
FileProvider: OneDrive/antivirus interoperability remains native owner QA.
Permission comparisons check owner/group SIDs, the ordered DACL ACEs (including
masks and flags), null versus empty DACL, and `SE_DACL_PROTECTED`, on the prepared
file before publication as well as the source and retained preimage afterward.
`SE_DACL_AUTO_INHERITED` is Windows bookkeeping for inheritance processing and
is not compared as a user permission; ACEs and inheritance protection must match.

Linux core clippy and Windows MSVC cross-clippy for the full package, including
all test targets, supplement these tests; neither substitutes for their execution
on Windows. The Unix rename integration test and inode assertion remain gated
on Unix; portable search-update assertions also compile on Windows.

## Desktop integration

Windows uses the shared Edit source / Live Preview input, Ctrl+S, Ctrl+F,
New note/folder and reviewed link-rewriting rename flows. Editing is restricted
to the local NTFS backend; network and unsupported filesystems fail closed.
A source editor records the original ancestry identities and pins and checks
all ancestors for each operation. Pins are released between operations so an
explicit folder move is possible; the next read/save refuses a replaced parent.
Dirty drafts are persisted before any source validation or replacement failure.

Exclusive sharing holds the editor journal lock until the last queued writer
finishes. Generation checks serialize native same-volume draft publication,
so a delayed write cannot supersede a newer draft or acknowledged save. Drafts
and link-move/history records live in application state, outside the index.
Before source mutation, history records both prepared and preimage paths.
Prepared recovery bytes remain discoverable after preparation/publication errors;
an unacknowledged save keeps protected history and can acknowledge its already
published exact draft on reopen.
History preview limits each prepared recovery read to 128 MiB, checking the
opened file size before allocating and bounding the read itself. Windows displaced
recovery uses the same bound and checked native open, refusing symlinks, reparse
redirection and hard links rather than reading their targets. Oversized or
non-UTF-8 recovery bytes remain on disk with a listing warning; this preview
limit never deletes recovery or limits the source save.

Native preimages retain the source DACL until a successful save has persisted a
complete acknowledged snapshot in application state. The history owner then
removes only the checked original preimage through a DELETE handle, verifying
identity and exact bytes and excluding writers. The snapshot remains subject to
the existing 20-version/30-day/128-MiB retention. A crash during cleanup or a
sharing error leaves an identified cleanup record; startup retries only these
acknowledged records for the opened vault. Interrupted saves, changed/replaced
preimages, reparse points, and unassigned synced leftovers remain protected.
All `.tessera-save-*` names are excluded from inventory, search and the tree on
every platform, even with Show hidden enabled. This includes old six-character
tempfile names, native prepared/raced files and synced preimages. Visibility is
separate from cleanup ownership: only acknowledged identity-bound preimages are
automatically removed. Unassigned
legacy files can still be inspected through recovery; they require a separate
dry-run inventory and owner approval before one-time deletion. Folder
snapshots exclude generated `.tessera-save-*` recovery entries from canonical
inventory; ordinary sources/assets and directory identities remain revision-bound.
The link-move journal retains complete before/after bytes even when vault-side
history names move with a folder. No automatic rollback overwrites a concurrent
writer; post-publication verification failures are reported as completed moves
requiring inspection.

`python scripts/okilum-save-dry-run.py /absolute/vault/root` prints a read-only
JSON inventory grouped into canonical Windows UUID `.previous` names, legacy
six-character tempfile names and other `.tessera-save-*` entries. It reads only
directory entries and metadata, never file contents, and does not follow symlinks
or Windows reparse directories. Scan errors are reported explicitly. Send the
exact report to the owner before considering any legacy cleanup; a matching name
alone is not proof that the bytes are safe to delete.

Old Unix saves used `tempfile::Builder` with this prefix and intentionally kept
displaced inodes without ownership records. Commit b2d1fcff replaced that path
with directory-bound UUID names and durable history archival. Current Unix saves
archive to application state; a cross-device archive retains the identity-bound
vault preimage under history retention. Historical unassigned names are not
automatically adopted or deleted.

Creation Undo checks the original identity and exact initial source (or empty
folder), then marks that checked native DELETE handle for deletion. It refuses
changed/replaced notes and populated folders. Windows system Trash is separate
from this limited Undo operation. Startup markers distinguish live launches from
unclean exits through sharing exclusion and are cleared only after drafts are
protected at normal application shutdown.

`tests/desktop_editor.rs` runs portable `windows_editor_*` contracts on both Unix
and native Windows. Additional Windows tests exercise queued-draft protection
after a parent swap, sharing errors, checked creation Undo, acknowledgement loss
and last-moment replacement. Native acceptance must cover these and the eleven
`windows_save_*` backend controls at the final SHA; cross-clippy is supplementary.
Velopack beta QA checks the actual Windows input, Source/Live Preview, navigation,
Ctrl+S/Ctrl+F, creation/rename and OneDrive/antivirus behavior.

## Windows vault path boundaries (#803, #805)

Filesystem paths keep native Windows spelling for I/O. A path becoming a vault
identity passes through the scanner's `vault::note_path` conversion to `/`.
Creation returns this identity before template validation and immediate tree
publication. Rename normalizes the selected destination before preview; the
shared preview entrypoint applies the same boundary to native source/destination
inputs. Unix literal backslashes are preserved by the platform-aware helper.

Approved moved/referring sources retain exact byte validation. If an unrelated
existing note changes while a preview is open, validation reads it again and
proves its current links need no move rewrite before proceeding. A new incoming
link, changed approved source, changed inventory, or unverifiable target still
requires a new preview. Unrelated external source bytes are never overwritten.
Native contracts use filesystem joins and `set_file_name` in nested folders,
round-trip create/template/tree identities, rewrite both wikilinks and the sibling
Markdown link, and check unrelated-edit/new-referrer controls for indexed and
full-scan previews.
