# Resolve Sync conflicts inside a note (#2044)

On Linux, a selected note's shallow parent watcher checks for both Syncthing
`<stem>.sync-conflict-<date>-<time>-<device-prefix>.<extension>` and editor
`<stem>.okilum-conflict-<date>-<time>-<random>.<extension>` copies. Creating or
removing a sibling copy updates the note banner independently of the search
index. Review opens the first copy directly; Refresh conflicts exposes the full
inventory. Settings → Sync lists copies in connected and currently open vaults,
with pagination and explicit incomplete-inventory/error status.

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
under unique `.okilum-save-*` names; ordinary Sync ignore policy excludes these.
Publication uses exclusive staging and NOREPLACE, followed by directory fsync.
The conflict-named extra copy disappears only as part of the explicit decision.
A separate fsynced `.complete` marker acknowledges successful publication. A
`.recovered` marker acknowledges explicit repair of an interrupted operation,
so later recovery cannot resurrect an intentionally deleted note.

Both preimages appear under the canonical note in Note history and in Recover
notes, including if the canonical file is missing. They stay protected and are
available for #2045 to adopt; this change never expires conflict records or
retained originals. History recovery uses its existing exact-byte preview and
exclusive Save as recovered note behavior.

A failed/partial decision reports NeedsAttention. After a process stop, choose
Recover interrupted resolutions in Sync conflicts. It recreates only missing
canonical/copy names from durable preimages, never overwrites present versions,
and is safe to repeat. A successfully published canonical decision remains in
place; the restored conflict copy requires another explicit review. Old records
without directory identity, replaced ancestors and unreadable records remain
visible recovery evidence, without path-recreation authority. No idle recovery
or retention worker silently resolves a conflict.

Acceptance lives in `conflicts::resolution_tests`, the mounted GPUI
`issue2044_` tests, and the checksum-pinned, reaped two-runtime
`managed_real_conflicts_resolve_each_choice_with_hashes` fixture. The crash test
terminates a subprocess after the journal, each capture and each publication,
then checks exact recovery and preservation of intervening writers. The runtime
fixture creates real concurrent offline edits, checks both preimages by SHA-256,
resolves each way, and checks convergence on both runtimes.
