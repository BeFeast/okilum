# Reader startup diagnostics

Every ordinary desktop launch records phase timings, including successful opens.
The UI only queues events; a diagnostic thread appends JSON lines outside notes.
The current log rotates at 4 MiB and retains one previous file. Warning/failure
reports append without replacing startup timings. No note content is logged.

On macOS, send the latest launch timings with one Terminal command:

```sh
tail -n 250 "$HOME/Library/Application Support/com.befeast.okilum/reader-diagnostic.log"
```

On Linux the file is `$XDG_STATE_HOME/okilum/reader-diagnostic.log`, or
`~/.local/state/okilum/reader-diagnostic.log` when that variable is unset.
Windows retains `%LOCALAPPDATA%\okilum\reader-diagnostic.log`.

`elapsed_ms` uses the process startup clock; `details.duration_ms` measures a
single operation. `launch`, `build`, and `version` distinguish runs. The clock
does not measure LaunchServices before `main`, or final GPU presentation.

- `platform_application`, `app_run_callback`, `components_init`, `fonts_load`,
  `recovery_and_window_state`, `appearance_load`: before Reader creation.
- `startup_history_and_root_validation`, `window_key_and_geometry`,
  `native_window_open`, `reader_constructor`: history and window startup.
- `window_platform_create`, `window_view_build`, `window_first_draw`: the three
  steps inside `native_window_open`, in order. The first is the platform window
  and its renderer, which on macOS builds the Metal library from source
  (`runtime_shaders`); the second is the Reader and Root construction; the third is
  the first frame `open_window` draws before it returns (#1008).
- `requested_path_resolve`, `startup_snapshot_load`, `warm_cache`: cache size,
  presence, or rejection reason. A missing cache means the launch is cold.
- `history_and_primary_discovery`, `primary_selection`,
  `cached_inventory_construct`, `primary_source_and_render`,
  `saved_search_index_open`: first-document worker preparation.
- `first_worker_ready`, `first_event_received`, `text_state_stage`,
  `document_published`: event delivery and asynchronous document parsing.
- `first_ui_stage`, `inventory_ui_publish`, `ready_ui_publish`: time spent
  handling each publication on the UI thread, including synchronous cleanup.
- `ready_worker_send`, `ready_event_received`: distinguish event queue delay
  from the Ready callback and the subsequent publication-to-paint gap.
- `reader_first_paint`, `document_first_paint`, `inventory_first_paint`: actual
  Reader paint callbacks; the latter includes the usable document and inventory.
  The publication-to-paint gap includes layout, font shaping and paint work.
- `watcher_registration`, `reconcile_source_bank_load`, `reconcile_source_cache`, `replay_cursor_load`,
  `reconcile_phase`, `reconcile_stats`, `background_search_prepare`,
  `snapshot_persist`, `vault_ready`: background work after first publication.
- `cached_backlink_titles`: builds labels from already reconciled source text;
  it does not open linking notes a second time.
- `last_document_cache`: the existing history worker caches the latest opened
  source separately, so a late reconcile cannot replace it with the initial note.

A partial inventory persists a provisional cache with unreadable paths. Startup
loads only the small inventory/graph/primary manifest and latest-document cache. The complete source
bank is loaded after document publication and reconciled in the background.
Older caches migrate through the complete bank once. Full scans and incremental
watcher/save handling remain the scope of issue #487.

## Cloud-latency reproduction (#502)

The Linux-only probe injects latency into actual vault `open`, `read`, `stat`
and canonicalization calls. Cache files stay local. Positive controls perform
both metadata and source reads and assert the injected delay. Setup and cache
creation are excluded. Build and run from the repository root:

```sh
/usr/bin/cc -shared -fPIC -O2 -Wall -Wextra -Werror \
  scripts/probes/slow-vault-fs.c -ldl -o target/slow-vault-fs.so
cargo test --locked -p okilum-core --lib \
  warm_primary_five_thousand_links_cloud_profile --no-run
# Use the okilum_core executable printed by cargo, not cargo itself:
OKILUM_SLOW_FS_PREFIX=/tmp/okilum-cloud-link- \
OKILUM_SLOW_FS_MS=2 OKILUM_CLOUD_PROFILE_SAMPLES=3 \
LD_PRELOAD="$PWD/target/slow-vault-fs.so" \
  target/debug/deps/okilum_core-<hash> \
  warm_primary_five_thousand_links_cloud_profile \
  --ignored --nocapture --test-threads=1
```

The fixture contains 5001 notes (~35 MiB), with 5000 Markdown basename links
in the cached primary note. It measures `primary_source_and_render`, before
the First event, rather than native window/GPU presentation or a full reconcile.
On the same Linux host/session, 2 ms latency produced 21869.70/21854.14/21771.44 ms
before the fix (10000 metadata probes per sample), and 640.67/649.79/701.94 ms
after it (zero vault calls in that phase). Provisional identities defer unknown
occupancy and attachment verification to background reconciliation. Complete
resolution retains source-relative precedence and filesystem validation.
The counters and delays are active only around the timed render call, using
the `warm_primary` phase; setup, cache loading and later reconciliation are
outside that counted window. The separate `positive_control` phase proves
that both metadata and source-read instrumentation fired.

This establishes a first-publication delay. Native build 6453 diagnostics then
identified rejected caches and complete backlink resolution as separate gates,
described below.

To include startup cache loading, text preparation and the first GPUI test-renderer
draw, build the shell test executable and run the publication probe:

```sh
cargo test --locked -p okilum-shell warm_first_tree_frame_profile --no-run
OKILUM_SLOW_FS_PREFIX=/tmp/okilum-warm-frame- OKILUM_SLOW_FS_MS=2 \
OKILUM_WARM_PROFILE_PARAGRAPHS=350 OKILUM_WARM_PROFILE_LINKS=100 \
LD_PRELOAD="$PWD/target/slow-vault-fs.so" \
  target/debug/deps/okilum-<hash> warm_first_tree_frame_profile \
  --ignored --nocapture --test-threads=1
```

This 5001-note fixture (~54 MiB canonical sources) publishes the cached tree and
last note with 100 Markdown links in 139.90/150.24/184.14 ms on the same Linux
host. All samples assert usable parsed text/search input and a complete cached
tree while reconciliation is still held. Before-publication filesystem calls
are actually delayed: four stats and six canonicalizations per sample, with
zero vault source opens/reads. The preferences hold isolates the first frame
from the deterministic executor's serial background execution; it is not the
latency injection. The probe cancels that reconcile after measurement. Existing
warm UI regressions cover completion, input, selection and position retention.
These test-renderer numbers exclude native window/GPU presentation.
With `OKILUM_WARM_PROFILE_LINKS=5000`, three clean stress samples were
1848.39/1797.36/1926.43 ms, still before reconciliation. The extra time is text
preparation/layout, not per-link vault I/O; filesystem counts stayed identical.

## Native evidence and complete backlink reproduction

The M4/iCloud 5082-note logs from build 6453 show no usable warm cache on any
of three launches. The two later launches reject an existing 6.37 MB manifest
with `Invalid Reader startup inventory`; both also reject the source bank,
read every note, and rebuild the graph. A POSIX fixture reproduces that exact
manifest rejection with legal `:` and literal backslash filenames, which the
scanner admitted but the cache validator rejected. Native cache filenames were
not supplied, so the specific offending path is not established. Validation now
matches native path components and logs the rejected identity and cause chain;
parent traversal, absolute identities and Windows drive/ADS paths remain refused.

The two completed native runs spend 24.82/44.40 seconds in Preparing backlinks
and 5.71/5.70 seconds persisting the cache. The process sample places 14688 of
14955 worker samples under backlink Markdown resolution, `canonicalize`, and
iCloud `__getattrlist`. Graph construction now uses enumeration metadata for
source-relative occupancy and known in-vault absolute paths, preserving dangling
symlink and non-directory-parent shadows. Live UI actions still verify files.
Absolute paths outside known roots or across observed aliases/parent components
retain one verification per distinct destination to preserve outside-file/alias
identity; repeated occurrences share that result. They are not represented as
purely in-memory resolutions.

Run the complete-graph probe with the same test executable and preload:

```sh
OKILUM_SLOW_FS_PREFIX=/tmp/okilum-cloud-graph- \
OKILUM_SLOW_FS_MS=2 OKILUM_CLOUD_PROFILE_SAMPLES=2 \
LD_PRELOAD="$PWD/target/slow-vault-fs.so" \
  target/debug/deps/okilum_core-<hash> complete_backlinks_cloud_profile \
  --ignored --nocapture --test-threads=1
```

This fixture has 5001 notes and 10000 incoming references (absolute in-vault and
suffix links). On the same Linux host/session with 2 ms per filesystem call,
Preparing backlinks changed from 74953.46/74864.99 ms to 483.63/455.21 ms.
Before: 30000 canonicalizations and 5000 stats per sample; after: zero vault calls
in the timed `complete_graph` loop. Both runs have separate source-read and stat
positive controls. Enumeration, source loading and the one root normalization
are outside this counted phase; this is not a full-reconcile measurement.

JSON cache writes now use a 64 KiB buffer and propagate final flush errors.
The primary probe also compares identical 50.91 MB JSON bytes: 1897.54 ms
unbuffered versus 1542.92 ms buffered on this host, not a predicted Mac speedup.
The cache-name regression includes source reuse on the following reconcile.
With a legal colon attachment included, the delayed GPUI publication probe
still publishes the 5001-note tree before reconciliation in 162.21/176.01/168.05 ms.

The second native launch has an additional 30.98-second gap between `vault_ready`
and inventory render; the supplied sample covers the third launch's backlinks,
so it does not identify that gap. New Ready queue/callback timings locate it if
it recurs. Native warm-tree <1 second and background reconcile around <5 seconds
remain owner acceptance checks on the published replacement build.

## Imported timestamps and source reuse (#487)

The next owner log (`reader-diagnostic-6503.txt`, whose new launch records say
build 6517) confirms cached document/tree paint at 0.51–0.55 seconds and short
Ready callbacks. Background reconciliation still takes 11–14 seconds: 5026
sources read and only 66 reused among 5092 notes, with `replay_force_all=false`.
The previous reuse guard required fractional mtime even when the full saved
revision matched. Imported whole-second mtime therefore forced a read and full
backlink parse each launch, despite precise native Unix ctime.

Reuse now accepts fractional mtime **or native Unix ctime**, while still requiring
exact equality of size, mtime, device, inode and ctime. A changed tuple, replay
invalidation, unavailable metadata or genuinely coarse timestamps still forces
a read. No byte hashing or extra filesystem calls were introduced. The shared
CandidateIndex uses the same revision precision helper and equality check.
Regressions cover cache reload, zero-read unchanged sources, same-size writes
with preserved mtime, atomic inode replacement, dirty/forced reads, genuinely
coarse metadata, and a new incoming link in a previously unselected move source.

`reconcile_stats.details.reuse` separates the read rejection reasons (`forced`,
`missing_source`, `invalidated_revision`, `unavailable_metadata`,
`imprecise_revision`, `changed_revision`). These counts sum to `read`. Its
`mismatch` counts identify changed revision fields; several fields can change
in one revision. `coarse_mtime` and `precise_ctime` count all inspected notes.
`graph_reused` reports whether graph parsing was skipped. `replay_invalidations`
reports dirty-path count and whole-root invalidation without logging source text.
The owner snapshot/revisions were not supplied: the fixture establishes a
matching defect, and these native counters distinguish any remaining cause.

Run the imported-time fixture using the core test executable and the same preload:

```sh
OKILUM_SLOW_FS_PREFIX=/tmp/okilum-coarse-reconcile- \
OKILUM_SLOW_FS_MS=2 OKILUM_SLOW_FS_METADATA_US=100 \
OKILUM_CLOUD_PROFILE_SAMPLES=2 LD_PRELOAD="$PWD/target/slow-vault-fs.so" \
  target/debug/deps/okilum_core-<hash> imported_mtime_warm_reconcile_profile \
  --ignored --nocapture --test-threads=1
```

This 5092-note fixture (~50 MiB), 5026 imported mtimes and 66 ordinary mtimes,
reproduces the exact native read/reuse counts before the fix. Actual source opens
and reads each incur 2 ms, while each metadata/canonicalization call incurs
100 microseconds. The split is explicit: uniform 2 ms stats alone would impose
over 10 seconds on the remaining metadata walk and cannot establish a 2–3 second
total target. Separate positive controls assert both syscall delays.
Same Linux host/session: 37469.92/37032.03 ms before → 919.62/869.29 ms after.
Across two samples source opens drop 10052→0, reads 20104→0, stats 30290→10186;
backlink graph parsing is skipped, preserved links are asserted. Cache loading,
persistence and search are outside this timed reconcile. Native timings remain
owner QA; incremental save/watcher indexing is the remaining #487 scope.

The probe also launches two fresh child processes sequentially on that same
persisted cache. Each reloads both startup manifest and full source bank, asserts
`read=0/reused=5092` and graph reuse, refuses any canonical source read, then saves
the next manifest/source bank for the following launch. Canonical notes are not
modified. `IMPORTED_MTIME_RELAUNCH` reports cache loading, reconcile and persistence
separately; the syscall counters for each child verify zero opens/reads inside
reconcile rather than relying on inherited parent memory.

## Directory replay amplification (#502/#487)

The beta 6536 native log confirms source reuse on both the local copy and iCloud.
The slow iCloud open receives 24 replay paths including the vault root: the old
callback converts the root to an empty dirty path, and source invalidation clears
all 5092 saved revisions. Three notes are new; all 5095 are read and reconciliation
takes 26.24 seconds. The zero tuple-mismatch counters in that run do not establish
stable metadata: invalidated revisions bypass tuple comparison.

The following unchanged iCloud launch reads zero/reuses all 5095 sources, retains
the graph, paints the tree/document at 0.444 seconds, reconciles in 278 ms, and
publishes Ready at 1.274 seconds. No FileProvider content-version or ctime relaxation
is required by this evidence.

Root and known directory replay events now leave source revisions available for
the complete startup metadata/revision walk. That walk discovers changed children,
new/deleted paths, and replacements. Exact file and unknown non-root paths still
invalidate sources; incomplete/dropped/coalesced history and root/volume replacement
retain full fallback. `replay_invalidations` adds `directory_paths` and
`root_directory_changed`, independently of explicit `dirty_paths`/`whole_root_dirty`.
Directory events are not discarded from inventory reconciliation.

Portable regressions cover unchanged root/subdirectory events, newly created notes,
same-size child edits with restored mtime, backlinks and forced file invalidation.
The native PR gate executes the real FSEvents replay test and callback tests for
directory classification and invalid-history flags.

Run the paired 5001-note root-event probe using the shell test executable:

```sh
OKILUM_SLOW_FS_PREFIX=/tmp/okilum-replay-directory- \
OKILUM_SLOW_FS_MS=2 OKILUM_SLOW_FS_METADATA_US=100 \
LD_PRELOAD="$PWD/target/slow-vault-fs.so" \
  target/debug/deps/okilum-<hash> \
  reader_replay::tests::directory_replay_cloud_profile \
  --exact --ignored --nocapture --test-threads=1
```

The probe compares the previous empty-root invalidation with the production replay
classifier on the same persisted source bank. Separate positive controls assert
actual source and metadata syscall delays. It refuses any unchanged source read
after directory classification and asserts preserved backlinks. Cache load/persist,
search and native FileProvider behavior are outside the timed reconcile.

Same Linux host/session with 2 ms source I/O and 100 microsecond metadata latency:
legacy root invalidation takes 34493.30/34318.37 ms (5001 reads), compared with
865.94/862.98 ms after classification (zero reads, graph reused). The fixture
measures the replay-invalidation consequence, not native FSEvents delivery,
cache load/persist, search, or final presentation.

## Windows cache publication (#517)

Windows builds search generations in unique directories under
`generations/<fingerprint>.repairs/<uuid>`. The existing completed-generation
lookup opens them only after the writer and merge workers finish and a `complete`
marker is written. Publication does not move a directory containing file handles.
Cancelled/incomplete attempts are disposable; completed generations are immutable
and another attempt never removes them. Other platforms keep directory publication
but explicitly finish merging before moving staging.

Source-bank/startup snapshot persistence is independent of successful disk search.
A memory-only search fallback saves no disk search generation in its snapshot.
The complete cold inventory is sent to the UI before search construction, just as
the warm inventory is, so a search-cache failure cannot hold the tree until Ready.

Windows source revisions use a shared metadata-only handle with no symlink
following. Volume serial, file index, size, last write and native change time are
compared exactly. Precise native change time allows rounded imported mtime reuse;
uncertain/unsupported metadata or genuinely coarse timestamps still require reads.
Old Windows stamps lack native identity/change time and refresh once. This extends
the preserved-mtime regression to Windows; native NTFS execution remains owner QA,
while the Windows CI cross-build validates the production API/cfg path.

QA on bragi: open the same local vault, wait for reconciliation once, Quit and
relaunch without edits. Expect `warm_cache.found:true`, immediate tree/last note,
`read` near zero, `reused` near the note count, and `graph_reused:true`, with no
search publication Access denied. Inspect `reuse.imprecise_revision` and
`reuse.unavailable_metadata` if a filesystem cannot supply a precise native stamp.

## Bounded source reads on network vaults (#516)

The native Windows SMB report contains 5096 notes: discovery 12.7 s,
Checking notes 127.5 s, backlinks 0.6 s. Source/revision checks were serial;
this slice bounds them to eight worker threads. The coordinator alone reports
progress and checks cancellation, and drops the bounded result receiver before
joining workers. Before/after revision safety, UTF-8 checks, per-entry failures,
and the inventory-derived graph are unchanged. Existing blocking OS calls cannot
be interrupted by cancelling a job; all this work remains off the UI thread.

UNC and mapped network drives, macOS SMB/NFS mounts and Linux SMB/NFS mounts
show `Auto-refresh limited` with `Rescan`. This intentionally does not promise
that local watcher APIs observe another network client's writes. Manual Rescan
rereads canonical bytes even when remote metadata appears unchanged. Safe-save
transactions are unchanged and still require native network acceptance.

Windows error 123 keeps the OS cause and explains invalid server names or SMB
short aliases. An original name is shown only if `GetLongPathNameW` supplies it.
Folder/file-picker preparation phases bind to the resolved vault, including UNC,
and per-vault first-paint markers are no longer suppressed by another open root.

Run a paired source-I/O comparison after building the core test executable,
after builds and other tests in this worktree have finished:

```sh
/usr/bin/cc -shared -fPIC -O2 -Wall -Wextra -Werror \
  scripts/probes/slow-vault-fs.c -ldl -o target/slow-vault-fs.so
cargo test --locked -p okilum-core --lib parallel_source_latency_profile --no-run
OKILUM_SLOW_FS_PREFIX=/tmp/okilum-source-latency- \
OKILUM_SLOW_FS_MS=2 OKILUM_SLOW_FS_METADATA_US=100 \
OKILUM_CLOUD_PROFILE_SAMPLES=2 LD_PRELOAD="$PWD/target/slow-vault-fs.so" \
  target/debug/deps/okilum_core-<hash> \
  vault::warm::tests::parallel_source_latency_profile \
  --exact --ignored --nocapture --test-threads=1
```

The preload phase is process-wide so new source threads receive the same actual
open/read/stat latency. Each timed serial/parallel run checks per-phase syscall
counts, all 5001 reads, identical source bytes, inventory and 10000 backlinks.
The follow-up warm run asserts zero source opens/reads and graph reuse.
Same Linux host/session, two paired samples: Checking notes
35001.31/34772.57 ms serial → 4253.98/4216.36 ms parallel (about 8.2×).
Total reconcile 38443.73/37856.23 ms → 7609.72/7614.08 ms. Backlink parsing
remains serial: 3.04–3.35 s in this debug fixture. Both modes perform exactly
5001 opens, 10002 reads and 15004 stats per cold pass; the parallel counters
prove injected delays reached worker threads. Warm: 160.56/161.80 ms,
read=0, reused=5001, graph_reused=true, no source opens. Other host activity
was not controlled; builds/tests in this executor worktree had finished.
This measures core reconcile, excluding cache persistence/search construction,
native SMB transport and native Windows/macOS GUI presentation. Native #516
acceptance, including mapped paths, disconnect/reconnect and safe-save, remains open.

## Warm startup tail on an unchanged vault (#652)

After the document is visible, an unchanged warm relaunch still spent most of
its background time in `background_search_prepare` ("Checking search data")
and `snapshot_persist`. Three pieces of work were avoidable:

- The search fingerprint hashed every source byte with SHA-256 even when the
  retained graph keeps the completed search generation, so the hash was
  discarded. It is now computed only when a new generation name can be used.
  Cached sources are moved into search documents instead of being copied.
- Every reconcile minted a new snapshot ID, so the full source bank (48 MB of
  JSON here) was re-serialised and rewritten. A snapshot decoded from an
  untouched bank file (no incremental delta applied, file revision unchanged
  across the read) now keeps its ID when reconciliation proves the same
  inventory, source bytes, revisions, unreadable entries and retained graph.
  The bank write is skipped only while its snapshot ID, search generation and
  on-disk file revision still match; replay invalidation, incremental batches and any edit
  still save.
- The startup manifest is compared byte-for-byte and rewritten only on change.
  Its graph map is serialised in sorted key order so identical content gives
  identical bytes across processes. Title extraction stops at the first H1.

No cache write in this path calls `fsync`; persistence remains a buffered
temporary file plus rename. Published Ready data (sources, titles, search
generation, graph) is unchanged; the regression
`unchanged_warm_relaunch_keeps_persisted_snapshot_and_ready_data` asserts this
and that the bank and manifest files are untouched, with an edited-note
positive control that does rewrite them.

Benchmark (release profile, 5001 generated notes, 48.45 MB source bank; every
warm sample asserts zero source reads, 5001 reused and a retained graph):

```sh
cargo test --release --locked -p okilum-shell --bin okilum --no-run
target/release/deps/okilum-<hash> warm_startup_tail_profile \
  --ignored --nocapture --test-threads=1
```

Same Linux container and session, paired binaries, five samples each (ms):

| phase | before | after |
|---|---|---|
| `background_search_prepare` | 260.7–290.4 | 69.7–156.9 |
| `snapshot_persist` | 124.0–182.2 | 9.3–22.7 |
| `cached_backlink_titles` | 44.7–82.5 | 18.8–28.9 |
| tail (sum of the three) | 459.8–521.6 | 98.0–198.7 |
| reconcile start to Ready | 754.7–847.9 | 405.1–560.4 |

The test asserts a 400 ms tail budget in release builds;
`OKILUM_WARM_TAIL_BUDGET_MS` overrides it for a slower host. `vault_ready` is
a UI-thread event: this probe measures the worker up to the Ready event, not
native window/GPU presentation. Native macOS/iCloud timings remain owner QA.

Integration regression on Linux (2026-10-08, debug test executable): the existing
`persisted_relaunch_with_small_source_batch_refreshes_graph_and_search` test ran
with 5001 notes, 2 ms per actual open/read and 100 microseconds per metadata call.
Each launch used a new process; open/read/stat positive controls verified the
preload. Both scenarios retained the incremental graph and exact search results:

| Changed files before relaunch | source read / reused | backlinks refresh | next unchanged read / reused | next unchanged Ready |
|---|---|---|---|---|
| 1 | 1 / 5000 | 36.7 ms | 0 / 5001 | 1593 ms |
| 10 | 10 / 4991 | 19.0 ms | 0 / 5001 | 1407 ms |

The unchanged launches made zero source open/read calls and did not rebuild
search. These are correctness checks with injected latency, separate from the
paired release benchmark above; Ready denotes the worker event, not native paint.
