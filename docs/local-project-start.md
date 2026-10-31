# Local project start (#2033, path A)

Oleg chose local start on 2026-10-30. A folder, including the open vault root,
can become a project before it has a repository, goal, executor, stage or T3
connection. The product must not require internal orchestration configuration
just to keep local work in view.

## Slice 1: project and empty Board

**New project** accepts the open vault root as well as ordinary folders inside
it. A blank Folder means Vault root; the folder name supplies the initial project
name. The existing explicit creation writes the project manifest and only creates
`_index.md` when absent. A root manifest refers to `_index.md`, not `/_index.md`.
An existing manifest/index and files outside the selected vault are not replaced.
Internal hidden folders and symlink escapes retain their existing refusal boundary.
Discovery and duplicate-create checks include the root project.

Every kind starts locally: selecting Code does not opt into T3 or demand a
checkout. A saved remote connection is not called by local creation. The existing
explicit T3 checkbox still exercises its configured-host path; it is not local
T3 host/workspace verification. That later work belongs to #1801 / the next
onboarding slice, and is not claimed by slice 1.

After opening the project, **Create board** starts with its name. Goal,
Repository and stage note are optional. Saving those defaults creates an empty
Board with no executor or issue prerequisite. An unbound Board says **Local
Board** and explains that PR/CI links are inactive and where to add Repository.
Missing/empty `goal` and `repository` are valid in the version-1 Board reader;
an explicitly supplied repository still has to be a valid, matching Forgejo URL.

Repository can be added later to an empty unbound Board in **Manage board →
Board details**. Once bound it is immutable, preserving the identity of numeric
issue references. Unbound numeric issue cards are visible invalid entries and
cannot reach the network reader. They cannot silently acquire a repository:
remove those invalid references before binding. Normal bound issue cards keep
their existing validation, revision checks and Undo.

This slice keeps Create board as an explicit write; creating a project does not
automatically create a Board. No conversion of existing manifests or removal of
legacy files takes place. Older builds can reject a newly unbound/goal-free Board
until this reader reaches their channel. Existing bound Boards retain their data
and behavior.

## Slice 2: local cards

An unbound or repository-bound Board offers **Add card** in Queue and **Local
cards** in Manage board. A local card needs only a title. Details are ordinary
multiline text; no issue, expected-result field, executor, T3 connection or stage
note is required. Todo is the default. Optional manual Todo/Doing/Done states
appear in Queue/In progress/Ready respectively, marked **Local**. They never
establish executor activity, PR/CI success, installed builds or human acceptance;
local Done never enters Verified. Local cards have no agent binding and appear
under All agents rather than an executor-specific filter.

Canonical local work lives in an optional separate `ops/board-local.toml`:

```toml
version = 1
[[cards]]
id = "9864df9f-87ad-4b47-bd86-85b591f7a428"
title = "Read the project design"
details = "Optional context, not an issue reference"
state = "todo" # todo | doing | done; omitted means todo
```

The UUID is allocated once for the add draft and retained through saves, edits
and Undo. Titles and details never become inferred issue references. A missing
sidecar is quiet and no reader creates it. Existing Board/project files remain
byte-for-byte unchanged when writing local cards, so older readers continue
reading existing bound Boards without knowing this sidecar. No file conversion
or cleanup takes place.

Read failures keep the last successful local projection with a visible source
error and its age. Invalid individual cards stay visible as diagnostics beside
healthy cards; duplicate UUIDs are ambiguous and neither matching card is
selected. The normal 512 KiB manifest read/write bound applies; no truncation
or partial write is allowed. Symlinked sidecars/ops are refused.

Add/edit/remove use the existing owner-file checks, same-directory atomic
publication/replacement and exact source revision. The active project and Board
must still match the versions the form opened. A stale write keeps the draft
and reports reload rather than overwriting external edits. Remove exposes one
revision-checked **Undo remove**, restoring the previous source exactly. Other
local cards and TOML comments retain their text. Discuss emits the existing
Manager draft-only action with project/card/source references; it never sends or
launches anything. Manager host/workspace verification still belongs to slice 3.

## Remaining authorized slice

- Manager for code on the corporate Mac uses local T3 next to its checkout.
  Verify host/workspace before launch and fill the manager binding (#1801).
  Retained launch recovery and registry compatibility must retain their positive
  controls; an uncertain launch never becomes a duplicate by timeout or deletion.

Each slice gets meaningful regressions, bounded CT141 checks through the CI
queue, and Sprint before its PR. Sprint retains the Mac delivery window; no
publication between 02:20 and 09:00 Israel.
