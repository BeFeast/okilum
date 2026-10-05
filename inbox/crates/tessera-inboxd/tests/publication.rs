#![cfg(target_os = "linux")]
use std::{fs, os::unix::fs::symlink};
use tessera_inbox_domain::{Capture, OwnerId};
use tessera_inboxd::{
    publication::Publish,
    store::{Error, Store},
    vault::Vault,
};
use uuid::Uuid;

#[test]
fn publication_is_create_only_replayable_and_original_survives() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    fs::create_dir_all(root.join("Projects")).unwrap();
    let db = dir.path().join("inbox.db");
    let owner = OwnerId(Uuid::new_v4());
    let item = Uuid::new_v4();
    let mut store = Store::open(&db).unwrap();
    store
        .capture(
            owner,
            &Capture {
                operation_id: Uuid::new_v4(),
                item_id: item,
                text: "original".into(),
            },
            1,
        )
        .unwrap();
    let vault = Vault::open(&root, vec!["Projects".into()]).unwrap();
    let request = Publish {
        operation_id: Uuid::new_v4(),
        folder: "Projects".into(),
        filename: "Идея.md".into(),
        content: "# Идея\nExact draft\n".into(),
    };
    assert_eq!(
        vault
            .publish(&mut store, owner, item, &request)
            .unwrap()
            .state,
        "published"
    );
    drop(store);
    let mut store = Store::open(&db).unwrap();
    assert_eq!(
        vault
            .publish(&mut store, owner, item, &request)
            .unwrap()
            .state,
        "published"
    );
    assert_eq!(
        fs::read_to_string(root.join("Projects/Идея.md")).unwrap(),
        request.content
    );
    assert_eq!(fs::read_dir(root.join("Projects")).unwrap().count(), 1);
    assert_eq!(
        store.item(owner, item).unwrap().unwrap().original_text,
        "original"
    );
    let mut collision = request.clone();
    collision.operation_id = Uuid::new_v4();
    assert!(
        matches!(
            vault.publish(&mut store, owner, item, &collision),
            Err(Error::PublicationConflict)
        ),
        "same bytes under another operation do not prove delivery"
    );
    let mut altered = request.clone();
    altered.content = "changed".into();
    assert!(matches!(
        vault.publish(&mut store, owner, item, &altered),
        Err(Error::OperationConflict)
    ));
    fs::write(root.join("Projects/Идея.md"), "external edit").unwrap();
    assert!(matches!(
        vault.publish(&mut store, owner, item, &request),
        Err(Error::PublicationConflict)
    ));
    assert_eq!(
        fs::read_to_string(root.join("Projects/Идея.md")).unwrap(),
        "external edit"
    );
}
#[test]
fn recovery_after_link_before_ack_and_symlink_traversal_rejection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    fs::create_dir_all(root.join("Projects")).unwrap();
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let mut store = Store::open(&dir.path().join("inbox.db")).unwrap();
    let owner = OwnerId(Uuid::new_v4());
    let item = Uuid::new_v4();
    store
        .capture(
            owner,
            &Capture {
                operation_id: Uuid::new_v4(),
                item_id: item,
                text: "original".into(),
            },
            1,
        )
        .unwrap();
    let vault = Vault::open(&root, vec!["Projects".into()]).unwrap();
    let request = Publish {
        operation_id: Uuid::new_v4(),
        folder: "Projects".into(),
        filename: "resume.md".into(),
        content: "durable".into(),
    };
    store.prepare_publication(owner, item, &request).unwrap();
    let stage = root
        .join("Projects")
        .join(format!(".tessera-inbox-{}.tmp", request.operation_id));
    fs::write(&stage, &request.content).unwrap();
    store
        .advance_publication(owner, request.operation_id, "queued", "prepared")
        .unwrap();
    fs::hard_link(&stage, root.join("Projects/resume.md")).unwrap();
    assert_eq!(
        vault
            .publish(&mut store, owner, item, &request)
            .unwrap()
            .state,
        "published"
    );
    for filename in [
        "../escape.md",
        "/absolute.md",
        "a/../b.md",
        "a//b.md",
        "a/.hidden/b.md",
        "a\\b.md",
        ".hidden.md",
        "wrong.txt",
    ] {
        let mut bad = request.clone();
        bad.operation_id = Uuid::new_v4();
        bad.filename = filename.into();
        assert!(
            vault.publish(&mut store, owner, item, &bad).is_err(),
            "{filename}"
        );
    }
    let mut bad = request.clone();
    bad.operation_id = Uuid::new_v4();
    bad.folder = "../outside".into();
    assert!(vault.publish(&mut store, owner, item, &bad).is_err());
    symlink(outside.join("target.md"), root.join("Projects/link.md")).unwrap();
    bad.folder = "Projects".into();
    bad.filename = "link.md".into();
    assert!(vault.publish(&mut store, owner, item, &bad).is_err());
    assert!(!outside.join("target.md").exists());
    fs::rename(root.join("Projects"), root.join("old")).unwrap();
    symlink(&outside, root.join("Projects")).unwrap();
    bad.filename = "escape.md".into();
    assert!(vault.publish(&mut store, owner, item, &bad).is_err());
    assert!(!outside.join("escape.md").exists());
    assert!(Vault::open(&root, vec!["Projects".into()]).is_err());
}

#[test]
fn restart_recovers_partial_queued_stage_without_overwriting_destination() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    fs::create_dir_all(root.join("Projects")).unwrap();
    let database = dir.path().join("inbox.db");
    let mut store = Store::open(&database).unwrap();
    let owner = OwnerId(Uuid::new_v4());
    let item = Uuid::new_v4();
    store
        .capture(
            owner,
            &Capture {
                operation_id: Uuid::new_v4(),
                item_id: item,
                text: "source".into(),
            },
            1,
        )
        .unwrap();
    let request = Publish {
        operation_id: Uuid::new_v4(),
        folder: "Projects".into(),
        filename: "Recovered.md".into(),
        content: "complete content".into(),
    };
    store.prepare_publication(owner, item, &request).unwrap();
    fs::write(
        root.join("Projects")
            .join(format!(".tessera-inbox-{}.tmp", request.operation_id)),
        "com",
    )
    .unwrap();
    drop(store);
    let mut store = Store::open(&database).unwrap();
    let vault = Vault::open(&root, vec!["Projects".into()]).unwrap();
    assert_eq!(
        vault
            .publish(&mut store, owner, item, &request)
            .unwrap()
            .state,
        "published"
    );
    assert_eq!(
        fs::read_to_string(root.join("Projects/Recovered.md")).unwrap(),
        "complete content"
    );
    assert_eq!(fs::read_dir(root.join("Projects")).unwrap().count(), 1);
}

#[test]
fn nested_publication_conflict_and_legacy_inspection_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    fs::create_dir_all(root.join("Projects")).unwrap();
    let database = dir.path().join("inbox.db");
    let mut store = Store::open(&database).unwrap();
    let owner = OwnerId(Uuid::new_v4());
    let item = Uuid::new_v4();
    store
        .capture(
            owner,
            &Capture {
                operation_id: Uuid::new_v4(),
                item_id: item,
                text: "original".into(),
            },
            1,
        )
        .unwrap();
    let vault = Vault::open(&root, vec!["Projects".into()]).unwrap();
    let mut request = Publish {
        operation_id: Uuid::new_v4(),
        folder: "Projects".into(),
        filename: "tessera/идеи/План.md".into(),
        content: "# План".into(),
    };
    assert_eq!(
        vault
            .publish(&mut store, owner, item, &request)
            .unwrap()
            .state,
        "published"
    );
    let path = root.join("Projects/tessera/идеи/План.md");
    assert_eq!(fs::read_to_string(&path).unwrap(), request.content);
    request.operation_id = Uuid::new_v4();
    assert!(matches!(
        vault.publish(&mut store, owner, item, &request),
        Err(Error::PublicationConflict)
    ));
    drop(store);
    let mut store = Store::open(&database).unwrap();
    let rows = store.publications(owner, item).unwrap();
    assert_eq!(rows[0].state, "published");
    assert_eq!(rows[1].conflict.as_deref(), Some("file_exists"));
    fs::remove_file(&path).unwrap();
    assert!(
        matches!(
            vault.publish(&mut store, owner, item, &request),
            Err(Error::PublicationConflict)
        ),
        "known collision is terminal even if target disappears"
    );
    assert!(!path.exists());
    // Old prepared records cannot prove whether a link succeeded before crash.
    request.operation_id = Uuid::new_v4();
    store.prepare_publication(owner, item, &request).unwrap();
    store
        .advance_publication(owner, request.operation_id, "queued", "prepared")
        .unwrap();
    let stage = path
        .parent()
        .unwrap()
        .join(format!(".tessera-inbox-{}.tmp", request.operation_id));
    fs::write(&stage, &request.content).unwrap();
    fs::write(&path, "another file").unwrap();
    let mut old = store.publications(owner, item).unwrap().pop().unwrap();
    vault.inspect(&mut old);
    assert_eq!(old.conflict.as_deref(), Some("occupied"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "another file");
    fs::remove_file(&path).unwrap();
    fs::hard_link(stage, &path).unwrap();
    let mut lost_ack = store.publications(owner, item).unwrap().pop().unwrap();
    vault.inspect(&mut lost_ack);
    assert_eq!(lost_ack.conflict, None);
    assert_eq!(
        vault
            .publish(&mut store, owner, item, &request)
            .unwrap()
            .state,
        "published"
    );
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, root.join("Projects/escape")).unwrap();
    request.operation_id = Uuid::new_v4();
    request.filename = "escape/new/evil.md".into();
    assert!(vault.publish(&mut store, owner, item, &request).is_err());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn schema_four_publications_migrate_without_losing_replay_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.db");
    let mut store = Store::open(&path).unwrap();
    let owner = OwnerId(Uuid::new_v4());
    let item = Uuid::new_v4();
    store
        .capture(
            owner,
            &Capture {
                operation_id: Uuid::new_v4(),
                item_id: item,
                text: "original".into(),
            },
            1,
        )
        .unwrap();
    let request = Publish {
        operation_id: Uuid::new_v4(),
        folder: "Projects".into(),
        filename: "old.md".into(),
        content: "# Legacy".into(),
    };
    store.prepare_publication(owner, item, &request).unwrap();
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE execution_mutations; DROP TABLE execution_briefs; DROP TABLE execution_projects; ALTER TABLE publications DROP COLUMN conflict; PRAGMA user_version=4;")
        .unwrap();
    drop(db);
    let mut store = Store::open(&path).unwrap();
    let migrated = store.prepare_publication(owner, item, &request).unwrap();
    assert_eq!(migrated.request, request);
    assert_eq!(migrated.state, "queued");
    assert_eq!(migrated.conflict, None);
}
