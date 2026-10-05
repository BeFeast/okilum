# Source history and recovery (#362)

Reader exposes **Note history…** for the current note and **Recover
notes…** for unsaved drafts and history whose original file was moved or deleted. History restores
exact UTF-8 bytes, including BOM and line endings. The user previews a version
before replacing the current note; restoration checks that the file still
matches the version shown at confirmation. Dirty editors and unrelated drafts
block replacement. **Save as recovered note…** uses exclusive creation and never
overwrites a destination. Recovery never requires recreating the original path.

Saved preimages belong in durable application state, outside the derived index.
A save records its original bytes and displaced-file identity before exchanging
the canonical file. After exchange, the displaced inode is moved into history
where possible. An interrupted save or a displaced version that differs from the
expected original remains protected; cleanup must not treat it as ordinary old
history. Legacy unlabelled `.tessera-save-*` files cannot reliably be assigned to
a note and are never silently deleted. Recover notes also exposes these
unassigned files for preview and recovery as a new copy. If vault and history
are on different filesystems, the completed snapshot stays in durable JSON and the displaced inode remains
beside the note until its retention deadline. Cleanup verifies its recorded inode
and exact bytes; an unexpected change protects it from expiration.

Normal completed history keeps at most 20 versions per note for 30 days, within
a 128 MiB completed-history budget. Cleanup runs on save and when history is
opened, not on an idle timer. Inactive acknowledged draft journals expire after
30 days while holding the editor lock; dirty or unreadable journals never expire. Unsaved drafts, incomplete operations, unreadable
records and unexpected displaced bytes are excluded from expiration and size
limits and are surfaced as protected recovery. This is a bounded recent history,
not a replacement for a vault backup.

Link-move manifests already contain exact preimages for every affected note.
The history UI reads these same preimages and offers recovery as a new copy or
routes their restore action to whole-operation recovery; **Recover link moves…** continues to
revert the whole operation, checking each file against its before/after bytes.
Completed operations expire as a unit after 30 days; incomplete operations do
not expire. A successful rename with a changed/unreadable destination or failed
directory sync is still unresolved recovery and does not expire. No per-note pruning may remove part of a retained operation.
