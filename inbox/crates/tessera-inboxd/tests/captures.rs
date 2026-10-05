use rusqlite::Connection;
use tessera_inbox_domain::{Capture, InvalidCapture, OwnerId, MAX_CAPTURE_BYTES};
use tessera_inboxd::store::{Error, Store};
use uuid::Uuid;

fn owner() -> OwnerId {
    OwnerId(Uuid::new_v4())
}
fn capture() -> Capture {
    Capture {
        operation_id: Uuid::new_v4(),
        item_id: Uuid::new_v4(),
        text: "  Мысль с телефона\r\nне потерять исходник 📝\n".into(),
    }
}

#[test]
fn lost_response_replays_exact_original_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("inbox.db");
    let who = owner();
    let request = capture();
    let original = Store::open(&path)
        .unwrap()
        .capture(who, &request, 123)
        .unwrap();
    let mut reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.capture(who, &request, 999).unwrap(), original);
    assert_eq!(
        original.item.original_text.as_bytes(),
        request.text.as_bytes()
    );
    let page = reopened.captures(who, 0, None, 100).unwrap();
    assert_eq!(page.changes.len(), 1);
    assert_eq!(page.changes[0].item, original.item);
}

#[test]
fn conflicting_retry_and_item_reuse_cannot_replace_capture() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(&directory.path().join("inbox.db")).unwrap();
    let who = owner();
    let request = capture();
    let first = store.capture(who, &request, 1).unwrap();
    let mut different = request.clone();
    different.text.push('!');
    assert!(matches!(
        store.capture(who, &different, 2),
        Err(Error::OperationConflict)
    ));
    different = request.clone();
    different.item_id = Uuid::new_v4();
    assert!(matches!(
        store.capture(who, &different, 2),
        Err(Error::OperationConflict)
    ));
    different = request.clone();
    different.operation_id = Uuid::new_v4();
    assert!(matches!(
        store.capture(who, &different, 2),
        Err(Error::ItemConflict)
    ));
    assert_eq!(store.item(who, request.item_id).unwrap(), Some(first.item));
    assert_eq!(store.captures(who, 0, None, 10).unwrap().changes.len(), 1);
}

#[test]
fn auth_owner_scopes_reads_and_replay_identity() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(&directory.path().join("inbox.db")).unwrap();
    let a = owner();
    let b = owner();
    let request = capture();
    store.capture(a, &request, 1).unwrap();
    assert_eq!(store.item(b, request.item_id).unwrap(), None);
    assert!(store.captures(b, 0, None, 10).unwrap().changes.is_empty());
    let mut other = request.clone();
    other.text = "another account".into();
    assert_eq!(
        store.capture(b, &other, 2).unwrap().item.original_text,
        other.text
    );
    assert_eq!(
        store
            .item(a, request.item_id)
            .unwrap()
            .unwrap()
            .original_text,
        request.text
    );
}

#[test]
fn capture_and_operation_are_one_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("inbox.db");
    let mut store = Store::open(&path).unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_operation BEFORE INSERT ON capture_operations BEGIN SELECT RAISE(ABORT, 'injected failure'); END;").unwrap();
    let who = owner();
    let request = capture();
    assert!(matches!(
        store.capture(who, &request, 1),
        Err(Error::Storage(_))
    ));
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert!(store.item(who, request.item_id).unwrap().is_none());
    assert!(store.captures(who, 0, None, 10).unwrap().changes.is_empty());
    db.execute_batch("DROP TRIGGER fail_operation").unwrap();
    store.capture(who, &request, 2).unwrap();
    assert_eq!(store.captures(who, 0, None, 10).unwrap().changes.len(), 1);
}

#[test]
fn concurrent_delivery_on_two_connections_commits_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("inbox.db");
    let stores = [Store::open(&path).unwrap(), Store::open(&path).unwrap()];
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let who = owner();
    let request = capture();
    let handles: Vec<_> = stores
        .into_iter()
        .map(|mut store| {
            let barrier = barrier.clone();
            let request = request.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.capture(who, &request, 1).unwrap()
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results[0], results[1]);
    assert_eq!(
        Store::open(&path)
            .unwrap()
            .captures(who, 0, None, 10)
            .unwrap()
            .changes
            .len(),
        1
    );
}

#[test]
fn pagination_has_a_stable_boundary_while_new_captures_arrive() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(&directory.path().join("inbox.db")).unwrap();
    let who = owner();
    let first = store.capture(who, &capture(), 1).unwrap();
    store.capture(owner(), &capture(), 2).unwrap(); // A different owner's sequence gap.
    let second = store.capture(who, &capture(), 3).unwrap();
    let page = store.captures(who, 0, None, 1).unwrap();
    assert!(page.has_more);
    assert_eq!(page.changes[0].item, first.item);
    let third = store.capture(who, &capture(), 4).unwrap();
    let tail = store
        .captures(who, page.next_after, Some(page.through), 1)
        .unwrap();
    assert!(!tail.has_more);
    assert_eq!(tail.changes[0].item, second.item);
    assert_eq!(tail.next_after, page.through);
    let next = store.captures(who, tail.next_after, None, 10).unwrap();
    assert_eq!(next.changes.len(), 1);
    assert_eq!(next.changes[0].item, third.item);
    assert!(matches!(
        store.captures(who, 0, None, 0),
        Err(Error::InvalidPage)
    ));
    assert!(matches!(
        store.captures(who, 0, Some(u64::MAX), 1),
        Err(Error::InvalidPage)
    ));
}

#[test]
fn invalid_capture_does_not_consume_identity() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(&directory.path().join("inbox.db")).unwrap();
    let who = owner();
    let request = capture();
    for (text, expected) in [
        (" \n".into(), InvalidCapture::Empty),
        ("x".repeat(MAX_CAPTURE_BYTES + 1), InvalidCapture::TooLarge),
    ] {
        let mut invalid = request.clone();
        invalid.text = text;
        assert!(
            matches!(store.capture(who, &invalid, 1), Err(Error::InvalidCapture(e)) if e == expected)
        );
    }
    assert!(matches!(
        store.capture(OwnerId(Uuid::nil()), &request, 1),
        Err(Error::InvalidOwner)
    ));
    assert!(matches!(
        store.capture(who, &request, -1),
        Err(Error::InvalidTimestamp)
    ));
    let mut invalid = request.clone();
    invalid.item_id = Uuid::nil();
    assert!(matches!(
        store.capture(who, &invalid, 1),
        Err(Error::InvalidCapture(InvalidCapture::Identity))
    ));
    assert!(store.captures(who, 0, None, 10).unwrap().changes.is_empty());
    store.capture(who, &request, 1).unwrap();
}

#[test]
fn foreign_newer_and_corrupt_databases_are_not_reset() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("foreign.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE valuable(content TEXT); INSERT INTO valuable VALUES ('keep');")
        .unwrap();
    assert!(matches!(
        Store::open(&path),
        Err(Error::UnsupportedDatabase)
    ));
    assert_eq!(
        db.query_row("SELECT content FROM valuable", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "keep"
    );
    db.execute_batch("PRAGMA user_version=99").unwrap();
    assert!(matches!(
        Store::open(&path),
        Err(Error::UnsupportedDatabase)
    ));
    let corrupt = directory.path().join("corrupt.db");
    std::fs::write(&corrupt, b"not a sqlite database").unwrap();
    assert!(Store::open(&corrupt).is_err());
    assert_eq!(std::fs::read(corrupt).unwrap(), b"not a sqlite database");
}

#[test]
fn auth_schema_upgrade_preserves_existing_capture_and_replay() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("inbox.db");
    let who = owner();
    let request = capture();
    let original = Store::open(&path)
        .unwrap()
        .capture(who, &request, 1)
        .unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE execution_replies; DROP TABLE execution_questions; DROP TABLE execution_mutations; DROP TABLE execution_briefs; DROP TABLE execution_projects; DROP TABLE publications; DROP TABLE discussion_turns; DROP TABLE auth_owner; PRAGMA user_version=1;")
        .unwrap();
    assert_eq!(
        Store::open(&path)
            .unwrap()
            .capture(who, &request, 2)
            .unwrap(),
        original
    );
}
