# Reader source editing (#354)

The approved editing step adds an explicit source mode to the desktop Reader.
The existing reader/MCP protocol remains read-only. It does not enroll a vault in
Brain or add record IDs. Live Preview, creation and rename/move are separate work.

Use the header's source button or Cmd+E (Ctrl+E on Linux). The editor shows exact
UTF-8 Markdown, including frontmatter, BOM and original line endings. Unsupported
encodings are refused, never decoded lossily. Native undo/redo and the existing
exact clipboard adapter apply. Cmd+S/Ctrl+S saves; leaving the note, losing focus,
closing the window and quitting also attempt a save.

A byte comparison with the opened base guards every changed save. An external
change blocks navigation and app-owned Quit until Reload or Keep mine resolves
it. Compare shows the current disk source above the editable draft. Keep mine is
explicit and checks the reviewed disk version again; a subsequent change remains
a conflict. No conflict markers or automatic merge are written into notes.

Every native mutation, including undo/redo, queues a background recovery write.
Obsolete writes cannot replace newer drafts or a completed save. The editor shows
“Protecting draft…” until the latest write completes; a crash during that interval
can recover only the last completed draft. Save and lifecycle exits synchronously
flush the current buffer before proceeding.

Drafts are atomically persisted in `editor-drafts` under application state,
separate from the disposable index. Reopen the note and enter source mode to
recover an unsaved draft. A clean journal never supersedes newer disk content.
If the file was deleted, moved or became unwritable, use **Copy draft** or
**Leave, keeping draft**. The latter exits only after protecting the current
buffer; restore a moved/deleted note at its original path to reopen its draft.
A second Tessera editor for the same canonical path is refused while it is open.

Saves write and sync a temporary file in the same directory, then atomically
exchange it with the note (Linux/macOS). The displaced inode is retained as a
hidden `.tessera-save-*` file. If it differs from the checked base, the exchange is
reversed and reported as a conflict. Both displaced versions remain recoverable;
there is no claim that advisory locks exclude Obsidian or sync. An unrelated
writer racing that exchange can briefly observe the proposed file. A crash during
commit leaves the durable draft and displaced inode available rather than losing
either version. Recovery files currently remain until manually removed.

File permissions are preserved. Symlinks, hard-linked notes and unsupported
atomic-exchange filesystems fail closed. Native GUI behavior on macOS is checked
on the beta build by the user; automated source and lifecycle tests run in CI.

## Unicode pointer safety (#398)

Ordinary source clicks position the cursor. Command-click (the platform primary
modifier) follows a link parsed from the current, exact source. Native hit tests
carry a source generation; pending clicks are hit-tested again after the current layout arrives. Events
whose source changes between emission and delivery are ignored.
Navigation first completes the existing revision-aware save; conflicts keep the
draft open. Missing and ambiguous targets retain the Reader's explicit notices.

The crash-producing range was in the shared backlinks panel: after truncating a
context at byte 160 and appending `…`, it clipped the *original* highlight to the
new length. For 158 bytes of prefix followed by a four-byte Cyrillic link label,
158..162 then ended inside the new three-byte ellipsis. Backlink ranges now clip
to the retained original prefix before the ellipsis is appended. GPUI-bound
highlights and rendered inline runs assert UTF-8 boundaries in debug builds and
clamp malformed ranges to preceding character boundaries in release builds.
Neither rendered ranges nor character counts are reused as source offsets.

An aborted-child-process regression verifies that a completed draft journal write
survives SIGABRT without saving the note, and still detects an external change on
reopen. While “Protecting draft…” is visible, only the last completed journal
write is guaranteed to survive abrupt termination.

Startup records a synced launch marker outside the note/index directories. A
marker whose process no longer holds its lock means the previous launch did not
complete shutdown; live concurrent launches are excluded. After such a launch,
the last note opens in reading mode with an explanation. Draft discovery never
constructs an editor or writes a journal. “Restore unsaved edits” explicitly opens
the recovered source; automatic startup never does so. The same button is offered
for surviving drafts from older builds that did not have a launch marker. A clean
shutdown clears the marker only after Reader drafts have been saved successfully.

### macOS state locations and crash-loop recovery

- `~/Library/Application Support/uk.oklabs.tessera/`: Reader last-document history
  (`update-session.json`), durable `editor-drafts/`, and `reader-runs/` markers.
- `~/Library/Application Support/tessera/` (`config_base/tessera`, or
  `$XDG_CONFIG_HOME/tessera` when explicitly configured): `appearance.json`,
  `reader-layout.json`, and a second `reader-runs/` marker. Moving only the
  bundle-id directory aside did **not** break the reported loop; moving this
  config directory did. This identifies configuration-dependent reproduction,
  not automatic source-session restoration.

An unclean marker in either location activates the guard. The last note still
opens in read mode, but saved appearance and panel widths are bypassed, panels
start closed, and saved managed connections are not started automatically.
Preferences and drafts remain in place; nothing is renamed, deleted or migrated.
Tests check both marker locations and unchanged preferences while defaults apply.
The two-directory layout is retained for compatibility in this hotfix; future
consolidation under the bundle-id must migrate explicitly and preserve conflicts
and recoverable drafts, rather than silently choose one copy.

## Save feedback and reproducible QA (#414)

The editing header shows **Edited**, **Saving…**, **Saved**, or **Conflict**.
Saved's tooltip gives the last successful save/check time. Explicit Save yields
one frame before its synchronous safe write so the queued state can render;
blur, app deactivation, navigation and quit still complete saving synchronously.
Draft journaling is separate from saving the canonical note. “Protecting recovery
copy…” may be too brief to see; it is not the test for whether the file was saved.
A clean source buffer updated by the watcher shows “Updated from disk”. A dirty
buffer is retained and marked Conflict. No automatic Undo of an external write is
offered: restoring an old file would itself require a revision-aware conflict check.

Switching to Terminal or Obsidian normally saves the note before any external
change; no conflict is expected in that sequence. On 6053 the watcher also refused
to replace an open source editor, so the reported replacement cannot be ascribed
to that path without the exact sequence. This version explicitly supports clean
source refresh with a notice.

On a disposable copy, select its absolute path in Terminal (replace the example):

```sh
python3 - '/absolute/path/to/COPY.md' <<'PY' &
import pathlib, sys, time
p = pathlib.Path(sys.argv[1])
time.sleep(20)
with p.open('ab') as f:
    f.write('\nExternal conflict test — 🧠\n'.encode())
PY
```

Immediately return to Tessera, open that COPY in source mode (Cmd+E), type a
recognizable line and confirm **Edited**. Stay in the editor; do not save, click
another control, or switch apps. After 20 seconds plus watcher latency, expect
**Conflict** with your line intact. Compare must show the external marker; Cmd+S
must not overwrite it. If the header became Saved before the command fired, the
prerequisite failed: repeat without leaving the editor.

For recovery, schedule `sleep 20; pkill -9 -x tessera` in Terminal in the background,
then immediately return and edit the COPY. Wait for recovery-copy protection to
finish while remaining focused and Edited. Relaunch should offer Restore unsaved
edits. Switching apps to issue the kill after editing instead tests an autosaved
note: no Restore is then expected. The dismissible banner reports that no unsaved
edits were lost only after successful draft discovery finds no newer saved draft;
checking/error states do not make that claim. Only completed journals survive a
kill; a process cannot recover keystrokes that never reached durable storage.

## Create ordinary notes (#361)

Reader More → **New note…** (Cmd+N), or a real folder's context menu →
**New note here…**, opens the native name/folder picker. It starts in the current
note's parent or selected folder. Choose an existing folder inside the open vault;
a missing extension becomes `.md`. Other extensions, paths outside the vault and
symlink folders are rejected. Cancel leaves the current note unchanged.

Creation saves the current draft first and refuses to proceed on conflict/error.
An exclusive create never replaces an existing file, including a concurrent
creator or dangling symlink, even if the native picker asks about replacement.
The empty file and its containing directory are synced, then it opens immediately
in source mode. Edits use the same durable journal and revision-aware first save
as existing notes. A failed sync retains the created file and reports the error;
retry never silently replaces it. No frontmatter, record IDs or Brain enrollment
are inserted. The watcher adds the file to the folder/search inventory.

## Rename / move ordinary notes (#360)

More → **Rename / move note…** chooses a new `.md` name or existing real folder
inside the open vault. A separate preview shows old/new paths, known indexed
incoming wikilinks (including ambiguity), and warns that incoming links and
relative outgoing links/attachments can change meaning. This first operation
explicitly offers **Cancel** or **Move without updating links**. It does not
rewrite any link text; link rewriting requires a separate explicit diff preview
and is not offered here. The indexed list is labelled as incomplete, never a
claim that a note has no other references.

The current source draft must save successfully first. A recovery draft found
while in read mode must be restored/saved explicitly; another editor's path lock
blocks the operation. The preview pins the source bytes and inode. Changes after
preview require a fresh preview; an atomic no-replace rename handles concurrent
destination creation without overwriting either file. Symlink/hardlink sources,
symlink folders, outside-vault destinations and cross-filesystem moves are
refused. A last-moment external change is retained and reported after the move;
there is no unsafe rollback over a newly created source. Directory-sync failures
are reported as a completed move requiring inspection, not a retryable non-move.

A destination with a dirty recovery journal or active editor is refused; its
journal is preserved. Both path locks are held while the confirmation is open.
Case-only renames on case-insensitive filesystems are refused as collisions.

The active window follows the new path, preserving source/read mode and updating
its path-based history/pins. Other open windows and authored links retain their
original path identity; they are not silently redirected to a namesake. If a
post-move race or sync warning occurs, the destination opens in read mode for
inspection instead of automatically re-entering source mode. File
contents (including BOM, CRLF and frontmatter) are unchanged by rename/move.

## Update links during rename / move (#440)

This extends the #360 flow above. A scrollable preview lists each changed target
with its source note and line, including wiki aliases/heading/block fragments,
embeds, Markdown destinations, frontmatter relations and outgoing relative
attachments. The default action is **Move and update N links in M notes**;
**Move without updating** and **Cancel** remain explicit alternatives.
Only destination byte ranges are replaced. Labels, aliases, fragments, BOM,
line endings and other source bytes remain untouched. YAML is never serialized;
a replacement that would change its quoting/decoded values is listed as requiring
manual update. Ambiguous, unresolved and unsupported targets appear under
**Not updated** with reasons. Reference-style Markdown links require manual update.

Unsaved affected editors block the move; this flow does not silently save them.
Clean editors reload after link updates. All scanned note revisions and the
folder inventory are checked again before any canonical write. A stale preview
is rebuilt for explicit approval. A destination collision never overwrites a file.
Per-file saves are atomic and revision-aware; the source note is moved last.

Before applying changes, a mode-0600, synced journal under the durable application
state directory `link-moves/` records every affected file's before/after bytes.
This directory is separate from the rebuildable index. Mid-operation failures
name files already changed and point to **More → Recover link moves…**.
Recovery remains discoverable after restart, including an interrupted completion
acknowledgement. Select an operation to inspect its file list and explicitly revert.
Dirty source editors block recovery; clean affected windows reload. Revert refuses later external edits
and source collisions; it never overwrites those versions. A partial revert can
be retried after resolving the reported obstruction. Preimages are retained;
general retention and cross-feature recovery remain tracked separately in #362.

Check on a copied vault: move a Cyrillic note containing an emoji, wiki alias and
block embed, an incoming relative Markdown link and a frontmatter relation. Read
the occurrence preview, apply, then follow each link and compare source bytes.
Keep a dirty affected editor open to confirm blocking. Change a referring file
while preview is open to confirm no stale writes and a refreshed preview. Finally
close affected editors and use Recover link moves to restore the original paths
and bytes; a later external edit must cause recovery to stop without overwriting it.

## Source history and recovery (#362)

More → **Note history…** previews and restores previous source versions.
**Recover notes…** also finds unsaved drafts for moved/deleted paths and
unassigned legacy displaced files; recover them to a new name without replacing
an existing note. Retention and recovery details: [source history](source-history.md).

### Indexed move preview (#471)

Reader publishes a rewrite candidate index from the existing warm source snapshot
after Reader readiness has been published; building it is not a readiness gate.
It uses the lossless link parser, including
wiki aliases/fragments, embeds, Markdown destinations and frontmatter relations;
the display backlinks alone do not cover all of these forms. Basename candidates
are conservative and include unresolved/ambiguous targets and both old/new names.

Preview walks directory metadata and verifies shared source revisions, then reads
only candidates, the moved note and changed/new/uncertain sources. It never treats
a stale watcher as proof of unchanged content. Before application, unchanged
non-candidate revisions and inventory must still match; edited files retain exact
byte comparisons. New links or changed inventory require a fresh preview.

Preparation runs in the background with progress and cancellation. If the index
is unavailable, **Scan folder** explicitly starts the full-scan fallback. Cancel
never changes canonical files. Native save panels and background application are
separate remaining UI work.

`reader-diagnostic.log` records `MOVE_PREVIEW` with plan, inventory, candidate
revision checking, reading, rewrite and total milliseconds, plus `indexed` and
`files_read`. On macOS the default location is
`~/Library/Application Support/uk.oklabs.tessera/reader-diagnostic.log`.

Same-session Linux fixture comparison (5,001 notes, four referrers; debug build,
local disk, OS cache warm): full scan 2,580–3,086 ms / 5,001 source reads, indexed
136–190 ms / 5 source reads. Both produce identical changes. This does not establish
iCloud latency; use the phase log from the actual Mac to verify the <1 s target.
The reproducible focused profile is `link_rewrite::tests::profile_preview_5000_notes`
(run explicitly with `--ignored --exact --nocapture`).

The #502 gate profile also supports `TESSERA_PREVIEW_READ_DELAY_MS=1` (one
artificial millisecond per source read, not a model of all iCloud behavior).
On the same 5,001-note Linux fixture, paired previews took 8,827–9,803 ms with
full reads versus 101–142 ms indexed, with identical edits and 5,001 versus 5
reads. Snapshot reconciliation took 6,745 ms; candidate-index construction added
1,490 ms when performed inline. It now runs after Ready, with a root/generation
check before publishing, so that work does not extend the readiness dependency.
These are core preparation timings, not an end-to-end macOS startup measurement.
