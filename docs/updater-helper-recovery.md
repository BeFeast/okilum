# Recoverable updater helper publication (Windows)

Independent of the Reader ZIP/Inbox SQLite migration (PR 2853). Velopack stays
at 1.2.161 on its own ZIP 8 major; the fixes live in the checksum-pinned vendor
patch (`scripts/velopack/hidden-delta.diff`, `manifest.json`) and in Okilum's
Windows updater. They address weaknesses that exist with any ZIP version: the
helper was truncated before its replacement had been read, the final package
was published before the helper was replaced, a failed helper extraction was
logged and ignored, and stub extraction errors were dropped.

## Extraction and admission

Every extraction is written to a random temporary file beside the destination,
read to the end through ZIP CRC and declared-size checks and flushed before
publication starts. A read, CRC, size or write failure removes only that
temporary file; the installed helper is never opened for writing.

`download_updates` prepares `Update.exe` from the downloaded `.partial` before
renaming it to the final `.nupkg`, so a failed helper preparation never leaves
an admitted package. A cached final package re-runs preparation.
`get_update_pending_restart` returns a package only when the installed helper
is byte-identical to that package's helper entry; Okilum repeats that check
before it reports Ready.

## Transaction and recovery: identity, not names

Names beside the destination: `Update.exe.vpk-txn` (record), `.vpk-stage`,
`.vpk-backup`, `.vpk-lock`, and `.vpk-quarantine-<id>` for anything set aside.

1. An exclusive (share mode 0) handle on the lock file serializes publication,
   recovery and helper launch across processes. The lock file is deleted after
   release, which cannot succeed while another process holds it.
2. Before anything is renamed, a record with the size and SHA-256 of the new
   helper and of the original is written to a temporary file, flushed, and
   renamed into place atomically.
3. The verified temporary file is renamed to the stage and re-verified through
   a handle. With a destination, `ReplaceFileW` runs with a mandatory backup
   name and flags 0 (ACL/merge errors fail); it carries the destination's DACL,
   owner, attributes and streams. Without one (first install) the stage is
   renamed through its verified handle.
4. Publication succeeds only if the destination is then the new helper by hash.

Recovery opens every candidate with `FILE_FLAG_OPEN_REPARSE_POINT`, accepts
only single-link regular files (no reparse points, symlinks, junctions or hard
links), hashes it through a handle that denies writers and deleters, and
renames or deletes it through that same handle. Nothing changes attributes
through a name.

| Destination | Action |
| --- | --- |
| missing, valid record | promote the stage if it matches the new helper, else the backup if it matches the original |
| missing, no matching copy or no valid record | promote nothing, delete nothing, report "needs repair" |
| present, matches the record (or no record) | keep it; delete leftovers that match the record, set unverifiable ones aside |
| present, matches neither side while a verified copy exists | set it aside and promote the verified copy |

Nothing is deleted while the destination is missing, and nothing unverified is
ever promoted. windows-result reports Win32 errors as HRESULTs (0x8007xxxx);
they are normalized to plain codes so 1175/1176/1177 are matched as documented
by [ReplaceFileW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew).
A native test drives real ReplaceFileW partial failures (held destination,
held stage, occupied backup name) and requires plain Win32 codes.

Locators run recovery before checking that `Update.exe` exists and only log a
refused recovery, so an existing helper is always found. Native API paths use
the extended-length prefix regardless of the host's long-path policy.

## Okilum

- **Needs repair is not Portable.** If the installation cannot be located while
  recovery files remain, Settings says updates are paused because the update
  helper needs repair, shows a path-free diagnostic line, and offers Retry now
  plus the Releases link to reinstall. The state is not cached for the session:
  Okilum retries recovery in the background every minute.
- **Verify and launch are one cross-process transaction.** Restart takes the
  helper lock, re-verifies that the pending package is unchanged and that the
  helper matches it, launches the helper, then releases the lock. Another
  Okilum process cannot replace the helper in between; the running helper
  keeps its image afterwards.

## Controls

`.github/workflows/updater-helper-compat.yml` runs on native Windows:

- `scripts/ci/updater-helper-controls.py`: the vendor suites, then 23 revert
  controls, each reverting one fix, rebuilding and requiring its named test to
  fail; then the real checksum-pinned beta Full and Delta packages are applied
  with fallback forbidden and installed payload hashes compared.
- `scripts/ci/updater-ready-guard.py`: Okilum's Ready check, launch lock,
  launch re-verification and repair classification, each with a compiled
  revert control.

The fault seam (`helper-replacement-tests` vendor feature) is enabled only by
`okilum-http`'s dev-dependency. The protocol targets process interruption and
documented API failures; installed-VM power-cut evidence is collected
separately (see the PR).
