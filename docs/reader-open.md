# Opening local Markdown (#327)

`okilum '/path/space/Note.md'` opens that exact document in a read-only Reader.
`okilum '/path/folder'` opens a local Reader folder. Existing `--vault ROOT
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
`OKILUM_VAULT` does not override that explicit managed intent. Existing managed
windows are not replaced by document delivery. This startup change needs its own
qualification and supersedes implicit-connect expectations in the old #317 matrix.

## Canonical files and cache

Reader defaults to `~/Library/Caches/okilum/reader/<root-hash>` on macOS and
`$XDG_CACHE_HOME/okilum/reader/<root-hash>` (or `~/.cache/...`) elsewhere.
The chosen cache must be outside the canonical root, including existing symlink
ancestors; otherwise supply an explicit external `--index-dir`. An explicit index
override is honored. Watcher bulk rebuilds retain the same index path.
Default caches retain three recently opened vaults (#513). Open Readers and their
workers hold shared cache leases across processes; if more than three vaults are
active, their caches remain until a later open can prune them. Eviction runs on
the background worker after Ready is sent; source refreshes do not run eviction.
Symlinks and unrelated directories are skipped. Only accepted opens update
`.usage/<hash>.opened` publication markers; older caches without markers are not
counted or evicted until next opened. Interrupted `.evicted-*` cleanup is retried
independently; an antivirus cleanup hold is logged and cannot stop other retention.
Durable history and recovery drafts are outside retention.
Explicit `--index-dir` remains an exact override and is excluded from managed LRU. It names
the index of the vault the instance is launched on; a vault opened later in the same instance
(Open folder, the first-run picker, an OS delivery) does not inherit it. An isolated instance (an
absolute `OKILUM_STATE_DIR`) keeps all such caches, and their LRU, under `<state>/cache` instead
of the user's cache (#1129).
The published vault owns its cache path immediately, so Retry/Rescan during
background preparation cannot write into the previous vault's cache. Returning
to a retained vault publishes its persisted tree, last note and search before
reconciliation. The diagnostic log records `vault_cache_lease` and
`cache_retention` events; failed cache maintenance cannot prevent reading.

Search generations inside one vault cache are bounded too (#572). A generation
family is one 64-hex name: `generations/<name>`, `generations/<name>.repairs/*`
and `attempts/<name>.<uuid>`. Every Reader or worker that opens, forks from or
publishes a family first takes a pin, an exclusively locked
`generation-pins/<name>.<uuid>` file held for the searcher's lifetime (or, for a
published checkpoint, until the persisted hint names it). After Ready and after
each checkpoint persists, the collector retires every family that is neither
pinned, in any process, nor named by `reader-startup.json`,
`reader-snapshot.json` or `reader-delta.json`. It reads the hints unlocked, then
takes the `generation-pins/gate` lock, which pin creation shares, and checks the
pins and that no hint file was replaced since. If any changed, the run is skipped.
Retirement renames each member into `retired-generations/<uuid>` (never a
generation in use, so Windows' complete-marker publication is unaffected), and
deletion happens afterwards outside the gate. An undecodable hint, a busy gate or
collector, or a failed rename or deletion keeps the files and is retried on the
next run. Interrupted cleanup is finished on the next run. A crashed
holder's unlocked pin is reclaimed. Legacy unnamed `attempts/<uuid>` staging is
never touched. Session forks are private copies and need no pin. The
diagnostic log records `search_generation_retention`.

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

Known create/delete/move/Undo mutations update the reconciled inventory, affected
link referrers and search rows incrementally (#545), like save. Watcher directory
notifications are scope hints: the worker checks their immediate children, reads
only changed notes, and discovers new or removed subtrees. Existing subtrees are
not walked for an ordinary parent hint; native dropped-event/MustScan signals
still run a complete reconcile. Unsafe or uncertain non-note topology retains
the background fallback, including case-only renames and topology batches of
50 or more entries. Known iCloud placeholders retain their warning without
reopening notes or triggering reconcile. An unchanged directory echo neither
rebuilds nor checkpoints search; an empty folder checkpoints inventory only.
Inventory and indexing filesystem work runs off the UI thread.
