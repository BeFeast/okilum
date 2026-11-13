use okilum_inbox_domain::{Capture, OwnerId};
use okilum_inboxd::store::Store;
use rusqlite::Connection;
use uuid::Uuid;

#[test]
fn old_sqlite_databases_open_migrate_and_preserve_durable_replay() {
    let owner = OwnerId(Uuid::from_u128(1));
    let original = Capture {
        operation_id: Uuid::from_u128(2),
        item_id: Uuid::from_u128(3),
        text: "  Legacy capture\r\nOriginal text 📝\n".into(),
    };
    for (schema, fixture) in [
        (
            1,
            include_bytes!("fixtures/legacy-inbox/rusqlite-0.37-schema-1.db").as_slice(),
        ),
        (
            12,
            include_bytes!("fixtures/legacy-inbox/rusqlite-0.37-schema-12.db").as_slice(),
        ),
    ] {
        // SQLite's file header records the library that last wrote the database.
        assert_eq!(
            u32::from_be_bytes(fixture[96..100].try_into().unwrap()),
            3_050_002
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.db");
        std::fs::write(&path, fixture).unwrap();
        {
            let db = Connection::open(&path).unwrap();
            assert_eq!(
                db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
                    .unwrap(),
                schema
            );
        }

        let first;
        let second = Capture {
            operation_id: Uuid::from_u128(4),
            item_id: Uuid::from_u128(5),
            text: "Written after the SQLite upgrade".into(),
        };
        {
            let mut store = Store::open(&path).unwrap();
            first = store.capture(owner, &original, 999).unwrap();
            assert_eq!(
                first.item.original_text.as_bytes(),
                original.text.as_bytes()
            );
            assert_eq!(first.item.received_at_ms, 123);
            assert_eq!(
                store.captures(owner, 0, None, 100).unwrap().changes.len(),
                1
            );
            assert!(store
                .captures(OwnerId(Uuid::from_u128(99)), 0, None, 100)
                .unwrap()
                .changes
                .is_empty());
            store.capture(owner, &second, 456).unwrap();
        }
        {
            let mut reopened = Store::open(&path).unwrap();
            assert_eq!(reopened.capture(owner, &original, 1_000).unwrap(), first);
            let saved = reopened.capture(owner, &second, 1_000).unwrap();
            assert_eq!(saved.item.received_at_ms, 456);
            assert_eq!(
                reopened
                    .captures(owner, 0, None, 100)
                    .unwrap()
                    .changes
                    .len(),
                2
            );
        }
        let db = Connection::open(&path).unwrap();
        assert_eq!(
            db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            12
        );
        assert_eq!(
            db.pragma_query_value::<String, _>(None, "journal_mode", |r| r.get(0))
                .unwrap(),
            "wal"
        );
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert!(!db
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }
}

#[test]
fn unsigned_sql_values_keep_checked_integer_conversions() {
    let db = Connection::open_in_memory().unwrap();
    let maximum = i64::MAX as u64;
    assert_eq!(
        db.query_row("SELECT ?1", [maximum], |r| r.get::<_, u64>(0))
            .unwrap(),
        maximum
    );
    assert!(db
        .query_row("SELECT ?1", [maximum + 1], |r| r.get::<_, u64>(0))
        .is_err());
    assert!(db
        .query_row("SELECT -1", [], |r| r.get::<_, u64>(0))
        .is_err());
}

#[test]
fn legacy_uncheckpointed_wal_is_recovered_before_open_and_replay() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy.db");
    let main = include_bytes!("fixtures/legacy-inbox/rusqlite-0.37-wal.db");
    let wal = include_bytes!("fixtures/legacy-inbox/rusqlite-0.37-wal.db-wal");
    assert_eq!(
        u32::from_be_bytes(main[96..100].try_into().unwrap()),
        3_050_002
    );
    assert!(wal.len() > 32, "fixture must contain WAL frames");
    // Positive control: the pending capture is absent from the main DB alone.
    let bare = directory.path().join("bare.db");
    std::fs::write(&bare, main).unwrap();
    let bare = Connection::open(&bare).unwrap();
    assert_eq!(
        bare.query_row("SELECT count(*) FROM captures", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    std::fs::write(&path, main).unwrap();
    std::fs::write(path.with_file_name("legacy.db-wal"), wal).unwrap();
    let owner = OwnerId(Uuid::from_u128(1));
    let pending = Capture {
        operation_id: Uuid::from_u128(6),
        item_id: Uuid::from_u128(7),
        text: "Committed only in the legacy WAL 📝\r\n".into(),
    };
    {
        let mut store = Store::open(&path).unwrap();
        assert_eq!(
            store.captures(owner, 0, None, 100).unwrap().changes.len(),
            2
        );
        let replay = store.capture(owner, &pending, 999).unwrap();
        assert_eq!(
            replay.item.original_text.as_bytes(),
            pending.text.as_bytes()
        );
        assert_eq!(replay.item.received_at_ms, 789);
        store
            .capture(
                owner,
                &Capture {
                    operation_id: Uuid::from_u128(8),
                    item_id: Uuid::from_u128(9),
                    text: "New writer after WAL recovery".into(),
                },
                456,
            )
            .unwrap();
    }
    let mut reopened = Store::open(&path).unwrap();
    assert_eq!(
        reopened
            .capture(owner, &pending, 1_000)
            .unwrap()
            .item
            .received_at_ms,
        789
    );
    assert_eq!(
        reopened
            .captures(owner, 0, None, 100)
            .unwrap()
            .changes
            .len(),
        3
    );
    drop(reopened);
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(!db
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .exists([])
        .unwrap());
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    // Optional inter-version harness: reopen this exact new-stack DB with the old Store.
    if let Some(destination) = std::env::var_os("INBOX_DOWNGRADE_DB") {
        std::fs::copy(&path, destination).unwrap();
    }
}
