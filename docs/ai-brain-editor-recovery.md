# Native Markdown editor recovery

Approved scope: [issue145](https://git.oklabs.uk/BeFeast/okilum/issues/145).
This contract describes implementation boundaries, not native acceptance or rollout.

Local drafts are separate from canonical Markdown, backend operational state and
exact brain export. Opening a workspace, listing records and restoring a draft
never save or replay a write. **Recover draft**, **Retry this Save** and **Discard**
are explicit UI actions. An uncertain Save must be recovered before discarding its
record. Typing does not save canonical source. A fresh explicit Save may attempt
one conservative independent-edit merge under the
[automatic Save contract](archive/ai-brain-disjoint-save.md); Retry and restore never plan one.

## Storage interface

`brain/editor_recovery.rs` exposes `EditorRecovery`, `Draft` and `RecoveryList` to
the parent brain module. The storage helper performs local file I/O only; the GUI
owns scheduling, source RPC and user actions. Run durability work off the render
path and show protection only after the matching generation is acknowledged.

`EditorRecovery::open(workspace)` uses the desktop configuration directory under
`okilum/editor-recovery/<brain UUID>`. Every record also binds the entire workspace
identity (root, records directory, managed flag and brain ID). `at(base,workspace)`
provides the same behavior with an isolated fixture directory.

| Method | Result and ownership |
| --- | --- |
| `list()` | `RecoveryList { drafts, problems }`; malformed or mismatched records are reported without exposing their content or removing them. No source RPC occurs. |
| `create(base,text)` | New `Draft` with independent UUID and generation1. Each window creates its own draft. |
| `update(expected,text)` | Compares the entire retained draft under a file lock, increments its generation, preserves base/conflict and any immutable pending Save. |
| `retain_save(expected,request)` | Requires the complete exact source-write envelope and current draft bytes. Persists before the caller sends it. A different pending request is refused, including another UUID for identical text. |
| `acknowledge(id,request,receipt)` | Validates the matching pending request and raw `WriteReceipt`: operation, path, previous revision and result SHA256. Returns `None` after retiring a fully saved draft; otherwise returns the newer retained text with its base updated to the bytes actually saved. |
| `retain_auto_resolution(expected,request,view)` | Compares the full original generation and atomically retains one merged child in a v2 local record before RPC. Manual/uncertain results authorize no child send. |
| `acknowledge_auto_resolution(id,request,receipt)` | Validates the exact deterministic child receipt, keeps the original true base and any newer text, updates conflict.current to saved bytes and returns a retained record for the native input check. Generic acknowledgement refuses automatic children. |
| `record_conflict(id,request,view)` | Validates the exact rejected operation and `ConflictView`, clears its uncertain Save and retains the current draft plus base/current/proposed conflict. |
| `refresh_conflict(expected,view)` | Allows only the current side of the same immutable conflict to change after a source_conflict read. Needed when restoring a conflict after another writer edited its note. |
| `discard(expected)` | Removes only the exact retained generation with no uncertain Save. |

`Draft` fields are `automatic_format`, `id`, `generation`, `base: SourceSnapshot` as JSON, `text`,
`pending_save: Option<Value>` and `conflict: Option<Value>`. `pending_save` is the
complete existing `{op:"source_write",request:{...},base:{...}}` envelope, without
UI bookkeeping or transport fields. Callers must strip local reply-routing data
before RPC and match late responses to their retained draft and request.

New ordinary records use `tessera-editor-recovery/v1`. Automatic children switch
the containing local record to `tessera-editor-recovery/v2` until retirement; the
format discriminator participates in generation comparison. The new reader accepts
both without rewriting v1 on list. The exact predecessor rejects v2 as a recovery
identity problem and preserves the bytes; it cannot run its unsafe generic ACK on
an automatic child. Backend source and journal schemas do not change.

## Recovery and durability

Use the original loaded base and expected revision for a restored draft; a current
source read must not silently rebase it. A subsequent Save can return the existing
revision-aware conflict. An explicit resolution request uses exactly the displayed
current snapshot; another concurrent change is another conflict. Missing and
non-UTF8 current sources are preserved rather than recreated or normalized.

Local writes serialize through an OS file lock, compare the full prior generation,
write and sync a temporary file, rename it, then sync the directory. The helper
does not acknowledge a failed durability step. If publication succeeded but its
acknowledgement failed, re-list before updating; a stale expected generation cannot
overwrite the published record. Atomic replacement bounds history to the current
record, and successful explicit retirement removes that record. Retained records
are not discarded to make space.

Listing reveals recoverable bytes, not a durability acknowledgement. Before
showing a recovered record as protected, call `update(expected,expected.text)`;
before explicit retry, call `retain_save(expected,expected.pending_save)` again.
Even when the bytes already match, these calls sync the record and directory and
refuse success if that durability barrier fails. A visible rename from a prior
failed publication is never sufficient to authorize a Save send.

Limits:64 records,8MiB UTF-8 per draft/base,80MiB serialized per record and256MiB per
workspace store. Input bytes and base revisions are checked before publication.
Corruption, budget refusal or lock/persistence errors leave visible recovery
problems; they are not represented as an empty recovered workspace.

Two independent windows use different draft IDs. If both explicitly restore the
same saved draft, generation comparison prevents either from silently overwriting
the other's newer retained work. Late acknowledgement reads the latest generation
under the lock and preserves later text instead of clearing it.

Unsent conversations, goal forms, sync, background replay, editor history and v0
writing remain outside this slice. No backend API, canonical schema or maintenance
fallback change is required.

## Native integration

The managed workspace editor protects changed Markdown on this device through one
background durability job per view. Typing during that job is coalesced into the
latest visible text and persisted next. **Protecting local draft…** is shown while
work is pending; **Protected on this device** requires an acknowledged generation
whose exact text still matches the editor. A listed record is reconfirmed with the
storage helper before it can receive that label.

The Source & preview surface lists recoverable drafts, including drafts for other
notes in the same workspace. Restore keeps its original source revision and exact
bytes. A restored conflict refreshes its current side before resolution. Merely
listing or restoring records never sends a source write.

Explicit Save waits for the current draft to be protected, retains its complete
immutable request, then performs RPC and receipt/conflict persistence in the same
background worker. An uncertain response keeps that operation for **Retry this
Save**; later text cannot replace it. Receipt handling updates only the matching
active draft. Ordinary saved bytes become the editor's base; newer visible text remains dirty
and is protected separately. Automatic child receipts require the matching input
epoch, visible submitted text, workspace, draft and complete local generation.
An unchanged editor adopts the actual merged bytes/revision before exact local
retirement. Otherwise newer text keeps its original base and an inspectable
conflict with saved merged current bytes; ordinary Save is blocked until the
distinct manual resolution action. Navigation and discard cannot
remove an uncertain Save. Recheck reports changed or missing generations instead
of silently adopting another window's edits.

No recovery work uses the backend journal or canonical brain directory. Protection
is local to the desktop and is not a sync guarantee. If the process exits during a
write, the retained request allows explicit recovery on the next launch; no worker
continuation after process exit is assumed.
