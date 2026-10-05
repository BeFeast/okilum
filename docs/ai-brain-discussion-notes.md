# Create an ordinary goal-owned note

The native **Save as note** action extracts one saved assistant answer into an
ordinary Markdown note. It does not change the conversation or promote generated
text into an Attention decision, proposal, verified result or reviewed Context.
The scope is the current goal only. **Sources → New note** also creates a
user-authored note without requiring a conversation.

## User-authored notes

In a writable managed workspace, **Sources → New note** opens a blank review for
the current goal. Without a selected goal the action explains that one must be
selected. Read-only workspaces cannot create notes. A required nonempty title and
an optional body are entered before explicit Save; Cancel creates nothing. An
empty body stays empty, without placeholder text. The first H1 is the title, and
canonical frontmatter contains only `type: Note` and the captured `goal_id`.
User-authored notes have no `assistant_origin`, verification status or assistant
attribution. They are not proposals, Attention decisions or reviewed Context.

The review captures the workspace, endpoint and current goal. A conversation is
not part of this target: opening or changing one does not invalidate a user note.
Changing the goal or workspace cannot redirect the note or its pending reply.
The shared filename, size, create-only save, exact recovery and departure rules
below apply to both entry points. Existing Context selections and drafts remain
independent.

## Discussion review and origin

Only a nonempty saved assistant message is eligible; streaming partial output and
user messages are not. The client captures the exact workspace, goal, conversation
ID/path, message position and displayed answer. It then reads the canonical
conversation through the existing guarded `source_read` operation and validates:

- SourceSnapshot schema, brain, path, UTF-8 bytes and their revision hash;
- canonical conversation schema/type, brain, goal, conversation ID and path;
- the exact assistant role and text at the captured message position.

`chat_get` reads the application journal and can expose an answer before its
canonical projection completes. `source_read` does not flush pending writes.
Missing, pending, changed, malformed or oversized canonical origin therefore
refuses preparation and offers an explicit reload. It never fabricates a revision
or uses journal-only provenance. A later conversation edit does not rewrite the
captured source revision.

The review presents a title, body and optional filename, with clear
assistant-derived/unverified attribution. Its reviewed body must remain
nonempty. Save is explicit; Cancel creates no note. The title is a nonempty single line, at most 512 UTF-8 bytes. The complete
serialized Markdown, including frontmatter, title, attribution and body, must fit
the existing 8 MiB editor limit. Oversized input remains in the review without
truncation; RPC limits are unchanged.

The default root-level filename combines a sanitized title slug with the frozen
operation UUID and `.md`. The entire filename fits 255 UTF-8 bytes, including the
suffix. A custom filename is a simple `.md` basename with no folders, control
characters, leading dot/underscore or internal path syntax. This action creates
no directories and never overwrites a filename collision.

## Ordinary Markdown, existing scope

Discussion-derived frontmatter uses `type: Note`, the captured `goal_id`, `verification: unverified`
and an `assistant_origin` mapping:

```yaml
assistant_origin:
  conversation_id: <captured-conversation-uuid>
  conversation_path: <canonical-relative-path>
  source_revision: <actual-captured-source-revision>
  message_index: <zero-based-saved-message-position>
  original_text_sha256: <hash-of-exact-original-UTF-8-answer>
  body_edited: <whether-reviewed-body-differs-from-original-answer>
```

The first H1 is the reviewed title. A separate readable attribution links to the
captured conversation and states when the body was edited before saving. The
reviewed body retains its exact accepted UTF-8 bytes. Neither `title:` nor an
operational `record_type` is added. A message position is not presented as a stable
turn UUID, and provenance is not a verification claim.

Existing source retrieval sees this as an ordinary goal-owned note. Other goals
do not gain it as shared knowledge. Saving does not automatically select it,
change Context, call a provider or adopt a proposal. Once created, the note can be
edited through the existing Source editor. A clean saved note can then enter the
reviewed-citation draft through [Source → Use in Context](ai-brain-source-context.md),
followed by explicit Build/Review.

## Existing API and exact recovery

No backend endpoint, capability, journal field or canonical operational record
type is added. The client reuses `source_write` under the exact connected
`expected_workspace`, with the existing `ai-brain/v1` SourceWrite, a frozen
operation UUID, complete serialized bytes and `expected_revision: null`.
Top-level `base` is absent: a new note has no previous source snapshot.

`Runner::write_source_with_base` flushes pending canonical writes first. If that
fails, the review and original request remain; the client does not bypass the
projection or fabricate a base. Existing SourceStore create-only, conflict,
pending-intent and exact replay semantics remain authoritative.

The native flow validates receipt operation ID/path, null previous revision,
`written` outcome and revision against the frozen proposed bytes. It then reads
the current source and validates its identity and revision against actual bytes.
Exact content opens normally; valid later edits open with an explicit notice.
Changed, deleted, unreadable or malformed readback never triggers recreation or a
new operation. After an accepted receipt, readback retry is read-only.

An uncertain write retains its exact operation, path and bytes. Repeated Save
cannot create another note. Only a validated retained filename conflict for the
same operation and exact proposed source permits a deliberate filename change
with a new UUID. An unmanaged-writer refusal is not a filename collision.

Exact request retry works across backend restart while the client retains the
request. Completed notes remain discoverable through Sources after reopening.
There is no new desktop draft store or promise to recover an unsubmitted review
after a crash.

## Native lifecycle boundary

Existing dirty source changes must be resolved through their normal protection
before opening a note review. Independent Context drafts remain untouched.
Pre-create review guards cover navigation, normal window close and
application-owned Quit, including pending writes and late responses. They do not
extend protection to OS/Dock Quit or forced termination.

The existing EditorRecovery requires a real source snapshot and matching base
revision. A pre-create review never impersonates that state with an empty file or
fake revision. After successful creation/readback, the real source snapshot enters
the existing editor and its existing draft/close/recovery behavior applies.
