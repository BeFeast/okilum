# Deep links (#1049) — design

A link clicked anywhere (T3 Code, a browser, Inbox web, chat, another note)
opens Okilum at the exact place: vault, note, line and column, or heading or
block. This document fixes the grammar and the rules before any code.

## Grammar

One scheme, `okilum:`. Every external link has an *address* and optional
*position*:

```
okilum://v/<vault>/<path>[?vault_id=<uuid>][&line=<n>[&column=<n>]][&page=<n>][#<heading> | #^<block>]
okilum://file/<absolute path>[?line=<n>[&column=<n>]][&page=<n>][#…]
okilum://note/<note-id>      (reserved, see «Stable ids»)
okilum://task/<task-id>      (reserved)
okilum://project/<project-id> (reserved, projects of the server model)
```

- `v/<vault>/<path>`: the vault form, which «Copy … link» produces in a
  vault window. `<vault>` is the vault's folder name on the copying machine
  and `<path>` is vault-relative, with its extension. Example:
  `okilum://v/Notes/Projects/Launch%20plan.md?vault_id=0f8fad5b-d9cb-469f-a165-70867728950e&line=12&column=4`.
- `vault_id` (#1414): the vault's portable identity, so a link opens the same
  synced vault where its folder has another name (one machine's «Obsidian
  Vault» is another's «Brain»). It is a UUID kept in
  `.okilum/link-identity.json`, which syncs with the vault; see «Vault
  identity». Okilum versions without it ignore the parameter and use the
  folder name. A `vault_id` that is not one UUID, appears twice or is on a
  non-`v/` link makes the link incomplete.
- `file/<absolute path>`: for tools that know a file but not a vault
  (T3 Code, editors, terminals), and what «Copy … link» produces in a
  single-file view. The path is absolute, `/`-separated; on Windows it is
  `file/C:/Users/…` (an encoded `\`, `C:%5CUsers…`, is also accepted, and so
  is the verbatim `\\?\C:\…` / `\\?\UNC\server\share\…` form, which means
  its plain spelling). Okilum finds the vault that contains it, otherwise
  the file opens on its own (see «Resolution»). Drive-relative forms
  (`C:relative/note.md`, a bare `C:`) are not absolute and are refused as
  incomplete on every platform: the parser is plain string parsing, not
  host path logic.
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
rendered link is rewritten to the `v/` form.

## Resolution

1. **Vault.**
   - Known vaults are the vaults in open windows first, then reading
     history. A folder only ever shown in a single-file view is not a vault
     and is not known (#1439).
   - `v/<vault>?vault_id=<id>`: exactly the one known vault whose
     `.okilum/link-identity.json` has this id, whatever its folder is called.
     The folder name is **never** a fallback for a link with an id: when no
     known vault has the id, the link is refused («This link belongs to a
     vault “X” with a different identity, so no vault was opened.» when a
     namesake exists, otherwise «Vault “X” is not on this computer.»), and
     several vaults with one id are refused too. A refusal selects no vault,
     asks nothing, moves no window and writes no metadata.
   - `v/<vault>` without an id matches the folder name of the known vaults.
     The comparison is exact after NFC and is case-insensitive on macOS and
     Windows. Links copied before #1414 keep working this way.
   - One match: use it.
   - None: «Vault “X” is not on this computer.» with Open folder….
   - Several: a chooser listing the full paths. It never guesses (AGENTS:
     ambiguity is surfaced). A vault without the note is listed but cannot be
     chosen; choosing one counts as the first-use consent below. A newer link
     replaces an open chooser.
   - `file/<path>`: canonicalize, then the known vault whose root contains it,
     with the deepest root winning. If no known vault contains it, the file
     opens on its own as a single-file view, after the first-use confirmation
     below for its folder; the folder does not become a vault. A missing file
     or a path that is not absolute on this computer is refused.
2. **Note.**
   - Exact vault-relative path. If it does not exist, the path does not
     already end with `.md` (in any case) and `<path>.md` exists, that file
     opens; this includes dotted names such as `v1.2` or
     `Daily/2026.10.29 Meeting`. The exact name always wins, `.md` is never
     appended twice, and with several vaults of the same name each choice
     applies the same rule. Otherwise nothing is guessed.
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
   raised. Otherwise the link opens a window as Open folder… does.

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
  without asking: the link only moves within what is on screen. A file
  outside every known vault asks the same once for its folder («A link asks
  to open a file in folder “Outside”.»), and Open opens the file on its own.
- Rejected outright, with a message: unknown hosts, internal hosts, relative
  or `..` paths, NUL or control characters (raw, or percent-encoded in the
  vault, a path segment, the file path, an id, a heading or a block; any
  `char::is_control` such as NUL, LF, CR, ESC, DEL or C1), links over 4096
  bytes, and
  non-`okilum` schemes passed as the argument.
- No network fetch for any link. The web page only redirects.
- Repeated identical links (link spam or double delivery) reuse the window and
  do not stack dialogs: one prompt at a time per vault.

## Copy link

- Note ⋯ menu: «Copy Okilum link» (no position).
- Editor context menu: «Copy link to line» (`line` and `column` of the caret).
- File tree and file menus: «Copy Okilum link» for notes and files.
- Heading context in the outline and the Read menu: «Copy link to heading»
  (`#Heading`).
- A link's menu: «Copy link address» (a note, heading, block or file of this
  window as an `okilum:` link; a web address as written).
- Every one of them makes its link the same way, by the window it is pressed
  in (#1414, #1439):
  - a vault window copies `v/<folder>/<path>?vault_id=<id>`;
  - a single-file view copies `file/<absolute path>`, so the link reopens
    that file on its own and never names its folder as a vault.
- The link is made when Copy is pressed, never while a menu is built: opening
  a menu writes nothing.
- A link must read back exactly. A path that is not Unicode, a name with a
  control character or a link over 4096 bytes is refused with «Link not
  copied: …»; nothing is replaced by an approximation that would point at
  another file.
- Clipboard is plain text; the toast says «Link copied». A refused or failed
  Copy leaves the clipboard and the vault as they were.

## Vault identity

- `.okilum/link-identity.json`: `{"version": 1, "id": "<uuid>"}`. It is
  separate from `.okilum/vault.json` (the display name), so a rename never
  touches it.
- Created only by the first Copy in a vault window, atomically and without
  replacing anything (a temporary file in `.okilum/`, then a no-clobber
  rename). Two Copies on one machine agree on one id. Reading, resolving and
  opening links never write it.
- Never repaired, migrated or rewritten: unknown fields from a newer Okilum
  survive; damaged JSON, a missing or invalid `id`, a directory in its place
  or an unwritable vault make Copy refuse with the reason and leave every
  byte as it was. A Copy that fails to write removes only the empty
  `.okilum/` folder it created itself.
- Accepted limit (decision 26, 2026-10-18): if two machines make their first
  Copy before the metadata syncs, two ids exist; after sync, links with the
  losing id are refused as above. Recovery, aliases and merging ids are a
  separate decision.

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

## Decisions (Oleg, 10.10)

- Vault is identified by **folder name**; several known vaults with that name
  show a chooser. Since #1414 a copied link also carries the vault's
  identity, which wins and has no folder-name fallback (decision 26,
  2026-10-18); links without one still use the folder name.
- A link to a note with unsaved edits moves the caret **without asking**.
- `okilum://project/<id>` is reserved next to `note/` and `task/`.
