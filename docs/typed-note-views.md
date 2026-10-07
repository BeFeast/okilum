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
