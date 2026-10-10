# Deep links (#1049) — design

A link clicked anywhere (T3 Code, a browser, Inbox web, chat, another note)
opens Okilum at the exact place: vault, note, line and column, or heading or
block. This document fixes the grammar and the rules before any code.

## Grammar

One scheme, `okilum:`. Every external link has an *address* and optional
*position*:

```
okilum://v/<vault>/<path>[?line=<n>[&column=<n>]][&page=<n>][#<heading> | #^<block>]
okilum://file/<absolute path>[?line=<n>[&column=<n>]][&page=<n>][#…]
okilum://note/<note-id>      (reserved, see «Stable ids»)
okilum://task/<task-id>      (reserved)
okilum://project/<project-id> (reserved, projects of the server model)
```

- `v/<vault>/<path>`: the canonical form, which «Copy Okilum link» produces.
  `<vault>` is the vault's folder name and `<path>` is vault-relative, with
  its extension. It works on all three of Oleg's machines because the same
  synced vault has the same folder name there. Example:
  `okilum://v/Notes/Projects/Launch%20plan.md?line=12&column=4`.
- `file/<absolute path>`: for tools that know a file but not a vault
  (T3 Code, editors, terminals). The path is absolute, `/`-separated; on
  Windows it is `file/C:/Users/…`. Okilum finds the vault that contains it.
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
   - `v/<vault>` matches the folder name of the known vaults: open windows
     first, then reading history. The comparison is exact after NFC and is
     case-insensitive on macOS and Windows.
   - One match: use it.
   - None: «Vault “X” is not on this computer.» with Open folder….
   - Several: a chooser listing the full paths. It never guesses (AGENTS:
     ambiguity is surfaced).
   - `file/<path>`: canonicalize, then the known vault whose root contains it,
     with the deepest root winning. If no known vault contains it, the file
     opens on its own as today's single-file mode, after the first-use
     confirmation below.
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
  About says links need the installed version.
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
- **First use of a vault from a link needs confirmation** («Open vault
  “Notes” at …?»). The same applies to a file outside every known vault.
  Vaults already opened by the user open without asking.
- Rejected outright, with a message: unknown hosts, internal hosts, relative
  or `..` paths, NUL or control characters, links over 4096 bytes, and
  non-`okilum` schemes passed as the argument.
- No network fetch for any link. The web page only redirects.
- Repeated identical links (link spam or double delivery) reuse the window and
  do not stack dialogs: one prompt at a time per vault.

## Copy link

- Note ⋯ menu: «Copy Okilum link» (`v/` form, no position).
- Editor context menu: «Copy link to line» (`line` and `column` of the caret).
- File tree and file menus: «Copy Okilum link» for notes and files.
- Heading context in the outline: «Copy link to heading» (`#Heading`).
- Clipboard is plain text; the toast says «Link copied».

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
  show a chooser.
- A link to a note with unsaved edits moves the caret **without asking**.
- `okilum://project/<id>` is reserved next to `note/` and `task/`.
