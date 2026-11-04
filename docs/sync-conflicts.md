# Resolve Sync conflicts inside a note (#2044)

On Linux, a selected note's shallow parent watcher checks for both Syncthing
`<stem>.sync-conflict-<date>-<time>-<device-prefix>.<extension>` and editor
`<stem>.okilum-conflict-<date>-<time>-<random>.<extension>` copies. Extensionless
editor copies omit the final dot; legacy copies with that dot still map to the
extensionless note. Hidden notes, including Syncthing's marker-before-leading-dot
convention, participate in inventory, preview and resolution. A symlink used to
open the vault root is canonicalized consistently by inventory, preview and
recovery; descendant directory/file symlinks are still refused. The inventory
normalizes its root on a worker before enabling actions so the unsaved-editor
check uses the same identity as open Reader windows.

Creating or removing a sibling copy updates the note banner independently of the
search index. Review opens the first copy directly; Refresh conflicts exposes the
full inventory. Settings → Sync lists copies in connected and currently open
vaults, with pagination and explicit incomplete-inventory/error status.

The comparison shows exact source side by side with changed lines highlighted.
UTF-8 is preserved, including BOM and CRLF; non-UTF-8 inputs use hexadecimal
preview and are never silently decoded. Each pane shows filesystem modification
time in UTC. Syncthing's filename time/device prefix is labelled as filename
evidence, with its timezone unverified. An Okilum suffix is random, never a device
identity. An already running Sync controller may add a device name only when the
file's exact bytes match the authenticated Syncthing database. Missing or
ambiguous attribution remains visibly unknown; reviewing never starts a daemon.

Keep this leaves the canonical content; Keep that publishes the other content at
the canonical path; Keep both keeps the current content and creates a uniquely
named `-kept-<uuid>` sibling for the other version. Decisions refuse stale bytes,
replaced inodes/directories, symlinks, hardlinks, oversized sources, unsafe paths,
unsaved editors and durable drafts. Source reads, diff preparation and application
run on workers. A submitted decision cannot be replayed against unseen bytes.

Before moving either source, the controller exclusively creates and fsyncs a
schema-1 `sync-conflict-<uuid>.json` record in private durable application state
`sync-conflicts/`, outside the vault and its derived index. It contains the vault,
canonical/copy paths, both exact byte arrays, choice, creation time, directory
identity, file modes and retained-original paths. Both displaced inodes remain
under unique `.okilum-save-*` names. Publication uses exclusive staging and
NOREPLACE, followed by directory fsync. The conflict-named extra copy disappears
only as part of the explicit decision.

Managed enrollment seeds `.okilum-save-*` in `.stignore` on **each computer**.
Before a decision or interrupted repair creates any retained/staged file in a
Syncthing folder (`.stfolder` present), it verifies a bounded regular `.stignore`
with an effective unrooted `.okilum-save-*` or `**/.okilum-save-*` entry. A missing
entry, unreadable policy, or an earlier include/negation refuses the operation
without changing note files. Put the exclusion before includes/negations, then
review again. Okilum does not silently rewrite an existing custom policy.

After convergence, both computers have the chosen canonical text and, for Keep
both, the additional `-kept-<uuid>` note. The conflict-named copy is removed on both.
**Only the computer that made the decision** keeps the displaced `.okilum-save-*`
inodes and the private application-state journal containing both exact preimages.
The other computer does not receive these backups or journal through Sync; its
own previous-version retention, if configured, is independent. History and Recover
notes on the resolving computer expose both preimages even if the canonical file
is missing. These records never expire here and remain available for #2045
adoption. For an ordinary folder without `.stfolder`, retained files remain local
files; external sync tools need their own exclusion configuration.

A separate fsynced `.complete` marker acknowledges successful publication. Both
revision checks finish before journal creation, so a stale/refused preview cannot
become an interrupted operation. A journal persistence failure is marked terminal
before returning a refusal. If the completion acknowledgement fails after successful
publication, a fsynced `.refused` marker disables automatic path recreation while
retaining both versions in History; if that marker cannot be created, the journal
itself is exclusively renamed to `sync-conflict-<uuid>.refused.json`, also terminal.
The action reports NeedsAttention. A `.recovered` marker acknowledges repaired
operations so later deletions are never resurrected by those records. Marker and
fallback-rename failures remain explicit errors; no successful acknowledgement is
claimed when storage rejects it.

The primary Reader process runs interrupted-resolution recovery **once at startup,
under its instance lock, before requested-path validation, warm-cache reads,
indexing or managed Sync initialization**. A second launch forwards to that process
and cannot repair an operation still in progress. Terminal records are skipped
before decoding their source bytes or accessing their vaults, so completed decisions
do not warn about an intentionally deleted/disconnected vault. Recovery recreates only missing
canonical/copy names from durable preimages, never overwrites present versions,
and never resumes a choice. This also repairs a crash between moving the canonical
note away and publishing its replacement, without opening Settings or the note.
A global startup notice reports repairs and any failures, including when no vault
is open. Sync conflicts also retains its explicit Recover interrupted resolutions
action. An independently running Syncthing daemon may propagate the temporary
deletion before Okilum restarts; startup restores the note locally so the restored
file can propagate again.

A successfully published canonical decision remains in place during interrupted
repair; any restored conflict copy requires another explicit review. Old records
without directory identity, replaced ancestors and unreadable records remain
visible recovery evidence without path-recreation authority. No retention worker
silently resolves conflicts. History recovery keeps its existing exact-byte preview
and exclusive Save as recovered note behavior.

Acceptance lives in `conflicts::resolution_tests`, the mounted GPUI `issue2044_`
tests, and the checksum-pinned, reaped two-runtime
`managed_real_conflicts_resolve_each_choice_with_hashes` fixture. Crash controls
terminate a subprocess after the journal, each capture and each publication;
separate restart controls prove both missing-canonical gaps repair automatically.
Tests cover intervening writers, second-check refusal, failed completion markers
(including terminal-rename fallback), missing/overridden ignore policies,
extensionless/hidden notes, symlink-root recovery and dirty-editor refusal. The
runtime fixture creates real concurrent offline edits, checks all preimages by
SHA-256, resolves each way, checks convergence on both runtimes, and proves the
retained backups stay on the resolving computer after both runtimes scan.
