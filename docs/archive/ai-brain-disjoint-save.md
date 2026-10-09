# Automatic independent source edits (#206)

> **Historical document.** Written before Tessera was renamed Okilum (2026-10-09). Names, paths and links are kept as they were then.

Status: reviewed contract; implementation and acceptance evidence tracked in #206.
The existing managed `source_write` API and source journal remain unchanged.

An explicit fresh Save may resolve one known stale-revision response automatically
when the exact loaded base, current note and submitted draft admit a conservative
independent line merge. The editor displays the saved merged bytes after a matching
write receipt. Overlap or uncertainty retains the existing inspectable conflict
and local draft. Opening, restoring, previewing and retrying never initiate a new
automatic resolution.

## Selected design and ownership

Reuse the existing pure `brain/merge_preview.rs` helper and its conservative policy.
Use existing `SourceSnapshot` / `ConflictView` identities, ordinary CAS source
writes and `EditorRecovery` durability. A new native coordinator may make a maximum
of two existing source RPCs for one fresh Save: its original write, then one fully
retained resolution write after a known stale conflict. No new backend operation, authoritative note store or SourceWrite field is
required. A narrowly versioned local recovery record fences automatic attempts
from predecessor acknowledgement behavior; ordinary existing records stay readable.

| Owner | Files / responsibilities |
| --- | --- |
| Contract/recovery owner | This contract; `brain/editor_recovery.rs` and a subordinate `editor_recovery/auto_resolution.rs` module for pure plan validation, deterministic resolution identity and atomic local retention; focused recovery/actual-backend evidence. |
| Native owner | `brain/editor_ui.rs`, optional `editor_ui/auto_save.rs`, editor view integration, Save intent classification and result display; relevant `brain.rs` / `main.rs` wiring only as needed; linux-test-host acceptance. |
| Existing merge policy | `brain/merge_preview.rs` stays the single algorithm; change only if a demonstrated acceptance gap requires separately reviewed correction. |
| Root | Independent contract/source review, integration, CI, publication and normal merge. |

The owners agree the typed seam below before either edits the other's files.
The native code lane may run alongside #48 when ownership does not overlap; actual
linux-test-host acceptance waits for #48 rig cleanup. The shared build lock remains serial.

Implementation must not add artificial backend changes merely to create a backend
component. An API-level opt-in auto-write was considered but rejected for this
slice: it would add replay/receipt semantics while the desktop already owns the
exact Save intent and durable outbox needed here.

## Pure merge policy and validation

Merge units are exact UTF-8 lines including their original terminators. Existing
limits apply to each input and output: 256 KiB and 4,096 lines, with a 100 ms
background computation deadline. Exhaustion is a manual conflict, not permission
to accept the diff library's coarse deadline fallback. No work runs on the render
thread.

The policy preserves LF, CRLF, BOM, absent final newline, frontmatter, wikilinks,
fenced code and unchanged bytes verbatim. It does not parse or normalize Markdown.
Two changes to one line remain an overlap even if different words changed.

- Exact current=draft, current=base or draft=base identities are unambiguous after
  missing/text/budget validation. Identical changes at the same range occur once.
- Distinct replacements/deletions use nonoverlapping half-open base line ranges.
- Inserts at distinct unambiguous anchors can combine. Different inserts at the
  same anchor conflict. An insert touching either end of a replacement/deletion
  conflicts; it is not assigned an arbitrary before/after order.
- Repeated removed/replaced lines, repeated adjacent insertion anchors and
  moved/copied existing lines remain manual when alignment is ambiguous.
- Missing/deleted base or current, non-UTF-8 bytes and invalid identities remain
  inspectable; a missing note is never recreated automatically.

Before planning, validate schema, managed workspace/brain and canonical relative
path; all three snapshot hashes against decoded bytes and `text/markdown` media type;
canonical original operation UUID;
original request expected revision and base; proposed bytes against the exact
submitted request; conflict ID and reason `stale_revision`. The displayed current
snapshot may be newer than the conflict's original observed preimage, but it must
be explicitly returned by the existing conflict read and become the child's exact
CAS base. An unmanaged conflict never qualifies.

## Durable seam

Module-facing method:

```rust
EditorRecovery::retain_auto_resolution(
    &self,
    expected: &Draft,
    original_request: &Value,
    conflict: &Value,
) -> Result<AutoResolution, String>

enum AutoResolution {
    Ready { draft: Box<Draft>, request: Value },
    Manual { reason: ManualReason },
}
```

The method performs no source RPC. On `Manual`, it leaves the retained original
Save untouched so the caller can use existing `record_conflict` with its exact
response. A comparison/validation/durability failure also never sends a child.

On `Ready`, under the existing local file lock it compares the entire expected
protected draft generation, requires its pending Save to equal the original
request, and requires its text to equal the original submitted bytes. It atomically
publishes one version-2 local Draft with original base, the original ConflictView,
merged candidate text and the full ordinary child `source_write` envelope. It
increments the generation and syncs both record and directory before returning.
The child envelope uses that ConflictView's current snapshot as `base`, its current
revision as `expected_revision`, and exact merged bytes as the proposal.

The child operation ID is derived reproducibly from SHA256 of the versioned
namespace `tessera-auto-disjoint/v1`, brain ID, path and original operation ID,
with NUL field separators. Use the first 16 bytes with RFC UUID version 8 and
variant bits set. It is stable for one original operation regardless of subsequent
source content: there can be only one automatic child identity. Existing source
operation replay refuses any attempt to reuse it with different input. Canonical
UUID validation is unchanged; no new dependency or UUID generator state is needed.

The local retained conflict binds the original and child while pending. The backend
original conflict record retains original request/base/preimage; the child's
ordinary recovery record retains exact merged proposal and current CAS base.
The deterministic ID makes their association reproducible without another mapping
store. The original conflict remains inspectable after the child succeeds.

## Fresh Save, retry and crash behavior

1. Fresh Save freezes the exact current editor text, loaded base, workspace and
   draft identity through existing protection/retention before its original RPC.
2. A confirmed original receipt completes normally. A transport/ACK uncertainty
   retains exactly that original operation and stops; no merge is inferred.
3. Only the known stale response to this fresh Save may reach the pure planner and
   atomic retention method. A newer durable generation refuses auto retention and
   records the inspectable original conflict without replacing newer text.
4. Only a successfully retained child may be sent. A second stale response is
   retained as an ordinary conflict; there is no recursive merge or retry loop.
5. Any lost child ACK keeps its complete pending envelope. Explicit Retry this
   Save re-sends that identical child through existing source operation replay.
   Retry never computes another candidate or substitutes a new current revision.
   Restoring a pending child reconfirms that exact record without refreshing its
   conflict; the immutable pending operation must be resolved first. Ordinary
   restored conflicts without a pending request still refresh before resolution.
6. Restoring/listing a record sends no RPC. A crash before child publication leaves
   the original pending request; a crash after publication leaves the child. An
   uncertain local durability acknowledgement requires existing recheck/confirm
   before explicit retry, even if the new record is already visible on disk.
7. A fresh Save of an already displayed conflict uses the existing deliberate
   resolution flow. It must not silently start another auto resolution chain.

The fresh-versus-recovery flag is a transient UI action property, not a retained
instruction to run later. It defaults to no auto attempt. Losing it on restart is
intentionally conservative. The local pending envelope stays the existing exact `{op,request,base}` shape.
Its containing automatic record is intentionally refused by the predecessor local
recovery reader; the source API and source journal remain compatible.

## Receipt and editor identity

Result handling must carry the **actual submitted child envelope** when automatic
resolution occurred; the original proposal is not the saved result. Only a matching
validated receipt changes `source_snapshot`, loaded revision and `source_original`.
A preview/plan or local candidate publication does not advance the loaded revision.

The generic `acknowledge` method is not used for an automatic child. A specialized
`acknowledge_auto_resolution(id, child_request, receipt)` validates the same exact
receipt plus deterministic original/child binding, then atomically clears the
pending child while retaining a Draft with its **original true base**, original
conflict/proposal, and `conflict.current` replaced by the acknowledged merged
snapshot. It preserves any newer durable text. It returns that retained Draft even
when its text equals the merged bytes; retirement waits for the native input check.
No new source RPC or write is part of this receipt handling.

If the visible text still equals the frozen original proposal, its input epoch,
workspace and draft identity still match, and the returned record contains the
merged candidate with no newer durable generation, the native callback may adopt
the acknowledged merged bytes and revision. Only after this adoption may it retire
the exact acknowledged local record using the existing generation comparison. New
text typed after adoption is based on the now-visible merged version.

Otherwise the editor keeps the newer visible text and its **old loaded base**. It
protects that text against the retained old base and displays the original conflict
with acknowledged merged current bytes. It never pairs unreconciled newer text
with the merged revision. The ordinary Save button, Ctrl+S and Save-and-continue
remain blocked by `source_conflict`; add the same fail-closed check inside
`save_source` so direct callbacks cannot bypass it. Only the distinct visible
**Save resolved draft** action may choose `conflict.current` as a CAS base after the
existing inspection warning. Do not route ordinary Save into `resolve_source`.

Concrete mandatory regression: base `A/B`, submitted `X/B`, remote `A/Y`, child
saves `X/Y`, but the user types `X+/B` while completion is pending. The resulting
newer draft retains its old base and displays current `X/Y`; the next ordinary Save
must make **zero source-write calls** and cannot revert `Y` to `B`. Inspectable
manual resolution is acceptable. Cover both newer durable text and visible text
that has not yet reached local storage, including callback ordering around receipt
and local record retirement. A different window/workspace or newer generation is
never replaced by the late result.

If the automatic child itself conflicts, its immutable ConflictView.base remains
the child CAS preimage. A v2 record whose conflict identity is an automatic UUIDv8
uses its validated Draft.base for subsequent manual merge preview and the displayed
base pane, because visible text may still be authored against that original base.
The exact backend ConflictView remains retained unchanged. A later deliberate
manual resolution has an ordinary UUIDv4 and keeps its explicitly chosen conflict
base. Regression: original A/B/C, local X/B/C, first remote A/Y/C, then A/Y/Z before
child CAS; manual preview must produce X/Y/Z, preserving Y and Z.

Navigation and close guards cover the whole original/child attempt and its local
protection work. A close waits for already-authorized work or retains its uncertain
request; it does not authorize a new auto attempt. Failure leaves recoverable bytes
and a visible conflict/uncertain Save, not a success notice.

## Evidence and compatibility

Required deterministic cases: disjoint frontmatter/body and body/body changes;
identical edits; separated/same-anchor inserts; deletion-boundary touches; repeated
text; exact UTF-8, CRLF/BOM and final-newline preservation; missing/deleted/non-text
versions and malformed snapshot/operation identities; resource refusal.

Recovery/API cases: candidate retention compares the complete original generation;
newer durable and visible drafts survive with their true base; the `A/B → X/Y`
late-edit regression blocks the next ordinary Save; failed local publish does not authorize
RPC; only one automatic child is allowed; lost original/child ACK and restart use
exact retained requests; another managed writer changes current before child CAS
and produces an inspectable conflict; old source-write clients retain their existing
behavior; original and child recovery records preserve all three versions. Reopen a version-1 ordinary draft with the new reader and preserve its existing
behavior. Reopen the automatic record with the exact predecessor implementation
and verify explicit recovery-problem reporting, unchanged file bytes and no
source RPC or local retirement. Do not claim an unsafe old acknowledgement is
compatible merely because its JSON fields can be parsed.

## Local version fence

The original `tessera-editor-recovery/v1` remains the format for newly created
ordinary drafts. Automatic retention uses `tessera-editor-recovery/v2`, with the
same fields and pending source envelope. An internal Draft format discriminator
round-trips the header and participates in full-generation comparison. All later
updates/acknowledgements keep a v2 record at v2 until explicit safe retirement;
there is no downgrade while potentially unreconciled newer text remains.

The new reader accepts both versions. Existing v1 files are not rewritten merely
by listing or upgrading. The predecessor already rejects unknown record schemas
before retention/retry, preserving the file and reporting a recovery problem
(`Editor recovery identity does not match this workspace.` in the exact predecessor).
Validate that exact behavior rather than adding a global migration/enrollment.

This local fence is necessary because an old generic `acknowledge` can otherwise
attach an unreconciled newer `X+/B` draft to the acknowledged `X/Y` revision and
permit a later blind overwrite. It does not change canonical Markdown, SourceWrite,
RecoveryRecord, backend binding, index format or ordinary source-write clients.

Native acceptance on linux-test-host uses a synthetic managed writable brain: a clean merge
shows both writers' changes immediately after Save; overlap retains both versions;
newer local text is not overwritten by late completion and the subsequent ordinary
Save cannot revert an unseen remote change; uncertain Save/restart can
recover only its frozen operation. No external task/engine side effects, live
alpha corpus, Linux reference host, shared T3 or paused Maestro project are involved.
