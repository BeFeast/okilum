# Legacy Inbox SQLite fixtures

Synthetic databases created by the actual Inbox Store from main bed1dfd462766ddbed9895cc519b540535446043, with rusqlite 0.37.0, bundled libsqlite3-sys 0.35.0 and SQLite 3.50.2. The checkpointed fixtures contain one capture and its replay identity, fixed UUIDs, Unicode and CRLF text. No user data or credentials.

[Generation run](https://github.com/BeFeast/okilum/actions/runs/43306294766), source 222072fb (main plus the unchanged generator and a temporary CI workflow). The old Inbox lock needed its existing path graph synchronized; the named rusqlite update retained 0.37.0 and libsqlite3-sys 0.35.0, confirmed in the run.

- schema 12 is the current schema written by the old stack.
- schema 1 removes only later tables from that same generated Store, as in the existing migration test, to exercise every migration.
- Files are checkpointed, use DELETE journaling and are vacuumed before capture. No WAL companion is required.

SHA256:

| File | SHA256 |
|---|---|
| rusqlite-0.37-schema-1.db | 030d160c684ebe91a92a0e4df114d7a3e25145874dc5f4e6d1d6c21658864bf1 |
| rusqlite-0.37-schema-12.db | 43baeec8c7b5c5f21caa67e02e4ae53fc845d6c60a4cde335e762f9fdef0c809 |

To reproduce, use a disposable checkout of the baseline above, copy generate.rs to inbox/crates/okilum-inboxd/examples/legacy_fixture.rs, set LEGACY_FIXTURE_OUTPUT to a fresh temporary directory, synchronize the stale path graph with cargo update --manifest-path inbox/Cargo.toml -p rusqlite --precise 0.37.0, then run cargo run --locked --manifest-path inbox/Cargo.toml -p okilum-inboxd --example legacy_fixture on a permitted build runner. Confirm the Cargo tree versions first. Do not regenerate these compatibility fixtures with the new stack.


## Uncheckpointed WAL fixture

`rusqlite-0.37-wal.db` and its `-wal` companion are a snapshot made with the same actual old Store, rusqlite 0.37.0 and SQLite 3.50.2. The writer remains open until both files are copied; no `-shm` is needed. The main DB alone contains the original capture. The committed WAL adds a second capture with UUIDs 6/7 and timestamp 789. The new-stack test first proves its absence without WAL, then verifies recovery, exact text/replay, a new write, reopen and integrity checks. `SHA256SUMS` covers every committed database component.

Generation uses the `generate.rs` source here copied into the old baseline as described above. The fixture must contain WAL frames (the generator and reader test both reject an empty WAL).

`downgrade.rs` is a separate old-stack probe: copy it into the old inboxd examples, run the new-stack `legacy_sqlite` test with `INBOX_DOWNGRADE_DB` pointing to a new file, then run the probe with that same path and the old rusqlite/SQLite stack. It requires recovery of all three captures written by the new test, replays the legacy operation, writes a fourth capture with the old Store and reopens it. This tests the unchanged schema 12 and existing capture data; it is not a guarantee for a future schema migration.

[Uncheckpointed WAL generation run](https://github.com/BeFeast/okilum/actions/runs/43333591336), source `44b534370` over the original old-stack baseline. The generator retains the actual writer through the snapshot and rejects empty WAL. Both checkpointed fixture hashes reproduce exactly.
