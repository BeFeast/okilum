# Cached validation snapshots and test descriptor limits

A packet's graph receipt must read the same inventory its save phase captured.
An unchanged manifest is still a graph input: it cannot be omitted just because
it is absent from `TEST-CONTRACT.json`'s changed-file list.

Use the versioned helper for **new** cached validation packets, before freshness
touches or Cargo commands. Copy this helper into the packet and record its hash
so a later checkout switch cannot change the receipt implementation. Existing
recorded packets and their logs remain immutable.

```sh
python3 "$packet/cached_graph.py" save --checkout "$checkout" --out "$packet"
# Existing compile/test phases, under the canonical foreground build lock.
python3 "$packet/cached_graph.py" graph --checkout "$checkout" --out "$packet"
```

Replace the packet's old inline `save()` inventory and `graph()` comparison with
these calls. `save` verifies changed-file contract hashes, then snapshots their
union with root manifests/locks, every crate manifest, the vendor lock and vendor
Rust sources. `graph.json` and `protected.json` come from that same inventory.
Snapshots preserve file mtimes and cannot overwrite an existing originals folder.
A changed protected input (including vendor/source files) or a newly
added/removed crate fails the receipt.

Keep the packet's existing source-identity, original-release snapshot, restoration
and final byte/mtime checks. The old-release snapshot is saved separately;
`protected.json` describes checkout-relative inputs only. This helper does not
run Cargo, restore files, acquire a lock or replace those gates. Never repair an
old receipt by inventing missing originals after its compile already ran.

Before any cached Cargo tests, source `scripts/ci/test-nofile.sh` in the **same
shell** that starts Cargo. `check-linux.sh` now does this itself. The helper raises
only that process's soft descriptor limit to its inherited hard limit and logs
both. It fails if the process cannot raise it. No system limits, runner services,
credentials, target caches or Cargo manifests change. Raising a limit in another
SSH session does not affect the test process.

The regressions exercise an unchanged t3 manifest omitted from the contract,
byte/mtime snapshots and mutation detection, plus a real descriptor probe that
fails at soft nofile 1024 and succeeds after the helper in a child shell. They
need no Rust build or canonical checkout mutation.
