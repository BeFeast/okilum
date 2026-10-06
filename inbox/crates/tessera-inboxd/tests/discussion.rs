use tessera_inbox_domain::{Capture, OwnerId};
use tessera_inboxd::{
    discussion::Discuss,
    store::{Error, Store},
};
use uuid::Uuid;

#[test]
fn durable_intent_replay_restart_and_explicit_new_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.db");
    let owner = OwnerId(Uuid::new_v4());
    let item = Uuid::new_v4();
    let mut store = Store::open(&path).unwrap();
    store
        .capture(
            owner,
            &Capture {
                operation_id: Uuid::new_v4(),
                item_id: item,
                text: "original мысль".into(),
            },
            1,
        )
        .unwrap();
    let request = Discuss {
        operation_id: Uuid::new_v4(),
        text: "help clarify".into(),
    };
    let (_, context) = store
        .begin_discussion(owner, item, &request, "fixture", 2)
        .unwrap();
    let context = context.unwrap();
    assert!(context[1].content.contains("original мысль"));
    assert_eq!(context.last().unwrap().content, "help clarify");
    assert!(store
        .begin_discussion(owner, item, &request, "changed-config", 3)
        .unwrap()
        .1
        .is_none());
    let other = Discuss {
        operation_id: Uuid::new_v4(),
        text: "next".into(),
    };
    assert!(matches!(
        store.begin_discussion(owner, item, &other, "fixture", 3),
        Err(Error::DiscussionBusy)
    ));
    assert!(matches!(
        store.begin_discussion(
            owner,
            item,
            &Discuss {
                operation_id: request.operation_id,
                text: "different".into()
            },
            "fixture",
            3
        ),
        Err(Error::OperationConflict)
    ));
    assert!(matches!(
        store.discussion(OwnerId(Uuid::new_v4()), item),
        Err(Error::MissingItem)
    ));
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store.discussion(owner, item).unwrap()[0].state,
        "running",
        "opening a second connection must not interrupt work"
    );
    assert_eq!(store.recover_discussions().unwrap(), 1);
    assert!(matches!(
        store.finish_discussion(owner, request.operation_id, Some("late")),
        Err(Error::InvalidDiscussionTransition)
    ));
    let (turn, context) = store
        .begin_discussion(owner, item, &request, "fixture", 5)
        .unwrap();
    assert_eq!(turn.state, "uncertain");
    assert!(context.is_none());
    let (_, context) = store
        .begin_discussion(owner, item, &other, "fixture", 6)
        .unwrap();
    assert!(
        !context.unwrap().iter().any(|m| m.content == "help clarify"),
        "uncertain exchange is not invented as completed history"
    );
    store
        .finish_discussion(owner, other.operation_id, Some("answer"))
        .unwrap();
    assert!(matches!(
        store.finish_discussion(owner, other.operation_id, None),
        Err(Error::InvalidDiscussionTransition)
    ));
    assert!(matches!(
        store.finish_discussion(owner, Uuid::new_v4(), Some("unknown")),
        Err(Error::InvalidDiscussionTransition)
    ));
    assert_eq!(
        store.discussion(owner, item).unwrap()[1].answer.as_deref(),
        Some("answer")
    );
    assert!(store
        .begin_discussion(owner, item, &other, "fixture", 9)
        .unwrap()
        .1
        .is_none());
    let third = Discuss {
        operation_id: Uuid::new_v4(),
        text: "continue".into(),
    };
    let (_, context) = store
        .begin_discussion(owner, item, &third, "fixture", 10)
        .unwrap();
    let context = context.unwrap();
    assert!(context
        .iter()
        .any(|m| m.role == "assistant" && m.content == "answer"));
    assert_eq!(
        store.item(owner, item).unwrap().unwrap().original_text,
        "original мысль"
    );
}

#[test]
fn migration_preserves_captures_and_bounds_discussion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.db");
    let owner = OwnerId(Uuid::new_v4());
    let item = Uuid::new_v4();
    let mut store = Store::open(&path).unwrap();
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
    drop(store);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "DROP TABLE IF EXISTS execution_outputs; DROP TABLE IF EXISTS execution_results; DROP TABLE IF EXISTS execution_launches; DROP TABLE execution_replies; DROP TABLE execution_questions; DROP TABLE execution_mutations; DROP TABLE execution_briefs; DROP TABLE execution_projects; DROP TABLE publications; DROP TABLE discussion_turns; DROP TABLE auth_passkeys; DROP TABLE sync_scopes; DROP TABLE sync_requests; DROP TABLE sync_grants; PRAGMA user_version=2;",
        )
        .unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store.item(owner, item).unwrap().unwrap().original_text,
        "source"
    );
    assert!(matches!(
        store.begin_discussion(
            owner,
            item,
            &Discuss {
                operation_id: Uuid::nil(),
                text: "x".into()
            },
            "fixture",
            2
        ),
        Err(Error::InvalidDiscussion)
    ));
    for _ in 0..100 {
        let request = Discuss {
            operation_id: Uuid::new_v4(),
            text: "question".into(),
        };
        store
            .begin_discussion(owner, item, &request, "fixture", 2)
            .unwrap();
        store
            .finish_discussion(owner, request.operation_id, Some("answer"))
            .unwrap();
    }
    assert!(matches!(
        store.begin_discussion(
            owner,
            item,
            &Discuss {
                operation_id: Uuid::new_v4(),
                text: "too many".into()
            },
            "fixture",
            3
        ),
        Err(Error::DiscussionLimit)
    ));
}
