# ZIP and Inbox SQLite compatibility

Reader and the checksum-pinned Velopack SDK use ZIP 9.5.1; Inbox uses rusqlite
0.42.0 and bundled SQLite through libsqlite3-sys 0.40.0. GPUI remains exactly
0.3.3. No archive limits, native budgets or toolchain pins change.

## Reader names and link refusal

ZIP 9 prefers UTF-8 whenever raw name bytes are valid UTF-8, even with the ZIP
language-encoding flag clear. Reader explicitly decodes `name_raw()` according
to that flag: flagged UTF-8 retains the prior lossy handling, otherwise CP437.
The displayed name and extraction path use the same decoding. ZIP's path
validation still runs before rebuilding those validated components with the
decoded name. The ambiguous `C3 A9` fixture lists/extracts `├⌐.txt` with the flag
clear; the flagged control extracts `utf8/é.txt` with different bytes.

ZIP 9 interprets external attributes according to the archive's originating OS.
Reader additionally retains the prior conservative refusal of raw upper Unix
symlink bits, including DOS-origin entries. Such entries cannot be previewed and
are skipped during extraction; a neighboring ordinary file is the positive
control. Existing bounds, no-overwrite and rollback rules remain unchanged.

## Updater helper publication

ZIP 9 initializes decompressors on the first read. Velopack's helper search
explicitly skips methods outside `SUPPORTED_COMPRESSION_METHODS`, preserving
ZIP 8's selection behavior when an unsupported `tools/Squirrel.exe` precedes a
supported helper.

Every SDK file extraction stages a `NamedTempFile` in the destination directory,
reads through EOF/CRC validation, verifies the declared uncompressed size, syncs
the completed file, then uses `persist` for atomic replacement on Windows and
Unix. A read, size, write or publication error returns an error and drops the
stage, preserving the original destination. Windows `download_updates` now
propagates bundle loading and helper extraction failures instead of announcing
success. The SDK version stays pinned; source archive, patch and complete
resulting tree remain checksum-verified by `scripts/velopack-setup.py`.

`zip_compat` covers a CRC failure after partial output, mismatched size, direct
unsupported-method failure and the mixed-method selection/replacement control.
Windows also verifies error propagation through `UpdateManager::download_updates`.
`scripts/ci/zip-compat-controls.py` requires the CRC/size controls to fail when
staging is replaced with destination truncation, and the selection control to
fail when unsupported-method filtering is removed; it restores and verifies the
pinned vendor tree and reruns the production controls afterwards.

## Real published Windows packages

`scripts/ci/fixtures/windows-historical-update.json` pins the public beta feed
snapshot containing base Full 0.1.31653 and target Full/Delta 0.1.31678. The driver
`scripts/ci/windows-historical-update.py` downloads all three packages and checks
the feed's size/SHA256 before running the native Windows integration test.

The test seeds an isolated portable installation from the real base package,
uses the new SDK to prepare either the published Full or the actual Delta with
the published Update.exe, then applies it with the real updater. The Delta
source refuses Full downloads so fallback cannot conceal a broken patch. Both
the reconstructed and installed file trees must match payload bytes extracted
from the published target Full. This includes a 190,399,488-byte executable.
The three historical packages have no ZIP64 entries; this run makes no claim
about historical packages exceeding 4 GiB.

The separate console-inheritance test remains a process-creation control with a
stand-in patcher. It now returns a valid ZIP containing a helper, since the SDK
correctly refuses non-ZIP packages. It does not establish real delta application;
the published-package integration above provides that separate evidence.

## SQLite recovery and downgrade

The legacy fixture directory records the old writer source, exact baseline and
checksums. The WAL control includes both main DB and an uncheckpointed WAL,
created by the actual old Store with rusqlite 0.37.0 / SQLite 3.50.2. Main DB alone
must lack the pending capture; opening the pair with the new Store must recover
it, preserve operation replay/text/timestamp, accept a new write and reopen with
clean integrity/foreign-key checks.

The optional `INBOX_DOWNGRADE_DB` output from that test feeds the old-stack
`downgrade.rs` probe. It checks schema-12 read/replay, an old-stack write and a
second old-stack reopen after the new writer touched the DB. This bounded
compatibility check does not cover future schema versions.

The `storage-compat` workflow runs archive/updater controls on native Windows and
Intel Mac, the real published-package application on Windows, and full Inbox
lint/tests on Linux. The normal exact-head agent and native gates remain required.


## Package/helper admission and Windows metadata (re-review follow-up)

A final `.nupkg` is now published only after its Windows Update.exe entry has
been prepared successfully. Sharing violations therefore leave a retryable
`.partial`, not a discoverable final package. An already cached final package
from an older SDK must retry helper preparation before download_updates can
return success. Pending-package discovery also compares the installed helper
against that package's supported helper entry, through complete byte/CRC/size
validation. Okilum repeats that check when creating Ready and immediately before
arming a restart, independently of channel/feed receipts. A later same-size
helper change revokes admission too.

On Windows, existing destinations use ReplaceFileW with metadata merge errors
left fatal; the stage receives the existing file attributes before replacement.
This retains the existing destination's explicit DACL rather than adopting the
tempfile's inherited permissions. Missing targets use persist_noclobber so a
racing new file cannot lose its metadata. Other platforms retain atomic persist.
Native Windows controls hold a target open with only FILE_SHARE_READ (neither
WRITE nor DELETE sharing), require error plus byte-identical original through
repeated attempts, then unlock and require successful preparation. A separate
control carries a protected explicit World execute ACE and HIDDEN /
NOT_CONTENT_INDEXED attributes and compares the security descriptor and complete
attributes before/after replacement.

The vendored SDK's execution-stub extraction no longer retains upstream's
best-effort logging: it returns extraction failures. Its CRC-failure control
requires both an error and unchanged existing stub bytes. The historical
published Update.exe binaries remain unchanged; this patch changes the pinned
Rust SDK source and does not claim to rebuild those external binaries.

Compatibility runs are path-triggered on push to every branch and on pull
requests for the vendor patch/manifest, Cargo manifests/locks and updater code;
explicit storage-compat refs and workflow_dispatch remain usable. Runs for the
same source SHA share a concurrency group to avoid duplicate work when the agent
and native verification refs carry the same commit. The real package application
step remains mandatory on the Windows job. Revert controls cover late package
publication, cached-success bypass, pending admission, lossy metadata replacement
and swallowed stub errors; the native Windows lane additionally disables the
actual Okilum Ready guard and requires its regression to fail before restoring it.
