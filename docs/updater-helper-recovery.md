# Draft: recoverable updater helper publication

This lower-priority hardening is independent of the Reader ZIP/Inbox SQLite
migration in PR 2853. It retains main's ZIP 8.6.0 and Velopack 1.2.161. Existing
truncate-before-read and helper-after-package-publication weaknesses exist with
ZIP 8 too; no dependency bump is needed to address them.

## Publication and admission

Extraction stages beside the destination, reads the complete entry through CRC
and expected-size checks, syncs it, then hands it to the publication transaction.
Existing Windows files use ReplaceFileW with a mandatory backup name and fatal
metadata merge failures. The stage receives destination attributes first;
ReplaceFileW preserves the DACL and other original metadata. New files use a
no-clobber move. Helper preparation precedes final nupkg publication; cached
packages retry preparation, pending discovery rejects stale helpers, and Okilum
checks the actual helper/package entry bytes when creating Ready and before
restart. Stub extraction errors also propagate.

## On-disk transaction and recovery

A per-destination Windows exclusive file handle serializes publication and
recovery across processes. Its .vpk-lock file is persistent; ownership is the
handle, not the file's existence. Fully validated, synced bytes are transferred
to Update.exe.vpk-stage. A synced .vpk-ready receipt follows. ReplaceFileW always
receives Update.exe.vpk-backup. Random temporary extraction files have not yet
entered a transaction and are never startup recovery sources.

| Observed state | Recovery |
| --- | --- |
| Original + stage | Keep original; discard only obsolete stage/receipt |
| Missing original + backup + stage | Rename original backup back; retain every file if restoration fails |
| Replacement + backup, stage consumed | Swap completed; keep replacement, remove obsolete backup/receipt |
| Missing original + verified stage only | Recover the verified stage; an absent/incomplete receipt cannot authorize it |
| Missing original + incomplete stage | Return error and retain stage; never delete the only available copy |
| Destination + backup + stage | Return an ambiguity error and retain all three |

The SDK explicitly handles Microsoft's documented errors:
1175 ERROR_UNABLE_TO_REMOVE_REPLACED (both names retained),
1176 ERROR_UNABLE_TO_MOVE_REPLACEMENT (mandatory backup prevents deletion of the
original), and 1177 ERROR_UNABLE_TO_MOVE_REPLACEMENT_2 (original renamed to the
backup). All errors run recovery and still return error to package admission.
Cleanup holds the destination open without DELETE/WRITE sharing, so a presence
check cannot race deletion of the admitted helper. No failure removes
stage/backup while the destination is missing. A refused
rollback leaves all transaction files for the next process.

Recovery runs before locator helper-existence checks, including automatic
current-directory discovery and MSI fallback paths. Native API paths use the
extended-length prefix for local and UNC paths, independently of host MAX_PATH
policy. This protocol targets process interruption and documented API failure;
it does not claim full-device power-loss durability.

[Microsoft ReplaceFileW contract](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew).

## Controls and review status

A dedicated feature exposes the fault seam only in native test builds; normal
application builds contain no environment-controlled injection. Tests execute
the production publication/recovery state machine with each documented partial
failure, blocked rollback, pre-swap/post-backup/post-swap interruption, explicit
ACE and non-default attributes, locked target without DELETE/WRITE sharing,
same-size helper corruption, >400-character paths and a real child abort followed
by fresh locator recovery. A successful real ReplaceFileW is interrupted before
cleanup and must leave the actual original backup. This control fails if the
API backup argument is reverted to NULL. Another mutation deliberately deletes
the stage before a refused rollback and must fail its retention assertion.

The dedicated native Windows workflow restores the vendor source after each
mutation and reruns positives, then applies real published Full and Delta
packages with fallback forbidden and compares installed payload hashes. The
native shell control separately disables the actual Ready guard, requires its
assertion failure and restores it.

This remains a draft for separate adversarial review. It has no deadline and
must not merge before that review and explicit notification to Oleg. ZIP-8
RustSec advisories remain open on the trusted update-package path; this draft
neither upgrades ZIP nor suppresses advisories.
