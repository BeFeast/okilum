# Editor source publication

The canonical save still owns exact-byte conflict checks, atomic publication,
recovery acknowledgement and the editor lock. Watcher/index publication is a
consumer of that result; it cannot authorize a write.

A successful save installs a SHA-256 receipt of the exact committed bytes on the
save worker. Its short receipt lock is separate from the canonical I/O lock.
The receipt names the saved immutable base and its generation: an external
refresh or Reload revokes an older receipt. A path, timestamp, queued draft or
watcher notification is never an own-save proof. The UI may observe the receipt
before the autosave completion callback; this does not acknowledge recovery.

Both incremental workers hash their observed source bytes off-thread. Shared
sessions retain these immutable publications independently of their mutable
baseline, including when another window starts the next batch. Full reconciliations retain the same inventory ownership and publish a complete
worker-prepared source snapshot; a versioned incremental publication then refreshes
only affected documents. Consuming a publication neither decodes the source bank
nor reads the file on the UI thread.
An exact receipt match keeps the current editor revision, Undo and preparation.
A later external revision remains observable through the next queued batch; a
subsequent save always checks canonical bytes independently of the receipt.

Own saves retain the inventory Vault Arc, including title, alias and link edits.
Workers publish an immutable semantic snapshot containing aliases and backlinks
through that stable handle. Cloning a Vault freezes the current semantics into
an independent index baseline; later publication cannot mutate that baseline.
Only real inventory/availability changes need a replacement inventory Arc.

A separate resolution identity changes for inventory or alias resolution, but
not for backlink-context/title changes. Each visible Live block lazily checks
its stored identity and dependency flag. There is no cache-wide scan or clear
on publication. A bounded FIFO retires obsolete block keys individually.
Graph-independent Markdown stays prepared; resolution-dependent
blocks prepare only when visited after a relevant change. Titles and
search/task/workbench indexes still publish their worker results.

External editor refreshes sample the native immutable revision, then use a
FileEditor copy on a worker for reads, exact comparisons and recovery persistence.
A refresh may claim only its captured journal generation. A new observation copy
resynchronizes generation ownership after an unadopted refresh reservation; it
never adopts that discarded worker’s canonical base. Foreground adoption
also checks the root, path, input identity, native stamp, explicit-save sequence,
recovery epoch and reconciliation generation. A superseded result never replaces
newer input; it is retried against the current revision. Dirty source remains
visible with the conflict version prepared by the worker. Deleted/returned notes
retain their explicit decision flow.

`issue2405_*` tests cover real debounced autosaves through both routes, a duplicate
reader, prepared-block retention, semantic alias/backlink/title publication,
external clean/dirty source, exact BOM/CRLF receipts and superseded recovery.
The allocation meter scopes foreground persistence/publication callbacks rather
than GPUI executor trace allocations. It also records inclusive elapsed nanoseconds
for the outermost foreground scopes, so nested scopes are not double-counted.
Production publication diagnostics separately report foreground duration and
queue/worker latency; the old unshared duration included both. The legacy refresh operation is an
allocator positive control at every size. Native projection and part-2 persistence
retain their own protected work probes.

For a real before/after control on one hosted VM, manually dispatch
`agent-checks.yml` on a public worker ref with `publication_baseline` set to a
pinned public main SHA. `scripts/perf/editor/publication-controls.py` archives that
SHA, overlays the identical probes and instrumentation, requires successful
compilation and a failing main publication probe, then runs the final-head probes
on the same host. It never mutates the worker checkout or treats a build failure
as a test verdict. The measurements are deterministic foreground work, not native
end-to-end latency or physical display timing.
