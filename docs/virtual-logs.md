# Virtual log source definitions (#1382)

The core source layer uses a versioned JSON file with extension `.okilog`.
JSON reuses the existing core serialization stack. The definition is canonical;
indexes, selection diagnostics and merged rows are rebuildable and never saved.
The desktop can load a saved definition, edit session criteria and copy the
resulting JSON explicitly. The saved definition is edited externally; session
changes do not rewrite it. No source log is modified.

```json
{
  "version": 1,
  "name": "Checkout rollout",
  "vault_root": true,
  "glob": "showcase/kubernetes/*",
  "query": "service=checkout-api",
  "rotation": null,
  "time": { "last_seconds": 3600 }
}
```

`vault_root` defaults to false (relative to the definition's folder). Glob syntax
is globset with literal path separators: `*` matches one segment and `**` traverses
folders. `logs/**` selects a folder's logs. Criteria are ANDed at **file** level;
a qualifying file contributes all its records, using the existing chronological
merge. A query and a time criterion need not match the same record. Time criteria
select files whose minimum/maximum record timestamps overlap the inclusive window;
mtime is never proof of membership. `now` is injected as Unix nanoseconds by the
caller, and a new plan freezes a relative window for a selection pass. Absolute
windows use `{"absolute":{"start":1792238400000000000,"end":1792242000000000000}}`.
Unknown fields, versions, invalid queries, reversed ranges and escaping globs fail
explicitly. The definition must exist inside the vault when compiling its plan.

`rotation` optionally names a base filename (`app.log` or glog's `prog.INFO`).
The existing filename detector and compressed reader determine supported forms.
Only log files participate; Markdown and txt are never candidates. File symlinks
inside the selection root are supported; canonical-path duplicates and outside-root
links are excluded with explicit reasons. Directory symlinks are not traversed.

## Bounded selection and diagnostics

`SelectionPlan::select` returns every enumerated log candidate with a `Reason`, the
opened files in merge ID order and the existing `MergedLog`. `Matched` means all
configured criteria passed. Nonmatches, unreadable files, missing timestamps,
partial windows, duplicates and limits are explicit. Progress reports attempted
file opens and input bytes, including failed opens. No content index is durable.

Limits per pass: 16,384 directory entries, 4,096 candidates, 128 selected files,
16 MiB input/decoded bytes per candidate, 64 MiB cumulative attempted input and
64 MiB cumulative successfully decoded data. The shared existing reader can decode
up to its own 64 MiB limit before rejecting a compressed candidate; this transient
work can exceed the remaining selection budget once, after which no further files
are opened. Reader memory leases remain in force. Enumeration truncation sets
`incomplete`; candidate-level bounds have explicit reasons. These limits are
constants rather than silently configurable global state.

## Watcher and desktop integration boundary

`VirtualLogWatcher::start(plan.root(), wake)` installs non-recursive watches before
selection, on up to 512 directories (and at most 16,384 enumerated entries).
Watch errors and exceeded scope fail explicitly. The callback wakes the owner's
executor; `drain()` supplies a bounded event batch. Reads are ignored. Mutations,
removals, backend rescan flags and a bounded-channel overflow are surfaced.
There is no periodic timer or idle scan in this module. On directory changes the
owner calls `refresh_directories()` before reselection, then drains events queued
during enumeration to close the discovery race. Backend overflow requires an
explicit rescan and visible incomplete coverage until that succeeds.

Logs → **Virtual logs…** loads a vault-scoped saved `.okilog` through Browse or
an explicit path. Opening a definition through the ordinary file route (including
Tree and Quick Open activation) embeds the same source form and result viewer
inside the document, preserving its filename header and navigation/history.
Opening only loads configuration; source selection/root watch still requires explicit Start.
Malformed definitions remain visible in this typed view, with their attempted
path available for retry, rather than falling through to a generic file card.
The form exposes name, glob/root, optional file-level query,
rotation family and all supported time modes (all, relative seconds, absolute
RFC3339 timestamps). Session edits are separate from the saved file; Copy
definition returns canonical supported JSON to the clipboard.

Start performs one bounded selection off the render path, reports progress and
per-candidate reasons, and hands the already-open immutable files to the existing
LogView. No path is silently reopened for rendering. The result keeps local
filters, selection, columns and merged export. Its source set and parser are
locked to that selection; ordinary file views retain their existing controls.
Errors and canceled/stale passes preserve the last successful result. A successful
explicit Rescan replaces it, including an intentionally empty selection.

Watch changes installs the existing watcher **before** selection and drains
queued events afterwards. Mutation/removal invalidates the result visibly and
retains its immutable history until an explicit Rescan. Lost/overflowed events
are an explicit incomplete-coverage error. This is an invalidation watch, not
automatic discovery/tail reconciliation; there is no polling scan. Stop/close
wakes an idle watcher immediately. A currently running backend selection pass
finishes within its existing bounds before cleanup; its result is discarded after
Stop. No immediate interruption inside a file read is claimed.

In-app canonical definition saving and automatic membership/rotation Follow
remain open. Full Follow must keep existing `LogTail`
sessions, consume reset segments and avoid duplicating a moved source. #1382
remains open for these capabilities and native acceptance; a usable desktop
snapshot flow does not establish them.

## Verification boundary

Offline tests use real temporary source files, the existing parser/query/merge and
gzip decoder, and mock notify events. They cover AND selection, exclusions, injected
clock windows, compressed rotations, copied-file reselection, preserved old immutable
snapshots and a positive event control before read silence. They do not prove the
OS watcher fires. Real watcher add/remove positive-control acceptance, mid-follow
rotation, desktop routing and measured 0% idle CPU remain NOTRUN. No live GUI,
daemon, remote build or production data is needed by these tests.
