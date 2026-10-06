use tessera_inbox_domain::{
    execution::{ProjectDraft, SaveBrief, SaveProject},
    OwnerId,
};
use tessera_inboxd::store::{Error, Store};
use uuid::Uuid;

fn project() -> SaveProject {
    SaveProject {
        operation_id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        expected_revision: 0,
        draft: ProjectDraft {
            title: "Pilot".into(),
            status: "Planning".into(),
            next_step: "Ask one question".into(),
        },
    }
}
#[test]
fn project_replay_precedes_stale_check_and_is_owner_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let who = OwnerId(Uuid::new_v4());
    let request = project();
    let first = store.save_execution_project(who, &request).unwrap();
    let mut update = request.clone();
    update.operation_id = Uuid::new_v4();
    update.expected_revision = 1;
    update.draft.status = "Doing".into();
    assert_eq!(
        store.save_execution_project(who, &update).unwrap().revision,
        2
    );
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.save_execution_project(who, &request).unwrap(), first);
    let mut stale = update.clone();
    stale.operation_id = Uuid::new_v4();
    assert!(matches!(
        store.save_execution_project(who, &stale),
        Err(Error::ExecutionRevisionConflict)
    ));
    stale.operation_id = request.operation_id;
    assert!(matches!(
        store.save_execution_project(who, &stale),
        Err(Error::OperationConflict)
    ));
    assert!(store
        .execution_project(OwnerId(Uuid::new_v4()), request.project_id)
        .unwrap()
        .is_none());
    assert_eq!(store.execution_projects(who, "", 100).unwrap().len(), 1);
}
#[test]
fn brief_revisions_are_immutable_and_cannot_move_between_projects() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("db")).unwrap();
    let who = OwnerId(Uuid::new_v4());
    let p = project();
    store.save_execution_project(who, &p).unwrap();
    let request = SaveBrief {
        operation_id: Uuid::new_v4(),
        project_id: p.project_id,
        brief_id: Uuid::new_v4(),
        expected_revision: 0,
        title: "Probe".into(),
        text: "  # Exact\nТекст\n".into(),
        target_id: "isolated-pilot".into(),
    };
    let first = store.save_execution_brief(who, &request).unwrap();
    let mut edit = request.clone();
    edit.operation_id = Uuid::new_v4();
    edit.expected_revision = 1;
    edit.text = "Revised".into();
    assert_eq!(store.save_execution_brief(who, &edit).unwrap().revision, 2);
    assert_eq!(
        store
            .execution_brief(who, request.brief_id, 1)
            .unwrap()
            .unwrap(),
        first
    );
    assert_eq!(store.save_execution_brief(who, &request).unwrap(), first);
    let other = project();
    store.save_execution_project(who, &other).unwrap();
    edit.operation_id = Uuid::new_v4();
    edit.expected_revision = 2;
    edit.project_id = other.project_id;
    assert!(matches!(
        store.save_execution_brief(who, &edit),
        Err(Error::OperationConflict)
    ));
    assert!(store
        .execution_brief(OwnerId(Uuid::new_v4()), request.brief_id, 1)
        .unwrap()
        .is_none());
    assert!(matches!(
        store.save_execution_brief(OwnerId(Uuid::new_v4()), &request),
        Err(Error::MissingItem)
    ));
}
#[test]
fn concurrent_edit_has_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let who = OwnerId(Uuid::new_v4());
    let request = project();
    Store::open(&path)
        .unwrap()
        .save_execution_project(who, &request)
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let jobs: Vec<_> = (0..2)
        .map(|i| {
            let path = path.clone();
            let barrier = barrier.clone();
            let mut update = request.clone();
            update.operation_id = Uuid::new_v4();
            update.expected_revision = 1;
            update.draft.status = format!("Edit {i}");
            std::thread::spawn(move || {
                let mut store = Store::open(&path).unwrap();
                barrier.wait();
                store.save_execution_project(who, &update)
            })
        })
        .collect();
    let results: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(Error::ExecutionRevisionConflict)))
            .count(),
        1
    );
}

#[test]
fn schema_five_upgrade_preserves_capture_and_operation() {
    use tessera_inbox_domain::Capture;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let who = OwnerId(Uuid::new_v4());
    let request = Capture {
        operation_id: Uuid::new_v4(),
        item_id: Uuid::new_v4(),
        text: "Original\n".into(),
    };
    let mut store = Store::open(&path).unwrap();
    let original = store.capture(who, &request, 1).unwrap();
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS execution_outputs; DROP TABLE IF EXISTS execution_results; DROP TABLE IF EXISTS execution_launches; DROP TABLE execution_replies; DROP TABLE execution_questions; DROP TABLE execution_mutations; DROP TABLE execution_briefs; DROP TABLE execution_projects; PRAGMA user_version=5;").unwrap();
    drop(db);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.capture(who, &request, 999).unwrap(), original);
    store.save_execution_project(who, &project()).unwrap();
}
