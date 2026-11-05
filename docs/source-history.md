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
history. Legacy unlabelled `.okilum-save-*` files cannot reliably be assigned to
a note and are never silently deleted. Recover notes also exposes these
unassigned files for preview and recovery as a new copy. If vault and history
are on different filesystems, the completed snapshot stays in durable JSON and the displaced inode remains
beside the note until its retention deadline. Cleanup verifies its recorded inode
and exact bytes; an unexpected change protects it from expiration.

Windows saves also record the complete proposed UTF-8 bytes and the checked
staging identity in durable app-state history before publication. After a refused
save, the owned `.prepared` inode is removed only through an identity- and
byte-checked native DELETE handle; its pending recovery snapshot remains protected
outside the vault. Startup retries this cleanup after interruption or a sharing
lock. Changed, replaced, hard-linked or redirecting staging files remain in place
with their snapshot and any readable unexpected version available for recovery.
Older records without a captured identity/snapshot and unassigned synced copies
are not cleanup authority. An assigned prepared file appears once in recovery,
not again as an unassigned displaced source. Successful save acknowledgement
discards the redundant proposed snapshot while retaining normal preimage history.

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

## Previous versions from Sync (#2045)

On Linux and macOS, **Note history** also reads the vault's default `.stversions`
store. **Recover notes…** includes deleted Sync notes, previews their exact bytes
and offers **Restore to original path**. Restore refuses an appearing destination,
unsaved editors and unrelated draft journals. The original parent folder must
still exist; **Save as recovered note…** offers another destination. Overwritten
notes use the same revision-checked, recoverable byte transaction as Encoding
Repair; the displaced current contents remain in durable application history.
The reviewed byte revision is captured when a version is selected. An edit after
that preview blocks Restore. Archives remain in `.stversions` after restoration.

Discovery is read-only and descriptor-relative, rejects symlinks, and bounds each
request to 10,000 entries, 100 versions and 16 MiB. Limits and unreadable entries
produce visible incomplete-result warnings. Unsupported/custom Syncthing version
stores are not imported or moved. This is a local safety net for remote deletions
and overwrites, not an archive of every local edit or a vault backup.
Recover notes excludes originals that still exist before charging its version
and byte budgets, so an active note's many archives cannot hide deleted notes.
The entry traversal limit still applies and is reported when reached.

Folders created by Okilum use staggered versioning with a default maximum age of
30 days on both the sharing and receiving computer. Existing owned folders
reconcile this default on Sync refresh. Settings → Sync offers 7, 30, 90 and 365
days per folder on this computer; the choice persists outside the index and does
not change another computer's choice. Staggering keeps fewer older revisions
within that age window; it does not promise every revision for 30 days.
Reducing retention requires an explicit confirmation showing the old and new
windows and how many days are removed. Syncthing can permanently delete versions
older than the new window on cleanup; increasing it later cannot recover them.
Cancel, Escape, closing the dialog and Enter do not authorize the reduction.

Only durable source/receiver ownership records authorize policy changes. External
folders (including folders in the same managed daemon) and legacy receivers with
no ownership witness remain unchanged. Configuration writes serialize with direct
Sync operations, authenticate and verify the selected daemon/path identity, persist
an exact before/after intent, PATCH only the folder's versioning fields, and read
back the result. A lost acknowledgement accepts only those exact snapshots;
unrelated changes stop reconciliation. Custom version store locations require
explicit review and are never silently reset.
An unavailable or changed folder records a warning beside its path in Settings
instead of stopping refresh for every vault. Healthy folders continue receiving
and reconciling. Failed bindings retain their ownership checks and pending
intents; refresh does not recreate removed folders or adopt another runtime.

Acceptance runs in `scripts/ci/sync-source-sharing.sh` through the reaped fixture
path, with checksum-pinned Syncthing 2.1.6 on both sides. The #2045 scenario covers
both managed migrations and idempotence, deleted-note discovery and Restore,
remote overwrite history, exact hashes, stale local edit refusal, external folder
preservation and retention persistence. A 31-day time-shifted archive survives the
90-day positive control, then disappears at 30 days while a 29-day sibling remains.
The fixture driver requires the named test and a non-zero exact passed count.
