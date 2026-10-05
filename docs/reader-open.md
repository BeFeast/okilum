# Opening local Markdown (#327)

`tessera '/path/space/Note.md'` opens that exact document in a read-only Reader.
`tessera '/path/folder'` opens a local Reader folder. Existing `--vault ROOT
--note REL`, `--query`, `--copy-source`, `--html` and explicit `--index-dir` remain
available. A positional target cannot be combined with `--note` or `--jump`;
contradictory Reader and explicit managed inputs are rejected.

Open file / Open folder buttons and File menu actions use the native picker.
Keyboard shortcuts are Cmd-O / Cmd-Shift-O on macOS and Ctrl-O / Ctrl-Shift-O
elsewhere. Picker cancellation does nothing. Missing, inaccessible, non-Markdown
and out-of-root explicit requests produce an error instead of selecting a namesake.

An explicit containing root wins, then a containing existing Reader root. Otherwise
a standalone file uses its nearest ancestor containing a `.obsidian` directory,
or its containing directory. The selected root is shown above the document.
The hint is optional; its contents are never read or changed. Folder selection
provides an explicit alternative. Document link semantics remain those in
[document-links.md](document-links.md).

Each picker or OS request opens a separate Reader window, preserving previous
Reader history/position and managed drafts/recovery. File URLs are parsed through
`url::Url::to_file_path`; remote hosts and query/fragment URLs are refused.

## Startup and managed workspaces

The application registers OS file-open delivery before launch and queues events
until initialization. Cold and warm events share the same validated dispatcher;
there is no timeout to guess whether an event will arrive.

On macOS, an ordinary no-argument launch retains the saved workspace identity but
**does not automatically connect**. Use its explicit Open/Retry action to connect
to that saved identity. This prevents late Finder delivery from first contacting a
Brain. Explicit `--brain-endpoint IP:PORT` still opens the managed client; inherited
`TESSERA_VAULT` does not override that explicit managed intent. Existing managed
windows are not replaced by document delivery. This startup change needs its own
qualification and supersedes implicit-connect expectations in the old #317 matrix.

## Canonical files and cache

Reader defaults to `~/Library/Caches/tessera/reader/<root-hash>` on macOS and
`$XDG_CACHE_HOME/tessera/reader/<root-hash>` (or `~/.cache/...`) elsewhere.
The chosen cache must be outside the canonical root, including existing symlink
ancestors; otherwise supply an explicit external `--index-dir`. An explicit index
override is honored. Watcher bulk rebuilds retain the same index path.
No Markdown, attachments or Obsidian settings are written. Synthetic tests compare
the complete canonical directory tree, not only existing note hashes.

## Qualification boundaries

Finder acceptance requires a future package with both the matching event handler
and Markdown document registration. Packaging/plist changes belong to #325; old
signed 261f65 artifacts do not include this feature. Native GUI acceptance is
pending a manager-controlled host lease; widget and CLI tests do not substitute
for Finder launch verification.

Windows packaging/runtime support is not introduced here. Existing `Vault::scan`
uses platform separators for keys while suffix/relative resolution assumes `/`;
that pre-existing normalization gap is tracked separately from #327.
