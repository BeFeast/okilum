# Typed note views (#636)

Approved owner direction: Markdown remains canonical; native view definitions
belong to the app, not special vault files. The first built-in view is `tasks`.
This first core slice defines selection and section parsing only. It does not
activate a native view, change Settings, or enable task writes.

Selection precedence is explicit frontmatter `view`, then an app-owned exact
`type → view` mapping, then Markdown. An invalid or unknown explicit override
falls back to Markdown rather than guessing from `type`. Type spelling and case
are preserved. A descriptor has a stable view ID and schema version; future
validated declarative schema sources can supply descriptors without executing
code or changing canonical notes. Built-in views must remain available offline.

````markdown
---
type: dashboard
view: tasks
---
# Today

```tasks
not done
due today
```
````

Tasks sections are top-level fenced `tasks` blocks in document order. The
immediately preceding heading supplies a plain title. Fenced examples, list
contents and block quotes do not become dashboard sections. Queries use the
existing Obsidian Tasks parser. An unsupported query or missing section returns
an explicit Markdown-fallback reason. Source line references include frontmatter
and preserve CRLF/Unicode identity; this projection never rewrites source.

Subsequent slices add validated layout defaults/overrides, app mapping
persistence, native presentation and always-available Show source. Task actions
will patch only parser-confirmed source spans under the existing revision-aware
editor lock. A stale source or active editor/draft refuses the mutation. Snooze
updates the scheduled date, not the due date. Every successful action gets one
bottom toast and revision-checked Undo using retained preimages. Native dashboard
activation waits for those safety and UI contracts, with Linux light/dark evidence.

## Revision-bound task edit plans (#674)

`task_edit::Target` captures a canonical relative path, full-source SHA-256,
line and verified checkbox range from the displayed source snapshot. Plans refuse
any source revision change and never relocate tasks by matching text. Checkbox
changes preserve unrelated metadata; due edits reject duplicate, malformed or
non-plain-text date metadata. BOM, CRLF, indentation and other task occurrences
remain unchanged. New due metadata precedes a trailing Obsidian block ID.

Plans produce exact before/after bytes, not filesystem writes. Future callers must
commit through FileEditor with its lock, durable draft and atomic conflict checks.
Undo checks the saved after-revision before proposing the exact preimage through
the same safe-write path; it refuses intervening edits. Collapsed copies must be
expanded to individual occurrences before editing. No source/MCP writes or native
controls are activated by this core-only slice.

## Safe-write and Undo integration

On Unix, `task_edit::write::apply` commits a captured occurrence through FileEditor.
It validates the vault-relative path, refuses symlinks and hard links, acquires the
same editor lock, and refuses retained dirty recovery drafts. The full captured
revision must match before creating a durable draft; FileEditor performs its own
atomic-save conflict check and archives the preimage outside the index.

A successful change returns an opaque in-process receipt. Undo uses the same lock
and save path and requires the same canonical vault and exact saved after-revision.
It restores the exact preimage, including BOM and line endings. A no-op creates no
save or receipt. Conflicts retain the recovery draft rather than overwriting newer
source. Snooze edits scheduled metadata (`⏳`) while preserving due metadata (`📅`).
This core-only slice does not activate dashboard controls or protocol writes;
platforms without FileEditor retain read-only capability.

Path validation rejects existing redirects and checks the canonical identity after
opening. It does not pin ancestor directories for the lifetime of FileEditor:
post-open parent replacement remains tracked in #699. Native write activation must
resolve that shared-editor limitation before claiming vault-bound writes under
concurrent directory replacement.

## Indexed edit evidence

The disposable Tasks index retains the full-source revision alongside each note's
tasks. A prose-only change in a task-bearing note republishes this evidence even
when visible task text stays unchanged. Files without tasks remain irrelevant.
The UI must retain the immutable index that produced a displayed row and use that
snapshot's `edit_target` on a worker with canonical source bytes. It must not look
up a newer index at click time: doing so would authorize a revision the user has
not seen. The index checks both the complete revision and occurrence membership
before creating the Target; the write layer independently checks again at save.
Cloned index snapshots retain their original evidence across updates/removals.
