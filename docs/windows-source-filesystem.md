# Native Windows source filesystem (#765)

The backend slice (#776) added NTFS primitives in `tessera_core::windows_files`.
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
on the exact backend SHA, using package `tessera-core` and test filter
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

Preimages remain on their source NTFS volume with the source DACL. Folder
snapshots exclude generated `.tessera-save-*` recovery entries from canonical
inventory; ordinary sources/assets and directory identities remain revision-bound.
The link-move journal retains complete before/after bytes even when vault-side
history names move with a folder. No automatic rollback overwrites a concurrent
writer; post-publication verification failures are reported as completed moves
requiring inspection.

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
