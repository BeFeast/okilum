# Reader source editing (#354)

The approved editing step adds an explicit source mode to the desktop Reader.
The existing reader/MCP protocol remains read-only. It does not enroll a vault in
Brain or add record IDs. Live Preview, creation and rename/move are separate work.

Use the header's source button or Cmd+E (Ctrl+E on Linux). The editor shows exact
UTF-8 Markdown, including frontmatter, BOM and original line endings. Unsupported
encodings are refused, never decoded lossily. Native undo/redo and the existing
exact clipboard adapter apply. Cmd+S/Ctrl+S saves; leaving the note, losing focus,
closing the window and quitting also attempt a save.

Typing coalesces canonical saves after one second idle, with a two-second ceiling
for continuous input. Recovery keeps its separate 300 ms pause. Focus loss
immediately hands pending recovery to a worker and requests canonical publication;
conflict, deletion, a move or IME may defer publication, but never that recovery
request. A request during an in-flight save follows its completion without another
idle delay, including failure/conflict outcomes. An unfocused IME commit resumes
publication. Quit/navigation retain synchronous latest-revision protection. File
publication remains serialized, revision-checked and atomic for external readers.

Large unwrapped editor layouts retain exact native shapes across popup/caret
frames in a separate 160 MiB cache (at most 128 entries), shared by all scripts.
Only printable ASCII lines use incremental native word-context shaping. Admission
requires reproducing the complete native layout exactly from font advances and
native word boundaries; each changed context is checked too. Replaying the pen
and word-width additions in native order preserves exact global geometry even
past 2^24 px. Tests compare every glyph, width, caret and far-end hit/selection
position; deliberately shifted/dropped glyphs fail the same acceptance check.
Any Unicode insertion retires the matching ASCII edit state and uses a full
native reshape. A native word context above 64 KiB still takes the full path
(for example a numeric array/base64 value without a safe break). This remains a
typing latency limitation, separate from exact reuse of unchanged rows and menu
frames. Backends/fonts that fail exact admission retain their full native path.

Latency diagnostics observe a second frame callback after the mutation frame's
draw, rate-limit detailed observations above 32 ms to one per second, and record
periodic aggregates. They include process-wide Reader render counts and contain
no authored text; they measure native submission, not physical display latency.

Incoming changes to an open note (#2486) are observed through a shallow parent
watch independently of vault indexing, including Syncthing's rename over the
original file. Clean Source and Live Preview buffers reload in place while
retaining the source selection and scroll. A native reset is bound to the refreshed base, so repeated incoming changes
do not turn a clean editor into an apparent unsaved edit. Dirty buffers retain their text and show
a conflict notice; an already active Sync controller may add the uniquely
identified remote computer name. Unavailable or ambiguous provenance remains
unattributed rather than guessing a computer.

Explicit Save during that conflict, or **Keep both versions**, creates a complete,
create-only `*.sync-conflict-YYYYMMDD-HHMMSS-XXXXXXX.md` copy of the latest local
buffer next to the note and names it in the notice. The incoming canonical file
and previous conflict copies remain unchanged; the draft and conflict stay open
for comparison or explicit resolution. Repeating Save for the same unchanged
copy reuses it; later local edits produce a new copy. Autosave stops at the
conflict and retains the recovery draft. This supplies the keep-both preservation
boundary of #2044 without claiming its full conflict-resolution workflow.

Within the same Reader window, choosing another supported Markdown note keeps the
chosen Read, Source or Live Preview mode (#1723). An explicit return to Read keeps
later notes in Read. File switching still completes the existing save/recovery
checks before leaving the current editor, and only the latest successfully
prepared note may reopen its editor. Other formats retain their existing preview;
a failed or unsupported open does not force editing. Window/session restoration
continues to use the existing saved mode policy, without a new global preference.
New note temporarily opens Live Preview for typing without changing the window's
chosen mode for other notes. Restoring an editor after a sidebar selection keeps
keyboard focus in the tree (#1730).

Contents heading clicks target the visible document (#2020). Read retains its
block landing. In Source and Live Preview the shared editor receives focus, the
caret moves to the authored heading's first column and that line scrolls to the
top. Frontmatter and original line endings count toward the authored line; body
edits in an unsaved draft may shift it. If a changed heading cannot be identified
unambiguously, a notice asks to refresh Contents in Read. Navigation never edits
or saves the source. Results from an older click, document, text revision or caret
position cannot move the current editor.

Find from Live Preview temporarily shows Source. Closing Find restores Live
Preview with the matched source row at its previous viewport position, including
large notes and newly measured rendered blocks (#2484). This handoff leaves the
editing selection, canonical text and Undo history intact. Heading and Back
landings wait for the current document's parse, and can initialize a prepared
Reader list before it is painted; an editor hiding that list cannot cause a
rendering timeout. Opening Find supersedes an older Reader-to-editor handoff,
including one that finishes after Find closes; an asynchronous placement retains
later keyboard focus. A new navigation or editing retires an obsolete landing notice.

Copy as Markdown uses Ctrl+Shift+C (Cmd+Shift+C on macOS) in Read, Source and
Live Preview (#2625). In the editors it copies the selected authored Markdown,
including markup and original line endings, and appears beside Copy in the
context menu. Plain Copy keeps its existing behavior. Read offers Copy as Rich
Text on Ctrl+Alt+C / Cmd+Option+C (Ctrl+Alt+Shift+C on Windows), which writes rich HTML plus plain text for
other applications. Its menu shows each command's live key bindings. Editor
Paste shows Ctrl+V / Cmd+V as the primary shortcut; Shift+Insert remains available.

Heading completion (#1638) uses the same inline popup and keyboard/Undo path as
note completion: type `[[#` for headings in this unsaved note, or
`[[Note#` for headings in a uniquely resolved note. Up/Down chooses a row;
Enter/Tab or a row click inserts the target once. Escape leaves the authored
text unchanged. The shared `HeadingInventory` excludes ambiguous or unsupported
heading destinations; completion does not guess between duplicate note names.

Completion refresh copies at most 1 KiB before the caret on its current line,
clips the window to UTF-8 boundaries, and shares an immutable Rope snapshot. Ranking and full Markdown-context checks run on the
background executor; code spans, code blocks and frontmatter publish no rows.
A paste ending in a trigger leaves the popup closed; subsequent typing can open
it normally. Enter/Tab during a pending note query remembers that exact query
and accepts its fresh best match once, subject to the same source, caret, focus,
vault and IME guards. Escape cancels the pending acceptance.

Type badges are derived presentation metadata, loaded only for the visible page.
Unchanged native file identity/change time reuses the bounded cache without
opening the source. A new watcher vault snapshot clears the cache, including
coarse/network filesystem changes; deleted/unavailable notes clear their badge.

Named headings are read and parsed off the UI thread from a contained, regular,
resident UTF-8 file, bounded to 8 MiB. Their rows retain the resolved note path,
native file revision and exact-byte digest. A changed target is recomputed before
publication; choosing a row validates its target again before the source edit.
If it changed or disappeared, the draft stays intact and the popup refreshes.
The editor source, caret, input session, vault, focus and composition guards also
apply to this asynchronous acceptance. This is an observed revision check, not a
lock against subsequent external writes. Current-note completion uses the unsaved
editor snapshot. Remote hydration and unsupported/oversized target files are not
performed by this provider; Windows reparse targets are refused. Platform/native
acceptance is separate from the headless source fixtures.

Live Preview smart punctuation (#1644) changes presentation only. Paired ASCII
quotes use curly English quotes, or `«…»` in paragraphs with Russian prose;
in-word apostrophes use `’`, `--`/`---` use `—`, and `...` uses `…`. Escaped
punctuation, incomplete quote pairs, literal code, math, frontmatter and unsupported
source remain authored. Language and quote pairing reset at a blank line. Only
classifier-approved prose participates; an excluded neighbour cannot authorize an
apostrophe conversion, and both ends of a quote pair must be approved grapheme
boundaries.

The current caret/selection, IME composition and explicit replacement reveal the
authored punctuation in their prose block. Other inline syntax keeps its existing
reveal policy; untouched code and formulas may stay rendered. The immutable
painted frame retains the mapping
for its own revision: a hit on a substituted glyph maps to its original source
edges before reveal or reflow. This shares the existing inline math overlay and
adds no canonical text edit or history transaction. Source mode, Copy, safe Save,
Undo/Redo, BOM and CRLF keep their original bytes; typing still changes the file
only by the user's authored text. Headless fixtures cover rendered mappings and
the shared mounted editor's Copy/save/history boundary. Native appearance and
large-vault performance remain separate acceptance.

Typing a third backtick or tilde at a Markdown line start adds its closing
fence (#1893). The caret stays at the opener for the language; Enter enters the
empty block. An adjacent empty closer grows with a longer opening fence.
Existing code bodies/closers and nonempty line suffixes stay literal. One Undo
removes the assistance while retaining the typed opening marks. The independent
Settings → Editor → Close code fences switch defaults on, including in older
profiles without that key. Quote markers and original indentation are retained;
each opening-line list marker, including an ordered marker, becomes continuation
spaces at its own nesting level. Fence marks on the same line after a task
checkbox (for example, `- [ ] ``` `) remain literal task text: assistance does
not insert a closer, split the line or rewrite the authored source. A real fence
on a list/task continuation line remains eligible, preserving that line's indent
and quote markers. Source and Live Preview use the same hook and
existing IME guards, exact line endings and native Undo transactions.

Document zoom scales code together with prose in Read and Live Preview (#1737).
Inline code inherits the surrounding text size; rendered fenced blocks scale
their themed monospace size by the same zoom factor, including inside callouts.

Windows Source and Live Preview use an app-owned exact Unicode clipboard reader
(#1434). A restricted mode of the same executable reads `CF_UNICODETEXT` before
updater, state or GUI initialization. Its parent limits the entire request to two
seconds and 8 MiB of UTF-8, cancels and reaps the child, and retains the editor's
source, selection and focus guards. Strict UTF-16 decoding preserves authored
Unicode, BOM and line endings. Empty text is distinct from unavailable or denied
clipboard data; changed offers and malformed text are refused. Plain/code files
and ordinary text fields retain this exact-text path.

Markdown notes in Windows Source and Live Preview additionally read the native
`HTML Format` flavor through a note-specific asynchronous provider (#2241).
CF_HTML fragment offsets are UTF-8 byte offsets, and valid HTML reaches the same
Markdown converter used on macOS and Linux. Labels and destinations remain
independent, including Unicode labels, destinations differing by a trailing slash,
and links styled in monospace. Missing or malformed HTML falls back to the same
clipboard offer's declared plain text. Explicit Paste as Plain Text and fenced
code skip HTML payloads. The existing request deadline, cancellation, changed-offer,
source/selection guards and atomic Undo still apply.

Linux reads the clipboard inside the paste command (#1602), so no activation,
focus or selection change can cancel it. On a named Wayland compositor with
data-control (wlroots compositors such as Hyprland and Sway, KDE) it reads the
regular selection's exact UTF-8 text, within two seconds and 8 MiB. GPUI on X11,
GNOME's Mutter (no wlr data-control), an inherited `WAYLAND_SOCKET`, several seats
and Okilum's own copy go through GPUI's clipboard connection instead: X11 text
arrives byte for byte; GPUI's Wayland reader folds CRLF into LF, as the editor's
document newlines (#848) do to pasted text anyway. Shift+Insert pastes like
Ctrl+V: Linux clipboard managers and dictation tools send it. A paste that inserts
nothing says why.

Smart paste (#1611) decides by the clipboard and the caret, first match wins:
inside a fenced code block the plain text (with the fence's quote or list
prefix); files, copied into the attachment folder in the background and
embedded (vault files are linked in place; above Settings > Editor's threshold a
Copy / Link to Original / Cancel prompt); image data, saved and embedded; IDE
code (VS Code editor data, or all-monospace HTML/RTF over several lines) as a
fence with its language; HTML or RTF converted to Markdown by `okilum-paste`,
remote images downloaded in the background; a selection plus one plain URL as
`[selection](url)`; TSV as a table; plain text. A copied-link title can still wrap
the selection when its HTML confirms the exact plain URL. Otherwise rich text
precedes the selection-link shortcut so a URL-looking Slack label cannot replace
its actual destination. Paste as Plain Text is
Option+Shift+Cmd+V (Ctrl+Shift+V elsewhere); Paste as Titled Link is
Cmd+Alt+V / Ctrl+Alt+V (Ctrl+Alt+Shift+V on Windows). Files dropped onto the editor take the files path.
Background results land only in the editing session of the note they were
pasted into; otherwise a toast names the saved file. macOS supports rich-text, image and file offers. Linux also routes native
PNG/JPEG and other supported image MIME offers, local `text/uri-list` files,
and `text/html` alongside its plain-text fallback through the common Smart paste
router. HTML images use the same attachment pipeline before an embed is inserted;
Paste as Plain Text keeps the plain flavor. The Wayland data-control read
keeps the same pinned-offer checks, deadline and payload limits. Where GPUI
runs on X11, the locked GPUI clipboard exposes only image or plain text, so
Okilum reads the CLIPBOARD selection's targets (`text/html`, images,
`text/uri-list`, UTF-8 text, including `INCR` transfers) over its own
short-lived X11 connection inside the paste command, with the same MIME
negotiation, deadline and limits (#2006); GPUI core stays unpatched. Okilum's
own copy (owner process named by the X-Resource extension), servers without
X-Resource, self-owned Wayland selections and unsupported compositors use
GPUI's typed clipboard on the UI thread.
Plain text containing HTML syntax stays literal instead of being guessed as an
HTML clipboard offer. Windows note HTML uses the bounded path described above.
Completed local attachment writes publish their exact saved paths to inventory, so a
paste or drop does not wait for filesystem notifications to display its image.

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
A second Okilum editor for the same canonical path is refused while it is open.

Saves write and sync a temporary file in the same directory, then atomically
exchange it with the note (Linux/macOS). The displaced inode is retained as a
hidden `.okilum-save-*` file. If it differs from the checked base, the exchange is
reversed and reported as a conflict. Both displaced versions remain recoverable;
there is no claim that advisory locks exclude Obsidian or sync. An unrelated
writer racing that exchange can briefly observe the proposed file. A crash during
commit leaves the durable draft and displaced inode available rather than losing
either version. Recovery files currently remain until manually removed.

File permissions are preserved. Symlinks and hard-linked notes fail closed.
Filesystems rejecting exclusive rename/exchange use the revision-checked,
create-only [compatible publication path](network-write-fallback.md), with its
explicit brief missing-name interval and durable captured-source recovery.
Drives that also reject hard links fail safely. Native GUI behavior on macOS is checked
on the beta build by the user; automated source and lifecycle tests run in CI.

## Notes deleted outside Okilum (#1986)

When the note on screen is deleted by a file manager, `git checkout`, sync or
a script, Read and the editor keep showing its contents. A persistent notice
over the note (no modal, no layout row) says it was deleted and offers
**Restore**, which writes the contents back at the note's own place, and
**Save As…**. Restore never replaces a file that appeared there meanwhile; in
the editor the restored file becomes the editor's base, so saving continues.
The header strikes the title through and adds **Deleted**; clicking it shows
the notice again after it was closed.

In Read, Source and Live Preview, opening another note, file or vault or closing
this note first asks **Save As… / Don’t Save / Cancel**. Save As… continues only
after complete, synced bytes are published to a new filename. Existing files,
including a destination that appeared while the picker was open, are never
overwritten. Cancelling the picker, a failed save, a stale document or edits made
behind the picker leave the note open. Don’t Save continues without writing the
canonical file; editor recovery drafts remain available. Failed recovery writes
still prevent navigation. Navigation also checks for deletion before a debounced
watcher notification arrives.

Closing the native window or choosing **Quit Okilum** (Ctrl/Cmd+Q) uses the same
choices (#2651), rather than leaving a toast that names unavailable conflict
actions. Cancel or Escape keeps the window open; Enter does not discard. Save
As only resumes the close after the complete copy is saved; picker cancellation,
a destination collision, changed contents or failed recovery persistence keeps
the window open. Explicit Quit checks every Reader window, presenting one
deleted-note decision at a time before its ordinary editor/managed-app guards.
This covers application-owned Quit, not forced OS/process termination.

Read-only recovery discovery also recognizes a dirty journal after its source
file disappears (#2427). It resolves the existing parent for the saved identity,
leaves the journal bytes intact and never recreates the source. A returned file
is compared with the draft before discovery reports it as unsaved.

An explicit Move to Trash that completes successfully closes its unchanged
document without asking the external-deletion question again (#2427). That
authorization follows the confirmed document identity and navigation generation,
and is used only after the Trash receipt. If an earlier external version was
already retained on screen, its leave confirmation remains: the item in Trash
may be a different, returned version. Undo restores that disk version without
replacing the retained display.

If the file returns at the same place with the same contents as the retained
buffer, the notice and marker go away. Different contents remove the Deleted
marker but keep a notice offering **Reload from disk** or Save As… in every mode.
Even a clean Source/Live Preview buffer stays intact until an explicit decision;
autosave and shared-window refreshes cannot silently replace it. The editor also
keeps its explicit Reload / Keep my version / Compare choices. The deleted note
does not remain in the folder tree; a list of recently deleted notes is out of
scope. These rules apply to both vault and single-folder windows on all desktop
platforms.

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

- `~/Library/Application Support/com.befeast.okilum/`: Reader last-document history
  (`update-session.json`), durable `editor-drafts/`, and `reader-runs/` markers.
- `~/Library/Application Support/okilum/` (`config_base/okilum`, or
  `$XDG_CONFIG_HOME/okilum` when explicitly configured): `appearance.json`,
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
On Windows, reopening a journal whose save was not acknowledged keeps its draft
pending even if the file already contains the proposed bytes (#2551). Explicit
Save verifies the current bytes before acknowledging the journal; protected save
history remains available. An acknowledged clean journal never replaces newer
external content.
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

Immediately return to Okilum, open that COPY in source mode (Cmd+E), type a
recognizable line and confirm **Edited**. Stay in the editor; do not save, click
another control, or switch apps. After 20 seconds plus watcher latency, expect
**Conflict** with your line intact. Compare must show the external marker; Cmd+S
must not overwrite it. If the header became Saved before the command fired, the
prerequisite failed: repeat without leaving the editor.

For recovery, schedule `sleep 20; pkill -9 -x okilum` in Terminal in the background,
then immediately return and edit the COPY. Wait for recovery-copy protection to
finish while remaining focused and Edited. Relaunch should offer Restore unsaved
edits. Switching apps to issue the kill after editing instead tests an autosaved
note: no Restore or crash message is then expected. A newer draft produces a
four-second Restore toast; source mode still offers recovery after it closes.
Draft-check errors use a dismissible error toast and never claim that edits are safe. Only completed journals survive a
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
On macOS, a case-only rename is accepted when both names identify the same file
in the same real directory. The atomic no-replace rename still refuses a distinct
destination. Recovery uses the stored directory-entry spelling to undo such a
rename, including an interrupted completion acknowledgement. Linux and Windows
collision behaviour is unchanged.

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
`~/Library/Application Support/com.befeast.okilum/reader-diagnostic.log`.

Same-session Linux fixture comparison (5,001 notes, four referrers; debug build,
local disk, OS cache warm): full scan 2,580–3,086 ms / 5,001 source reads, indexed
136–190 ms / 5 source reads. Both produce identical changes. This does not establish
iCloud latency; use the phase log from the actual Mac to verify the <1 s target.
The reproducible focused profile is `link_rewrite::tests::profile_preview_5000_notes`
(run explicitly with `--ignored --exact --nocapture`).

The #502 gate profile also supports `OKILUM_PREVIEW_READ_DELAY_MS=1` (one
artificial millisecond per source read, not a model of all iCloud behavior).
On the same 5,001-note Linux fixture, paired previews took 8,827–9,803 ms with
full reads versus 101–142 ms indexed, with identical edits and 5,001 versus 5
reads. Snapshot reconciliation took 6,745 ms; candidate-index construction added
1,490 ms when performed inline. It now runs after Ready, with a root/generation
check before publishing, so that work does not extend the readiness dependency.
These are core preparation timings, not an end-to-end macOS startup measurement.

## Inline creation and vault templates (#466)

New File / New Folder in the folder context menu or Folders header starts an
editable tree row. ⌘N (Ctrl+N on Linux) uses the current note's folder and opens
the sidebar. Enter creates; Esc cancels. Names may include nested folders;
Markdown names imply `.md`. Invalid names and collisions remain inline.
Intermediate folders created before a later failure are kept; no existing file
is replaced. Windows remains read-only.

New notes use `Note.md` from `_Assets/Templates/`, or the folder named in
`.obsidian/templates.json`. If Note.md is absent, the built-in source has
`type: Note`, `created: YYYY-MM-DD`, and an H1 matching the typed filename stem.
The inline ⌄ picker chooses another template or the built-in note for this
creation only; the next New File returns to Note.md. No per-folder overrides.

Templates support `{{title}}`, `{{date}}`, `{{time}}`, `{{date:FORMAT}}`,
and `{{time:FORMAT}}`. Formats support Moment tokens YYYY, MM, DD, HH, mm, ss
and bracketed literals; bare date/time use Obsidian's dateFormat/timeFormat or
YYYY-MM-DD / HH:mm. Unknown variables and unsupported formats remain literal.
Substitution is one pass: inserted titles are never interpreted as templates.
Other source bytes, including BOM and line endings, stay unchanged.

The templates folder appears normally in the tree. New File from a template
uses the vault root, and new-note destinations inside the template collection
are rejected. Templates and settings must be regular UTF-8 files, at most 1 MiB;
symlinks, invalid settings and unreadable selected templates abort with an error.
Windows stays read-only. Orphan drafts and active writers block note creation.

Inline creation uses an explicit Settings → Vault template-folder preference when
present, ahead of the Obsidian folder setting. Unrelated hidden `_Assets` branches
stay hidden when no template collection exists.

## System Trash and Undo (#466)

Move to Trash is an ordinary Reader action, available in the tree context menu
and through ⌘⌫ (Ctrl+Backspace on Linux) outside text inputs. Files with incoming
links require a compact warning before moving. Unlinked folders move immediately
with Undo; empty counts and Markdown extensions are omitted from the warning (#621). Current edits
must save successfully first; other source writers and orphaned drafts block the
operation. Links are left unchanged. Preparation and filesystem work run off the
UI thread. A deleted open selection closes to the empty vault view.

macOS uses NSFileManager's system Trash API. Linux writes freedesktop Trash
metadata and moves within the same filesystem (home Trash or a private per-user
Trash at the filesystem root); an unavailable Trash leaves the original in place.
The notification's Undo restores the exact returned Trash object. It refuses
occupied original paths, replaced objects and symlink parents; it never overwrites
another file. The notification hides after eight seconds; keyboard Undo remains available for
the session. After restart, use Finder
or the system file manager to restore items from Trash. Windows stays read-only.

QA on copies: trash a note and Undo, then repeat after creating a replacement at
the original path (Undo must refuse); cancel deletion of a linked note or folder
and verify every source is unchanged; inspect the actual system Trash.

### Inline rename and background application (#471)

Rename / move focuses an input in the sidebar row, containing the vault-relative
path. F2 or Enter on a selected note opens the same input; Space opens the note.
Folder Enter still expands/collapses it. Enter submits a destination such as
`Projects/Meeting.md`; Escape cancels without writing. Existing destinations and
paths outside the vault remain errors shown beside the input. The explicit
New note from template action also uses the inline creation row.

The indexed, cancellable preview is unchanged. After confirmation, file revision
checks, link writes, recovery copies and the move run on a background worker.
A loading dialog remains visible until completion. Clean source editor locks are
retained across the worker; affected windows cannot navigate/save or start another
move during application. Dirty affected editors still block before application.
Writes are not cancellable mid-transaction; interrupted operations retain their
existing recovery journal and exact source preimages.

## Inline names and Trash feedback (#547)

Inline filename inputs own Space and editing keys; tree shortcuts do not run
while those inputs are focused. Submitting the current rename destination simply
closes the row without moving a file. Clicking a tree row opens its note while
keeping keyboard focus in the tree, so F2 or Enter can immediately start rename.

Trash confirmation uses a trash-glyph action and plain Cancel. The compact Undo
notification uses an undo glyph with a keyboard-shortcut tooltip. It appears at
the bottom right and hides after eight
seconds or with its close button / Escape. Hiding it does not discard the session's
Trash history: Cmd-Z (Ctrl-Z on Linux) in the Reader/tree restores the latest
trashed item, with the same collision and identity checks. Text inputs retain their
normal text Undo. A failed restore remains available to retry.

## Create a missing linked note (#546)

Hovering a verified missing wikilink offers **Create note** on writable desktop
platforms. It opens the inline creation row with the default template and an
editable target path. A bare target uses the source note's folder; a target with
folders uses that vault-relative path. Aliases and heading fragments are not
filename text. Enter creates exclusively; Escape cancels. Ambiguous links,
missing headings within existing notes, attachments and unsafe paths never offer
this action. Ordinary clicks retain the existing missing-link behavior.

Successful creation immediately publishes the note identity to the local resolver
before watcher delivery. Returning to the source note shows the normal resolved
link (a heading still has to exist). Template originals and source links are never
rewritten. The watcher continues to maintain search and backlinks.

## Transient operation feedback (#576)

Create, rename/move, copy, disk reload and recovery success messages use the same
bottom-right notification overlay as Trash; the document and sidebar never reflow.
Ordinary feedback disappears after four seconds; notifications with Undo last eight
seconds. The close glyph or Escape dismisses notifications. Errors remain visible
until dismissed. Trash keyboard Undo remains available after its notification closes.
Move feedback names the destination folder (rename names the new file); link counts
appear only when at least one link was updated. Ambiguous destinations use persistent
choice notifications (also over expanded tables). Recovery offers use four-second
toasts only for actual newer drafts; no-draft startup is silent. History actions
float over the preview, with failures in the same notification overlay. Header save
status, loading progress, inline filename validation and explicit confirmation dialogs
are controls, not notification strips. Frozen Brain workspaces retain their separate
operation review forms; this Reader change does not remove their approval controls.

## Graceful process termination (#590)

SIGTERM/SIGINT use the native application quit path on Linux and macOS. Signal
handlers only set an atomic flag; the UI thread initiates shutdown, flushes every
editor (even if another note conflicts), and runs the usual state persistence hooks.
OS quit/logout and updater relaunch use the same quit observers. Both historical
launch-marker directories are cleared on deliberate shutdown when all drafts are
protected; a failed draft write or an in-flight move retains the safety marker.
Unresolved conflicts retain their durable drafts and are still discovered per note. SIGKILL, panic and
power loss leave their markers for conservative startup; no-draft recovery is silent.

## Missing-link creation placement (#591)

Create note in a missing-link hover places the focused inline name beneath the
nearest existing target folder, expanding/revealing its branch. The input contains
only the name (or remaining not-yet-created directory suffix), not the redundant
vault-relative parent path. Standard single-line input scrolling keeps long names
editable. Create-only validation still checks the complete authored destination.
Creation/template errors use readable bottom overlay notifications, leaving the
input available for correction without an error row constrained by sidebar width.
The hover header has a nonshrinking height; long fallback messages scroll inside
the bounded card rather than compressing its title into the border.

## Confirmation and toast lifecycle (#605)

A move preview with no link changes confirms with “Rename” or “Move to <folder>”,
without zero link/note counts. Trash preparation, application and the result share
one notification identity: progress is replaced by Undo or an error, and is removed
while a confirmation dialog is open or after cancellation. Undo has an explicit
eight-second lifetime, independent of the toolkit default.
While notifications are present, Reader, source and historical previews gain
additional scrollable end space (65% of the window height). The viewport/layout
stays fixed, and the last lines can scroll above the overlay. When the notification
list empties, the normal end space returns.

## Folder rename and tree keys (#578)

F2 or Enter opens inline rename for a Markdown note or a real folder. A folder's
name is not suffixed with `.md`; submitting the same path quietly ends rename.
Right/Left expand/collapse, with Left selecting the parent from a child. Clicking a
folder label selects it; its disclosure arrow or a double-click toggles it.
⌘↓ on macOS / Ctrl↓ on Linux opens the selected item or toggles a folder.

Folder moves use the existing background link preview and explicit update/without
update choice. All descendant identities are mapped together, including relative
links within moved notes and links to moved assets. Only link-target bytes change;
non-UTF-8 notes are listed as skipped and moved intact. Dirty descendant editors,
changed previews, existing destinations and orphaned destination drafts block the
move. One atomic non-replacing directory rename moves the subtree; durable
preimages support Recover link moves after partial link updates. Recovery refuses
later external changes, including changes to binary assets.

## Inline rename and Undo (#614)

Enter in the tree commits ordinary note/folder renames directly. The field shows
only the name, preserving the current parent; a typed folder/name chooses another
vault-relative destination. Link discovery and application run in the background.
The result replaces progress with one eight-second bottom toast and a glyph Undo.
Undo uses that operation's retained preimages and refuses newer edits or another
vault. Previous operations remain available through Recover link moves.

Only changes affecting more than twenty notes, or containing unresolved links or
unreadable sources, open the compact confirmation sheet. It shows five affected
note names, an expandable remainder, and the unchanged entries. Cancel writes
nothing; commit revalidates the preview. Name collisions, dirty editors and stale
previews remain under the inline field. The existing revision checks, locks,
non-overwrite writes and journals are unchanged.

## Folder destinations (#625)

Drag a note or folder onto a tree folder; the destination is highlighted. The
Folders header is a drop target for the vault root. The document and tree menus
also offer **Move to…**, with a searchable folder list and arrow/Enter/Esc control.
The picker uses the loaded inventory, excludes the current parent and the source
subtree, and checks the destination again before starting the shared revision-aware
move. It never interprets a typed query as a new filesystem path.

Both entry points preserve the source basename, use the existing link preview and
safe application, and finish with the same bottom **Moved to…** toast and Undo.
Large or ambiguous changes still require the existing risk sheet. Collisions,
foreign-vault drops, active operations and unsaved source editors cannot bypass
the move guards. Attachments can travel inside a moved folder; these commands do
not introduce standalone attachment moves.

## Parent-directory identity (#699)

An open FileEditor pins the real parent directory and records every canonical
ancestor identity. Reads, temporary-file creation, atomic exchange, rollback and
preimage archival use directory-relative handles. A parent/ancestor rename or
replacement makes Save, reload and conflict resolution refuse the stale path;
the durable draft is retained. Restoring the original directory allows recovery.

A directory swap racing after the final identity check cannot redirect writes or
archive a sibling's matching file. The exchange can complete only in the pinned
original directory; history retains its displaced inode, and the stale visible
path is reported as an error without acknowledging the draft as saved. This also
applies when Undo submits previous bytes through an already-open FileEditor.
Callers reopening an editor for a later Undo must still validate their own stored
path/revision identity before opening; FileEditor cannot infer a past session's
intended path from a new open.

## Windows file ownership and failed moves (#820)

After native publication, the checked replacement must also retain the displaced
source's exact DACL ACE bytes/order and protected/unprotected inheritance policy
(#845). `ReplaceFileW` can insert an explicit Administrators grant when ownership
falls back to the current user. Okilum compares the published DACL, restores only
that DACL through a checked native handle when needed, and verifies it before
acknowledging the save. Owner/group fallback remains unchanged. A raced published
inode is never a permission-repair target; an unsuccessful repair retains the
draft and native preimage with an explicit incomplete-save error.

Prepared save files first retain source security. If Windows rejects assigning
that owner with ERROR_INVALID_OWNER (for example Administrators under a
non-elevated user), creation retries with the current token's default owner/group
and the original DACL, ACE order and inheritance protection. The ACL and encryption
are applied before any proposed bytes are written. Other preparation errors still
fail closed; source attributes, durable history and revision checks are unchanged.
The fallback verifies effective read/write access before writing bytes. If an
OWNER RIGHTS ACE would remove that access under the new owner, the source stays
unchanged rather than publishing an inaccessible replacement.

On an ordinary failed link move, generated link text is removed from editor drafts.
Already published link changes are rolled back through revision-aware saves when
still equal to the operation's after bytes. A later external edit or a blocked
rollback is retained and reported for explicit recovery. Multi-file moves are not
a filesystem transaction: crashes and racing writers still require the durable
link-move journal. An ordinary denied rename with no concurrent changes leaves
original paths, links and clean drafts, and can be retried.

Reader Quit attempts every save, then permits exit when all latest drafts are
durable even if file access failed. Revision conflicts retain the existing
explicit Reload/Keep mine decision before app-owned Quit. A failed draft write or an in-flight move
continues to block this explicit Quit action. This matches recovery-aware OS close
and updater exit behavior; canonical save errors keep their friendly access notice
and detailed diagnostics remain in the application log.

## Code quote comforts (#1992)

Fenced Go snippets now pair raw-string backticks, wrap selected text (including
multiple lines) and type over their closing backtick through the existing Input
history in Source and Live Preview. A backslash is literal inside a Go raw
string; it does not escape the closing delimiter. JS/TS template-string escapes
keep their existing behavior, and shell backticks remain literal command syntax.
The existing bracket-pair preference, IME guards and one-step Undo/Redo apply.

Tab on a fenced-code selection also indents its empty lines, including an empty
last body line before the closing fence. Shift+Tab removes that indentation;
selection mapping, CRLF and one-step Undo/Redo use the existing Input transaction.
A line at the exclusive selection end, the closing fence and following prose
are not indented. This matches the embedded Input's selected-line indentation.

The inferred code indentation unit stays with the active fence while its body
is edited. Repeated Tab/Shift+Tab, Enter, typing and Undo/Redo do not re-detect
width from indentation they just inserted: a two-space line returns to its
original two spaces after Tab then Shift+Tab. Another fence/document or changed
fence structure detects its own policy. Adjacent differently-indented snippets
therefore keep independent units; this session state is not persisted.

Python Enter checks colon context with the embedded component's grammar:
colons inside comments, strings and f-string format specifiers keep the current
line indentation, while code colons (including ordinary if/def headers) still
open a level. Parsing is limited to 64 KiB snippets and a 20 ms budget, only
when Enter follows a Python colon. Unavailable/uncertain syntax keeps the
current indentation. Source and Live Preview use the same source transaction
and preserve authored LF/CRLF and one-step Undo/Redo.

## Explicit source hints and derived views

Identical canonical bytes can skip graph/search updates and checkpoint persistence.
They do not imply that each Reader already owns complete derived views: after a
Reader is rebound, Tasks, Projects or Now can need their per-path state restored.
The incremental worker therefore reconciles explicitly hinted paths from its
verified snapshot into those three indexes even when `batch.changed` is empty.
This work remains proportional to the hinted paths; it neither rewrites the
source bank nor creates another search generation.

The existing publication notifies the Reader, and its cached side panels observe
that notification. Task completion also notifies on a stale-owner rejection.
Now's prepared labels notify their owner on completion; Board metadata and project
records have their own notification paths. A global drawing refresh cannot repair
missing derived data and is not used for this reconciliation.

The unmodified held-snooze/rebound test exercises the actual calendar selection,
worker cancellation and current-owner retry. Independent regression controls also
retire Tasks, Projects and Now in turn, reconcile unchanged source, and require
restoration without changing source bytes or the committed search generation.
Their setup first requires a fully initialized incremental baseline and populated
views, so an unfired path cannot pass as a successful reconciliation.
