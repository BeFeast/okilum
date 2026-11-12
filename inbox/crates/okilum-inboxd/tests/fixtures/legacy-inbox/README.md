# Legacy Inbox SQLite fixtures

Synthetic databases created by the actual Inbox Store from main bed1dfd462766ddbed9895cc519b540535446043, with rusqlite 0.37.0, bundled libsqlite3-sys 0.35.0 and SQLite 3.50.2. Both contain one capture and its replay identity, fixed UUIDs, Unicode and CRLF text. No user data or credentials.

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
