# Progressive Reader opening (#338)

The Reader window is created before filesystem validation. Plain open intent travels
into a background job; an exact requested file never falls back to another note.
Folder discovery follows the shared Vault inclusion rules and stops at the first
readable Markdown file without completing or sorting the inventory.

The first document is prepared separately. Its TextView parses as a candidate while
the previous root, content, history and focus remain active. The candidate is
published only after TextView reports successful preparation of both source and
Reader parser configuration. Preparation readiness follows the accepted parse
revision, so equal text cannot acknowledge a pending configuration parse. Acknowledging that
publication releases the remaining inventory/backlinks/search worker. Empty roots
are terminal prepublication outcomes: they retain the old Reader and do not
record a last document or release the inventory worker.

Each Reader owns one generation and cooperative cancellation token. Progress,
publication and errors must match that generation. Explicit cancellation before
publication retains the previous Reader; cancellation after publication retains the
new document. Supersession revokes the previous token. Entity destruction revokes
its worker token. Inventory completion leaves unchanged content, selection,
scroll and focus intact. A changed primary source or pending embed is reconciled
under root/document/navigation fences, preserving history and the current scroll
position. Retry after publication resumes derived preparation. Old-root search
rows are cleared atomically at first publication, with stale pointer callbacks
fenced by their published root and generation; query text is retained.

Vault scanning, backlink parsing, watcher registration/draining, source reads and
index preparation run on the background executor. The watcher starts before the
inventory scan and scan-time changes cause reconciliation before Ready. Ordinary
watcher refresh uses the same background preparation path. Borrowed watcher
ownership follows the published root, independently of candidate load generations;
changes delivered during a candidate attempt are deferred until it settles.
Ready compares the displayed primary body with the indexed source snapshot,
covering the gap before watcher registration as well as drained scan-time edits.
Incomplete inventories render transclusions as loading, then reconcile their
bodies when inventory is available. Source files are read only. Link, search and backlink state remain pending while inventory is incomplete.

Search generations are immutable and content-addressed. A build uses the same
source snapshot as its fingerprint, retaining the shared resolver's `links_to`
semantics. Each attempt owns its staging directory. Cancellation removes only that
staging directory; publishing a completed generation never destroys an active
Reader's index or updates a mutable current-generation pointer. Warm preparation
checks content identity and validates opening the matching generation. A corrupt
completed generation is retained while a healthy immutable repair instance is
built and reused; another Reader's index is never recursively deleted. Cache paths
with parent traversal are rejected before directory creation. Cache generation
garbage collection is not implemented in this slice.

## Last-document history

`reader_history.rs` keeps a root-specific last-document map in the app's state
directory (Application Support on macOS, never inside notes or indexes). A Reader
records a document only after it is published and usable. When a folder opens
without an explicit document, the hint is validated with the existing OpenIntent
contract; a missing hint falls back, but an explicit missing document does not.
Readiness is never inferred from an empty placeholder inventory or from
full-index completion.

## Panel preferences during progressive open

Reader construction uses default panel widths without preference filesystem I/O.
The existing load worker validates the settings destination against the accepted
root and reads saved widths after the first usable document is published. Root
publication immediately revokes the predecessor's settings destination; refusal
leaves persistence disabled. A failed replacement retains the old usable Reader
and its settings capability. Preference delivery requires the current generation
and published root, and cannot overwrite widths edited since that job began.
Cancel revokes pending preference delivery as well as the remaining preparation.

## Evidence boundaries

Focused tests cover exact missing-file refusal, validated root-specific hint
selection, cancellation/supersession, immutable cold/warm cache generations, and
5001 synthetic notes with full preparation held for over three seconds. The GPUI
harness verifies first-document rendering, find, scrolling and cancellation before
release, with an unfinished-index positive control. Separate timings report first
frame, first document, input/scroll, cancellation acknowledgement, delayed teardown
and uncancelled cold/warm whole readiness. Source manifests cover every canonical
name and byte in the nested cold/warm fixture.

These are Linux test-harness observations, not native macOS timing or Finder
acceptance.
Vendor patch 0020 exposes revision-aware preparation status and a way to install
the same TextView parser configuration before staging source. GPUI core is unchanged.
Additional regressions exercise corrupt-generation recovery, concurrent staged
build cancellation, actual retained-watcher delivery, scan-time changes, pending
embed reconciliation and last-document recording.

## Default launch (#364)

An ordinary launch restores the most recently published local root, then uses its
validated last-document hint through the progressive opener. Brain profiles are
neither read nor changed on this path. `--managed-workspace` explicitly opens the
saved Brain chooser; `--brain-endpoint IP:PORT` remains an explicit managed launch.

The existing history file now records root recency alongside per-root documents.
An older file with a single root can be restored; multiple roots without recorded
recency are shown as recent choices rather than guessed. First launch, unavailable
roots and invalid history use the local “Open your notes” entry with Open folder /
Open file controls. Late explicit OS file delivery supersedes an in-flight history
read. Canonical notes and saved Brain identities are untouched.
