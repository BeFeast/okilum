# Recoverable updater helper publication (Windows)

Independent of the Reader ZIP/Inbox SQLite migration (PR 2853). Velopack stays
at 1.2.161 on its own ZIP 8 major; the fixes live in the checksum-pinned vendor
patch (`scripts/velopack/hidden-delta.diff`, `manifest.json`). They address
weaknesses that exist with any ZIP version: the helper was truncated before its
replacement had been read, the final package was published before the helper
was replaced, a failed helper extraction was logged and ignored, and stub
extraction errors were dropped.

## Extraction and admission

Every extraction is written to a random temporary file beside the destination.
It is read to the end through ZIP CRC and declared-size checks and flushed
before publication starts. Any read, CRC, size or write failure removes only
that temporary file; the installed helper is never opened for writing.

`download_updates` prepares `Update.exe` from the downloaded `.partial` before
renaming it to the final `.nupkg`, so a failed helper preparation never leaves
an admitted package. A cached final package (for example one left by the old
SDK) re-runs preparation. `get_update_pending_restart` returns a package only
when the installed helper is byte-identical to that package's helper entry, and
Okilum repeats that check before it reports Ready and again before restart.

## On-disk transaction

Names beside the destination: `Update.exe.vpk-stage`, `Update.exe.vpk-backup`
and the persistent lock file `Update.exe.vpk-lock` (an exclusive handle on it
serializes publication and recovery across processes).

Invariant: a file only ever acquires the destination, stage or backup name
through an atomic rename of a complete file. The stage is the verified, flushed
extraction (no-clobber `MoveFileExW`, write-through); the backup is the original
renamed by `ReplaceFileW`, which always receives the backup name (never NULL)
with flags 0, so ACL and merge errors fail. ReplaceFileW gives the new helper
the destination's DACL, owner, attributes and streams; the native metadata
control showed that copying attributes to the stage beforehand is unnecessary.
With no destination (first install) the stage is renamed into place.

Recovery runs at the start of every publication, after any failed publication,
and in the locator before it checks that `Update.exe` exists:

| Destination | Backup | Stage | Action |
| --- | --- | --- | --- |
| missing | present | any | rename the backup (the original) back |
| missing | absent | present | rename the stage into place |
| missing | absent | absent | nothing to recover |
| present | any | any | keep the destination; delete the other files while holding it open |

A refused rename leaves every file in place and returns an error. Nothing is
deleted while the destination is missing. Locators log a recovery failure and
continue discovery, so an existing `Update.exe` is always found, and a later
launch retries recovery. Every state therefore converges to exactly one
complete helper. A helper that does not match the pending package is never
admitted; the next download prepares it again.

[ReplaceFileW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew)
partial failures:

| Error | Documented state | Recovery |
| --- | --- | --- |
| 1175 `ERROR_UNABLE_TO_REMOVE_REPLACED` | both files keep their names | keep original, delete stage |
| 1176 `ERROR_UNABLE_TO_MOVE_REPLACEMENT` | with a backup name, both keep their names | keep original, delete stage |
| 1177 `ERROR_UNABLE_TO_MOVE_REPLACEMENT_2` | original renamed to the backup | rename the backup back, delete stage |

All of them still fail package admission. Native API paths use the
extended-length prefix (`\\?\`, `\\?\UNC\`) regardless of the host's long-path
policy. The protocol targets process interruption and documented API failures;
it does not claim power-loss durability beyond flushing before each rename.

## Controls

`helper-replacement-tests` is a vendor feature enabled only by `okilum-http`'s
dev-dependency; it compiles a fault seam that drives the production state
machine with injected ReplaceFileW errors 1175/1176/1177, a refused rollback,
and interruptions before the swap, between ReplaceFileW's renames and after a
real swap. A child process aborts mid-transaction and a fresh locator recovers.
Other tests cover a locked target opened without `FILE_SHARE_DELETE`/`WRITE`,
failure followed by retry, a protected explicit DACL with non-default
attributes, paths over 400 characters, first installs, leftovers next to an
existing helper, refused cleanup and refused restore during discovery, stale
and same-size corrupted helpers, and stub extraction failure.

`.github/workflows/updater-helper-compat.yml` runs them on native Windows.
`scripts/ci/updater-helper-controls.py` then reverts each fix separately,
rebuilds and requires the named test to fail. `scripts/ci/updater-ready-guard.py`
does the same for Okilum's own Ready check. The workflow then applies real,
checksum-pinned Full and Delta packages from the beta feed with fallback
forbidden and compares installed payload hashes.
