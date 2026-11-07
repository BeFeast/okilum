# Local writer admission (#2507)

`okilum-write-authority` is the dependency-free-from-product shared boundary for
core, Reader, Project/Board and Sync consumers. It coordinates **cooperating local
Okilum writers only**. It is not a Syncthing quiescence receipt, an external-editor
lock, or an `ExclusiveSession`. #2508 supplies managed transport quiescence;
#2504 must compose both proofs before granting exclusive apply ownership.
Non-empty direct attachment remains refused.

## Location and identity

The account state resolver uses `$XDG_STATE_HOME/okilum/local-writers-v1` (or
`~/.local/state/okilum/local-writers-v1`) on Linux, the application support
directory on macOS and LocalAppData on Windows. Reader's QA `OKILUM_STATE_DIR`,
per-window drafts, indexes and Sync profiles cannot choose another authority.
Account state must be outside the canonical root. Unix authority directories
require account ownership and mode 0700; final symlinks are refused. Windows uses
the account LocalAppData ACL boundary and native file identities.

A root's canonical native path selects its immutable binding. That binding records
its filesystem identity (including creation time when available), a random epoch, and the identities of the authority
directory and all three lock files. Directory replacement, a substituted root,
a missing/corrupt state, or a changed epoch refuses writes rather than creating
fresh authority. Durable state is operational data: deleting an index does not
clear it, and deleting authority is not a supported recovery/reset procedure.
Complete destruction/replacement of all private account state by its owner is
outside the cooperative fencing guarantee.

On macOS (#2692), schema 2 identifies a filesystem object by its full native
volume UUID, inode and creation time. `fgetattrlist(ATTR_VOL_UUID)` and metadata
come from the same no-follow file handle. Raw `st_dev` is a diagnostic hint:
APFS can renumber the Data volume after a reboot or OS update. Other platforms
retain schema 1 and their existing identity rules.

Existing Mac schema-1 bindings migrate automatically under registry, admission,
exclusive writer and owner locks. The canonical root and every recorded authority
directory/lock inode and nonmissing creation time must match. The migration only
adds the observed volume UUIDs and updates private binding metadata; it leaves
canonical files, epoch and the complete state/hold record unchanged. A busy old
writer must finish first. Old schema-1 handles remain stale after publication;
an unfinished hold still refuses admission. Replaced roots/lock files, unknown
birth identities and corrupt state fail closed. This is format migration, never
an automatic Re-attach. Subsequent UUID or inode/creation changes are replacements
even if the raw device hint stayed equal.

A vault folder re-created at the same canonical path (for example re-received by
Sync) stays refused until the user re-attaches it: Reader's failed preparation
offers **Re-attach folder** in place of Retry (#2589). That click calls
`Authority::rebind_replaced_root`, which only rewrites the recorded root identity,
under the registry and admission locks, and only for a plain folder at its own
canonical path owned by this account (no symlink, junction or symlinked parent),
with no running hold owner, no live writer lease and no other binding owning that
physical folder. The epoch, revision and any unfinished hold are kept: a durable
hold still closes admission and goes through normal recovery, and authorities or
leases minted for the old folder stay refused. Fresh lookups never re-attach.

Vault discovery registers the actual root before nested editors. A standalone
file uses its containing folder. Nested publishers reuse an existing enclosing
root. Discovery promotes idle smaller scopes into the actual vault root under
registry and child admission/writer locks. Existing editor handles follow the
published enclosing scope on their next admission, including nested rollback.
A live lease or unfinished child hold prevents promotion until it finishes or is
explicitly released/recovered; discovery surfaces that refusal. Publishing the
parent binding is the atomic promotion point; old bindings remain durable.
Interrupted initialization without a published binding can resume under the
registry lock, but an unfinished durable hold is never reset.
Root registration serializes a short metadata transaction on the opening worker.
Ordinary admission serializes bounded metadata work on admission.lock, so normal
concurrent writers do not fail due to transient contention. It never waits for
drain, transport, a peer or another canonical writer's publication.

## Lease and hold protocol

1. `Authority::operation` briefly serializes admission metadata, validates the
   authority once, reads durable Open state and takes a shared writers lock.
   It enters the synchronous scope without repeating validation. `admit` plus
   checked `enter` remain available for delayed work; resuming publication on
   another worker revalidates the retained lease.
2. A `WriterLease` spans the complete operation and any rollback. Synchronous
   nested publishers inherit it through a thread-confined `OperationGuard`.
   A delayed job retains a cloned lease and calls `enter` on its own worker.
   `PreparedCopy`, image import results and rich-paste results retain admission
   through later publication/acknowledgement/cleanup. Editor sessions themselves
   do not hold writer leases while the user types or reviews a preview.
3. `begin_drain(operation_id)` holds the owner lock and durably records Draining
   with an incremented revision **before** inspecting any existing writers.
   New writers refuse immediately and retain their proposals. Already-admitted
   operations can finish and call nested helpers even after admission closes.
4. On a background worker, `Hold::try_hold(expected_token)` performs a single
   nonblocking drain attempt. False means try later; it never grants ownership
   while a lease remains. Success durably records Held and advances the token.
   This means only local Okilum writers have drained.
5. `release(expected_token)` is explicit, checks epoch/revision/operation, verifies
   no admitted writer remains, durably records Open and advances the revision.
   The consuming caller must first reconcile its own operation. No timeout,
   cancellation, window close or `Drop` releases durable state.

Every transition rejects stale/replayed tokens. A restarted caller observes
Draining/Held as closed; `snapshot` records RecoveryPending after detecting the
owner lock is gone. `recover(exact_observed_snapshot)` requires that old owner to
be absent and creates a new revision. Recovery still has to drain and explicitly
release; opening a Reader or Sync process never does so automatically. An existing
live holder prevents another caller from adopting recovery. Errors retain the
closed state and must be surfaced to the caller.

## Production publisher audit

| Entrance | Admission lifetime / retained state |
| --- | --- |
| `FileEditor::save_before_exchange`, history restore | Dirty draft is durable before admission with one pre-publication journal flush; clean navigation needs no admission. One scope covers save/history/rollback. Protected drafts permit closing/quit after a save access refusal. |
| Task/reminder apply/Undo, virtual-log criteria | Admit before generating a temporary dirty draft, so certain hold refusal leaves retry/reopen usable. Canonical conflicts and unconfirmed errors still preserve recovery. |
| Windows creation Undo, prepared/preimage startup cleanup | Native `windows_files::undo_created` admits before unlink; ordinary save cleanup inherits the current lease. Startup warnings retain files for retry after release. |
| Rich paste while held | Data images and bounded local payloads remain inline in the text draft; unreadable/oversized local images retain file references. Both immediate and background results explain the paused write. |
| Explicit raw byte repair/history replacement | Whole raw transaction, including rollback, inherits one admission; existing exact history/recovery remains authoritative. |
| `SourceStore::write_base_with_hook` | Durable intent and preimage before admission; source replacement and receipt under one lease. |
| `note_files` create/attachment create | Whole create-only publication and cleanup. |
| `PreparedCopy` | Lease retained from preparation through delayed publish or cancellation cleanup. |
| File/directory `MovePlan::commit` (Unix/Windows) | Preview pins authority; commit admits once and retains through verification/durability. |
| Link rewrite apply/revert | Outer scope spans every rewritten file, move and rollback. |
| Capture and capture attachments | Checkpoints remain possible while held; aggregate publication shares admission across assets and note. |
| Unix Trash / Windows Recycle Bin and restore | Whole system/fallback/Undo transaction. |
| Paste attachment/file writers, image conversion and remote image results | Admission before directory creation; delayed cleanup retains original lease. |
| Missing explicit CLI note | Admission before create-new. |
| Project create/link, manifest replace, Board/local Board create/edit/Undo, Manager registry start | Outer operation inherited by nested writers; UI drafts retained on error. |
| Vault display-name metadata, `.stignore` update | Explicit local write respects the same root authority. |
| Exact archive/download destinations | Destination publication respects an enclosing registered vault. |
| Interrupted-save missing-source restoration | Admission before create-only recovery publication; refused recovery keeps original artifacts. |

Reader/MCP remain read-only. Drafts, source/capture intents, chat/outbox records,
source-history archives and recovery journals outside the vault remain writable
under a hold. Derived cache/index writes and external code-block scratch files
are not canonical publishers. Transport receive/promotion, service lifecycle and
autonomous restart fencing belong to #2508; this slice does not manufacture a
transport-exclusive claim from local locks.

## Verification

`scripts/ci/local-writer-admission.sh` explicitly selects authority, real
FileEditor process, core production publisher, shell Trash/attachment and Board
regression groups. Each command must print a non-zero passing count; an empty
filter fails CI. Child ready markers are written only after reaching the real
admission/publication boundary. Parent fixtures kill and reap children on every
exit. After-release controls prove the same writers can publish again. The normal
full core and shell suites remain required alongside these groups.
