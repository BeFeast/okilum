# Deep links (#1049) — design

A link clicked anywhere (T3 Code, a browser, Inbox web, chat, another note)
opens Okilum at the exact place: vault, note, line and column, or heading or
block. This document fixes the grammar and the rules before any code.

## Grammar

One scheme, `okilum:`. Every external link has an *address* and optional
*position*:

```
okilum://v/<vault>/<path>[?line=<n>[&column=<n>]][&page=<n>][&id=<vault-id>][#<heading> | #^<block>]
okilum://file/<absolute path>[?line=<n>[&column=<n>]][&page=<n>][#…]
okilum://note/<note-id>      (reserved, see «Stable ids»)
okilum://task/<task-id>      (reserved)
okilum://project/<project-id> (reserved, projects of the server model)
```

- `v/<vault>/<path>`: the canonical form copied by full-vault Readers.
  `<vault>` is the vault's folder name on the machine that made the link and
  `<path>` is vault-relative, with its extension. Example:
  `okilum://v/Notes/Projects/Launch%20plan.md?line=12&column=4`.
- `id=<vault-id>` (#1414): the vault's own id. The same synced vault can have
  another folder name on another machine (`Obsidian Vault` on three, `Brain`
  on idunn), so the name alone does not find it there. «Copy Okilum link»
  adds the id in full-vault scope; a link without one (every link made before) is looked up by
  folder name, as before. An id that is not 8–64 letters, digits and `-` is
  ignored. Older Okilum versions ignore the parameter and use the name.
- `file/<absolute path>`: for tools that know a file but not a vault
  (T3 Code, editors, terminals), and for Copy in a single-file quick viewer.
  The path is absolute, `/`-separated; on
  Windows it is `file/C:/Users/…` (`C:\…` and the verbatim `\\?\C:\…` form
  are read too: they are the same file). Okilum finds the vault that contains
  it; a file in no known vault opens on its own (see Resolution).
- `line`, `column`: 1-based. Column counts Unicode characters (scalar values),
  not bytes or UTF-16 units, so Hebrew and Cyrillic lines behave like
  Latin ones. Out of range is clamped to the last line or column.
- `page`: PDFs and other paged files (1-based).
- Fragment: `#Heading text` like Obsidian (a heading in the note), or
  `#^block-id` (a block reference). A fragment and `line` together: `line` wins.
- Encoding: RFC 3986. Each path segment is percent-encoded UTF-8; `/` is the
  separator and is never encoded inside the path; spaces are `%20`, never `+`.
  Parsers also accept unencoded non-ASCII (Hebrew, Cyrillic) as pasted by
  chat apps, and NFC-normalize before comparing with file names.
- Unknown query parameters are ignored, so links stay forward compatible.
  Unknown hosts are rejected (see Security).

### Internal URLs stay internal

Rendered notes already use `okilum://open/…`, `okilum://attachment/…`,
`okilum://footnote/…`, `okilum://footnote-back/…` and `okilum://outside-file/…`
inside the Reader. They are resolved against the window's current vault and are
never valid as external links. The external handler rejects these hosts with
«This link only works inside a note». «Copy link» never copies them: a copied
rendered link uses `v/` in a full vault and absolute `file/` in a quick viewer.

## Resolution

1. **Vault.**
   - `v/<vault>` looks at the known vaults: open windows first, then reading
     history. Quick-viewer folders in history are not known vaults; only an
     explicit full-vault open authorizes vault behavior. With an `id`, the
     vault that carries that id is the one,
     whatever its folder is called here, even if a same-name decoy carries a
     different id. Without an `id`, the folder name decides (exact after NFC,
     and case-insensitive on macOS and Windows). If no known vault carries a
     supplied id, an idless folder of that name (never linked from, or not
     synced yet) may still match. However, any known same-name folder carrying
     a different id blocks that fallback for all namesakes, including idless
     ones: Okilum refuses explicitly rather than concealing the conflict by
     selecting another folder. Resolution never rebinds an id or writes metadata.
   - A vault's id is a UUID in `.okilum/vault.json` (`"id"`), the file that
     already holds its display name and travels with it (Syncthing). The first
     «Copy Okilum link» from a vault open in a window writes it, if the vault
     is writable; a single-file view never creates this file, and reading a
     link never writes. (The Syncthing folder id is not
     used: it lives in Syncthing's own configuration, not in the vault, and not
     every vault is a Syncthing folder.)
     Copy creates an id only when metadata is absent or is a readable JSON
     object; it preserves existing fields and reuses any valid id found on
     its latest read. Broken, non-object or unreadable metadata is left alone;
     read/write failure leaves Copy using the folder-only link. The local
     read and atomic rename are not compare-and-swap across processes or
     machines. Independently copying before metadata sync can create conflicting
     ids. Decision 26 (19 October, 08:02 Israel; recorded in the 18 October ops
     decisions) accepts explicit refusal for that case: links carrying the
     losing id do not work until a separate recovery/merge-id decision. There
     are no aliases, recovery or id merging in #1414.
   - One match: use it.
   - Identity conflict: «Vault “X” has a different vault ID from this link.
     Okilum will not select a vault or change its identity. Using this link
     requires separate recovery.» No vault, chooser or consent is opened.
   - None with a supplied id and no conflicting namesake: «Cannot identify
     vault “X” using this link's vault ID. No matching ID or safe folder-name
     fallback was found on this computer.»
   - None without an id (legacy link): «Vault “X” is not on this computer.»
     with Open folder….
   - Several (two copies of one vault, or two vaults named alike): a chooser
     listing the full paths. It never guesses (AGENTS: ambiguity is
     surfaced). A vault without the note is listed but cannot be chosen;
     choosing one counts as the first-use consent below. A newer link replaces
     an open chooser.
   - `file/<path>`: canonicalize (on Windows to the verbatim `\\?\` form the
     known roots have), then the known vault whose root contains it, with the
     deepest root winning. Canonical POSIX paths retain their case, including
     on case-sensitive macOS volumes; Windows path spelling and prefix variants
     still match. POSIX containment compares native path components without
     lossy UTF-8 conversion; an unrepresentable vault-relative note path is
     refused explicitly. If no known vault contains it, the file opens on its
     own as the single-file view, after the first-use confirmation below. A
     file that is not there is said: «This file does not exist on this
     computer.»
2. **Note.**
   - Exact vault-relative path. If it has no extension and only `<path>.md`
     exists, that file opens. Otherwise nothing is guessed.
   - Missing file: «“Projects/Plan.md” is not in vault “Notes”.» The link
     never creates a note.
   - Paths with `..` or a leading `/` after the vault, or paths that resolve
     outside the vault through symlinks, are rejected.
3. **Position.**
   - In Reader: scroll to the block that contains `line` and flash it once.
     The column is ignored.
   - In Edit (Live Preview or Source): caret at `line:column`, scrolled into
     view, also when the note has unsaved edits: the caret moves without asking,
     since nothing is lost. The note's current mode is kept; a link never
     switches modes.
   - `#heading` and `#^block` use the existing document-link landing, the same
     as a wikilink.
   - `page` uses the PDF viewer's page.
4. **Window.** A vault already open in a window is reused and that window is
   raised. A standalone file link reuses the quick Reader for that exact
   canonical file, including while it is loading; a different file gets its
   own quick Reader. Otherwise the link opens a window as Open folder… does.
   Accepted consent closes once the requested Reader window exists, without
   waiting for a quick folder to become a vault.

## Delivery and single instance

- macOS: `CFBundleURLTypes` (`okilum`) in the bundle `Info.plist`. GPUI's
  `on_open_urls` is already wired; `reader_open::dispatch_urls` gains the
  `okilum:` branch next to `file:` URLs.
- Linux: the `.desktop` file adds `x-scheme-handler/okilum` to `MimeType` and
  `Exec=okilum %u`. The package refreshes the desktop database.
  `xdg-mime default` is the user's choice, documented in
  `docs/linux-releases.md`.
- Windows (installed build only): Velopack install and update hooks write
  `HKCU\Software\Classes\okilum` (`URL Protocol`, command
  `"…\okilum.exe" "%1"`). The uninstaller removes it (#974).
- Windows portable: no registration. Portable leaves no traces (#1037); its
  About says «Okilum links open in the installed version.»
- Every OS: a URL argument (`okilum okilum://…`) is accepted. A second process
  forwards it over the existing single-instance endpoint (`reader_instance`):
  the `Request` gains `link: Option<String>`. The running instance resolves
  it, so a click with Okilum already running never starts a second app.

## Web form

`https://okilum.app/open#<address>` (the same under `inbox.okilum.app`), e.g.
`https://okilum.app/open#v/Notes/Projects/Plan.md?line=12`.

- The address is in the **fragment**, so it never reaches the server or its
  logs. The page is static and has no analytics.
- The page shows the decoded vault and path, sets `location` to
  `okilum://<address>` on load, and keeps two buttons: «Open in Okilum» (the
  same link) and «Get Okilum» (download page), for when nothing handled the
  scheme.
- Universal Links (macOS associated domains) and Windows app URI handlers are
  a later step; the page works without them.
- Chat apps that do not linkify `okilum:` get the https form from «Copy link»
  with ⌥ / Shift held (exact control in the implementation PR).

## Security

- **Links only navigate.** They open a vault, a note and a position. They never
  create, write, rename, delete, run a command, change settings, start search
  indexing in a new folder, or connect sync or Inbox.
- **First use of a vault from a link needs confirmation**: «A link asks to
  open vault “Notes”.» with its full path, Open and Cancel. Open is
  remembered per vault (`link_trusted_vaults` in `reader-ui.json`), so only
  the first link to a vault asks. A vault already open in a window opens
  without asking: the link only moves within what is on screen. The same
  confirmation applies to a file outside every known vault: «A link asks to
  open the file “X”, which is not in a vault.» with its full path; Open is
  remembered per folder (the file's), so only the first link into a folder
  asks.
- Rejected outright, with a message: unknown hosts, internal hosts, relative
  or `..` paths, NUL or control characters, links over 4096 bytes, and
  non-`okilum` schemes passed as the argument.
- No network fetch for any link. The web page only redirects.
- Repeated identical links (link spam or double delivery) reuse the window and
  do not stack dialogs: one prompt at a time per vault or outside-file folder.
  A newer link for that same key replaces both the pending open target and
  the displayed target, full path and vault/file consent wording.

## Copy link

- Note ⋯ menu: «Copy Okilum link» (no position). Full-vault Readers use
  `v/` with the vault id when available; single-file quick viewers use absolute
  `file/` without reading or creating vault metadata. Heading, caret, and file
  menu Copy actions use the same Reader scope.
- Editor context menu: «Copy link to line» (`line` and `column` of the caret).
- File tree and file menus: «Copy Okilum link» for notes and files.
- Heading context in the outline: «Copy link to heading» (`#Heading`).
- Clipboard is plain text; the toast says «Link copied». If the quick viewer's
  full absolute path cannot be represented losslessly as UTF-8 (including a
  UTF-8 symlink alias that canonicalizes to a raw-byte POSIX parent), Copy
  refuses with an explicit error, preserves the clipboard, and shows no success
  toast. It never substitutes replacement characters or a vault link.

## Stable ids

`note/<id>`, `task/<id>` and `project/<id>` (projects of the server model) are
reserved now so the grammar does not change later. None of them has stable ids
yet (server concepts plan). Until they do, these links answer «This link needs
a newer Okilum» and do nothing.

## Slices

1. This document.
2. Parser and resolver in `okilum-core` (pure, table-tested: grammar,
   encoding, Hebrew paths, internal-host rejection, `..`, ambiguity), the
   `okilum <url>` argument with single-instance forwarding, the Linux
   `.desktop` registration and the Copy link items. Native check on CT141
   with `xdg-open okilum://…`, app closed and running.
3. macOS `Info.plist`, Windows Velopack hooks and uninstaller removal, the
   portable About line. Mac check on Hedva; Windows by hosted CI and an
   artifact registry check.
4. The `okilum.app/open` page (site repository), linked from Copy link with
   a modifier.

## Decisions (Oleg, 18.10, #1414)

- `file/` outside every known vault is implemented as the doc said (not
  dropped from the doc): the file opens as the single-file view, after the
  first-use confirmation, remembered per folder.
- A vault's identity in links is an id stored in the vault (`.okilum/vault.json`),
  with the folder name as the fallback; old links keep working.

## Accepted identity boundary (#1414, decision 26, 19.10)

Concurrent first Copy on machines whose metadata has not synced can create
different ids. Explicit mismatch refusal is the accepted current-slice limitation;
the product decision is resolved. Losing-id links remain unusable pending a
separate recovery/merge-id decision. This work is not part of the already delivered
LinuxStable14238.

Acceptance remains to be verified after resource GO:

- A losing-id request shows the explicit error and selects no vault; it opens
  neither chooser nor consent and does not navigate or rebind an existing Reader.
  Metadata bytes (including name, id and unknown nested fields) and directory
  entries stay unchanged; no temporary metadata file is left behind.
- A matching id opens the actual root under a different folder name despite a
  conflicting namesake, with the requested note and landing. Legacy folder links
  and unconflicted idless fallback remain supported; conflicting/idless namesakes
  refuse visibly. Resolution never writes metadata.
- Regression tests, meaningful mutation controls and restored positive controls,
  required CI and one independent review remain required. No runtime gate is
  claimed by this document. All changes after tested checkpoint `4525f4bf` remain
  uncompiled/untested until an authorized run.
- The retained pre-merge data-risk exception requires native checks on **two OS**:
  first Copy durably adds/reuses the synced id while preserving other metadata
  fields, write failures leave metadata intact and use a folder-only link, and
  reopen retains the identity. Check legacy links and differing folder names too.
  The accepted limitation does not waive these checks or grant a shared slot.

## Decisions (Oleg, 10.10)

- Vault is identified by **folder name**; several known vaults with that name
  show a chooser. (Since #1414 the vault's id comes first when the link has
  one; the folder name is the fallback.)
- A link to a note with unsaved edits moves the caret **without asking**.
- `okilum://project/<id>` is reserved next to `note/` and `task/`.
