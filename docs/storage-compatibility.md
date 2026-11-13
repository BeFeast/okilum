# ZIP and Inbox SQLite compatibility

Reader uses ZIP 9.5.1; Inbox uses rusqlite 0.42.0 / libsqlite3-sys 0.40.0.
Velopack 1.2.161 keeps ZIP 8.6.0. GPUI remains exactly 0.3.3; toolchain,
archive limits and native budgets are unchanged relative to main.

## Separate updater dependency

This migration changes only Okilum's archive dependency and Inbox SQLite.
Velopack's vendor diff, manifest and complete generated source are byte-identical
to main 718d23d8b799bd50569ba0b8ff40e784afb4ed0c. Windows updater production
code and its existing console controls are also unchanged. Updater hardening is
a separate draft; none of its replacement/admission changes is included here.

Both ZIP majors coexist in the resolved graph. ZIP 8 has exactly one direct
owner, Velopack, which reads only our own update packages after feed-checksum
verification; arbitrary Reader archives go through ZIP 9 and the Reader bounds.
The legacy ZIP path therefore retains these open RustSec advisories (this PR
does not claim to eliminate them):

- [RUSTSEC-2026-0367](https://rustsec.org/advisories/RUSTSEC-2026-0367.html): privileged Unix mode bits.
- [RUSTSEC-2026-0368](https://rustsec.org/advisories/RUSTSEC-2026-0368.html): external symlink targets.
- [RUSTSEC-2026-0369](https://rustsec.org/advisories/RUSTSEC-2026-0369.html): permissions through symlinks.
- [RUSTSEC-2026-0429](https://rustsec.org/advisories/RUSTSEC-2026-0429.html): allocation from a declared symlink size.

The input restriction is a scope statement, not a fix or an audit suppression.
RUSTSEC-2025-0168 is already fixed in ZIP 8.6.0 and is not retained by this path.

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

## Real published Windows packages

`scripts/ci/fixtures/windows-historical-update.json` pins the public beta feed
snapshot containing base Full 0.1.31653 and target Full/Delta 0.1.31678. The driver
`scripts/ci/windows-historical-update.py` downloads all three packages and checks
the feed's size/SHA256 before running the native Windows integration test.

The test seeds an isolated portable installation from the real base package,
uses the unchanged main SDK to prepare either the published Full or the actual Delta with
the published Update.exe, then applies it with the real updater. The Delta
source refuses Full downloads so fallback cannot conceal a broken patch. Both
the reconstructed and installed file trees must match payload bytes extracted
from the published target Full. This includes a 190,399,488-byte executable.
The three historical packages have no ZIP64 entries; this run makes no claim
about historical packages exceeding 4 GiB.

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

## Exact-source controls

`zip-compat-controls.py` requires ZIP 8.6.0 and 9.5.1 in Cargo metadata and rejects
any ZIP 8 owner other than Velopack. Independent main manifest/patch/tree hashes
prove the complete generated updater source is unchanged. It then removes each
Reader compatibility fix, requires its actual regression test to fail, restores
the source and requires the complete archive suite to pass.

The path-triggered `storage-compat` workflow covers vendor setup/patches, Cargo
manifests/locks, updater code, archive code and compatibility controls on every
pushed branch. Native Windows and Intel Mac run the graph/vendor/Reader checks;
Windows additionally applies the real Full and Delta packages. Linux runs full
Inbox lint/tests, WAL recovery and the actual old-stack downgrade probe. Exact-
head agent and native gates remain required. Historical ZIP64 / >4 GiB packages
remain unverified because the pinned published pair contains none.
