# Native Windows source filesystem (#765)

This first slice adds NTFS primitives in `tessera_core::windows_files`. It does
not enable Windows Reader editing or write access in the Reader/MCP protocol.
The subsequent UI slice adapts the existing editor, drafts/history, conflicts,
creation and rename flows to this backend; their Unix behavior stays intact.

A `Directory` pins every ancestor without WRITE/DELETE sharing and refuses UNC,
mapped network drives and non-NTFS volumes. Symlinks, junctions, unknown reparse
tags, hard-linked sources and readonly notes fail closed. Resident CLOUD-family
files are accepted because their tags do not redirect names; offline/recall
sources require an external download. Ancestor handles also prevent in-place
reparse retagging. Real OneDrive save behavior still needs owner QA.

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

Linux core clippy and an isolated Windows cross-clippy typecheck supplement
these tests; neither substitutes for their execution on Windows. UI remains
disabled until the native backend is accepted and the common editor integration
is reviewed.
