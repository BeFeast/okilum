use super::*;
use ::core::prelude::v1::test;
use okilum_t3::chat::MessageJournal;

fn metadata(kind: MessageType) -> Metadata {
    Metadata {
        kind,
        provenance: Provenance::ExplicitSelection,
        author: Author::User,
    }
}

#[test]
fn typed_1478_annotation_commits_before_exact_mcp_intent_and_refuses_relabeling() {
    let state = tempfile::tempdir().unwrap();
    let environment = Environment::new("fixture", "https://typed.invalid").unwrap();
    let store = Annotations::new(state.path(), &environment, "project", "thread");
    let capture = SendType {
        store: store.clone(),
        text: "BUG:  original\r\n🧠".into(),
        metadata: metadata(MessageType::Bug),
    };
    let message = okilum_t3::OutgoingMessage::queue("thread", capture.text.clone());
    let journal = MessageJournal::new(state.path(), &environment, "thread");
    let record = capture.prepare_mcp(&journal, message.clone()).unwrap();
    assert_eq!(record.message, message);
    let (revision, entries) = read(&store).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].message, message.client_request_id);
    assert_eq!(entries[0].source, fingerprint(&message.text, Author::User));
    capture.retain(&message.client_request_id).unwrap();
    assert_eq!(
        read(&store).unwrap().0,
        revision,
        "same identity/type is idempotent"
    );
    for conflicting in [
        SendType {
            text: "changed source".into(),
            ..capture.clone()
        },
        SendType {
            metadata: metadata(MessageType::Decision),
            ..capture.clone()
        },
    ] {
        assert!(conflicting.retain(&message.client_request_id).is_err());
    }
    assert_eq!(read(&store).unwrap(), (revision, entries));
    assert_eq!(journal.records().unwrap(), vec![record]);
    assert!(read(&Annotations::new(
        state.path(),
        &environment,
        "other",
        "thread"
    ))
    .unwrap()
    .1
    .is_empty());
    assert!(read(&Annotations::new(
        state.path(),
        &environment,
        "project",
        "other"
    ))
    .unwrap()
    .1
    .is_empty());
    let other = Environment::new("fixture", "https://other.invalid").unwrap();
    assert!(
        read(&Annotations::new(state.path(), &other, "project", "thread"))
            .unwrap()
            .1
            .is_empty()
    );
}

#[test]
fn typed_1478_annotation_failure_leaves_no_intent_and_prepare_failure_never_reuses_id() {
    let state = tempfile::tempdir().unwrap();
    let environment = Environment::new("fixture", "https://typed.invalid").unwrap();
    let blocked = state.path().join("blocked");
    std::fs::write(&blocked, b"not a directory").unwrap();
    let capture = SendType {
        store: Annotations::new(&blocked, &environment, "project", "thread"),
        text: "body".into(),
        metadata: metadata(MessageType::Bug),
    };
    let journal = MessageJournal::new(state.path(), &environment, "thread");
    let message = okilum_t3::OutgoingMessage::queue("thread", "body");
    assert!(capture.prepare_mcp(&journal, message.clone()).is_err());
    assert!(journal.records().unwrap().is_empty());
    assert!(
        !state.path().join("t3-messages").exists(),
        "annotation failure cannot prepare an intent"
    );
    let capture = SendType {
        store: Annotations::new(state.path(), &environment, "project", "thread"),
        ..capture
    };
    let wrong_source = okilum_t3::OutgoingMessage {
        text: "different".into(),
        ..message.clone()
    };
    assert!(capture.prepare_mcp(&journal, wrong_source).is_err());
    assert!(read(&capture.store).unwrap().1.is_empty());
    // Positive control: writable annotation, followed by a failing journal.
    let wrong_journal = MessageJournal::new(state.path(), &environment, "another-thread");
    assert!(capture
        .prepare_mcp(&wrong_journal, message.clone())
        .is_err());
    assert!(wrong_journal.records().unwrap().is_empty());
    let (revision, entries) = read(&capture.store).unwrap();
    assert_eq!(entries[0].message, message.client_request_id);
    let record = capture.prepare_mcp(&journal, message.clone()).unwrap();
    assert_eq!(
        record.message, message,
        "recover using the same preallocated identity"
    );
    assert_eq!(read(&capture.store).unwrap(), (revision, entries));
}

#[test]
fn typed_1478_annotation_cas_retains_winner_and_full_author_source_binding() {
    let state = tempfile::tempdir().unwrap();
    let environment = Environment::new("fixture", "https://typed.invalid").unwrap();
    let store = Annotations::new(state.path(), &environment, "project", "thread");
    let initial = Annotation {
        message: "exact-id".into(),
        source: fingerprint("body", Author::Manager),
        metadata: Metadata {
            author: Author::Manager,
            ..metadata(MessageType::Decision)
        },
    };
    let (revision, _) = write(&store, 0, None, initial.clone()).unwrap();
    let winner = Annotation {
        metadata: Metadata {
            kind: MessageType::Status,
            provenance: Provenance::UserCorrection,
            ..initial.metadata
        },
        ..initial.clone()
    };
    let (next, entries) = write(&store, revision, Some(&initial), winner.clone()).unwrap();
    assert_eq!(
        next,
        revision + 1,
        "metadata-only correction increments revision"
    );
    assert!(write(&store, revision, Some(&initial), initial.clone()).is_err());
    assert!(write(&store, next, Some(&initial), initial.clone()).is_err());
    assert_eq!(read(&store).unwrap(), (next, entries));
    assert_ne!(winner.source, fingerprint("body", Author::User));
    assert_ne!(winner.source, fingerprint("body\n", Author::Manager));
}

fn binding(project: &str, thread: &str) -> AreaBinding {
    AreaBinding::parse(&format!("t3:\n  schema_version: 1\n  environment: fixture\n  manager: {{project_id: '{project}', thread_id: '{thread}', workspace_mode: local}}\n  executor: {{project_id: executors}}\n  default_models:\n    manager: {{instanceId: codex, model: gpt-6.1-sol}}\n    executor: {{instanceId: codex, model: gpt-6.1-sol}}\n")).unwrap().unwrap()
}

#[gpui::test]
fn typed_1478_actual_mcp_send_keeps_draft_on_annotation_failure_and_retries_retained_identity(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    cx.update(reader_features::enable_test_modules);
    let state = tempfile::tempdir().unwrap();
    let environment = Environment::new("fixture", "https://typed.invalid").unwrap();
    let gateway = Arc::new(crate::project::fixture::Fake {
        fail_first: true,
        ..Default::default()
    });
    let blocked = state.path().join("t3-message-types");
    std::fs::write(&blocked, b"not a directory").unwrap();
    let mut owner = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = ProjectView::empty(Some(state.path().to_owned()), window, cx);
            view.binding = Some(binding("project", "thread"));
            view.chat = Some(Conversation::new("thread"));
            view.environment = Some(environment.clone());
            view.gateway = Some(gateway.clone());
            view.identity = Some(ServerIdentity {
                environment_id: "fixture".into(),
                label: "Fixture".into(),
            });
            view.draft_restore(window, cx);
            view
        });
        let composer = view.read(cx).composer.clone();
        owner = Some(view);
        Root::new(composer, window, cx)
    });
    let view = owner.unwrap();
    visual.run_until_parked();
    let text = "FR:  Full original\r\n🧠  ";
    view.update_in(visual, |view, window, cx| {
        let mut draft = TypedDraft::from_text(text.into());
        assert!(draft.detect_token());
        view.composer.update(cx, |composer, cx| {
            composer
                .replace_typed_draft(
                    draft.text.clone(),
                    Some(draft.retained_metadata()),
                    window,
                    cx,
                )
                .unwrap()
        });
        view.draft_changed(window, cx);
        assert!(
            view.can_send(),
            "positive control: the normal MCP send guard permits this fake connection"
        );
        view.send(None, window, cx);
    });
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        assert_eq!(view.composer.read(cx).typed_source_text(cx), text);
        assert!(view.records.is_empty());
        assert!(view.error.is_some());
    });
    assert!(gateway.sends.lock().unwrap().is_empty());
    assert!(!state.path().join("t3-messages").exists());
    std::fs::remove_file(blocked).unwrap();
    view.update_in(visual, |view, window, cx| view.send(None, window, cx));
    visual.run_until_parked();
    let record = view.read_with(visual, |view, cx| {
        assert!(view.composer.read(cx).typed_source_text(cx).is_empty());
        assert_eq!(view.records.len(), 1);
        assert!(
            view.records[0].receipt.is_none(),
            "fake ambiguous dispatch retains an unacknowledged intent"
        );
        view.records[0].clone()
    });
    assert_eq!(
        record.message.text, text,
        "dispatch uses full source, not the pill-stripped body"
    );
    let store = Annotations::new(
        &state.path().join("t3-message-types"),
        &environment,
        "project",
        "thread",
    );
    let saved = read(&store).unwrap();
    assert_eq!(saved.1[0].message, record.message.client_request_id);
    assert_eq!(saved.1[0].metadata.provenance, Provenance::ExplicitToken);
    view.update_in(visual, |view, window, cx| {
        view.send(Some(record.clone()), window, cx)
    });
    visual.run_until_parked();
    let sends = gateway.sends.lock().unwrap();
    assert_eq!(sends.len(), 2);
    assert_eq!(
        sends[0], sends[1],
        "retry must dispatch the same retained message identity and text"
    );
    assert_eq!(
        read(&store).unwrap(),
        saved,
        "retry cannot relabel or allocate another annotation"
    );
    let wire = serde_json::to_value(&sends[0]).unwrap();
    assert_eq!(
        wire.as_object().unwrap().len(),
        4,
        "local annotations add no outgoing fields"
    );
    view.read_with(visual, |view, _| assert!(view.records[0].receipt.is_some()));
}

#[gpui::test]
fn typed_1478_parent_projects_manager_source_and_persists_correction_before_publication(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    cx.update(reader_features::enable_test_modules);
    let state = tempfile::tempdir().unwrap();
    let environment = Environment::new("fixture", "https://typed.invalid").unwrap();
    let mut owner = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = ProjectView::empty(Some(state.path().to_owned()), window, cx);
            view.binding = Some(binding("project", "thread"));
            view.chat = Some(Conversation::new("thread"));
            view.environment = Some(environment.clone());
            view.draft_restore(window, cx);
            view
        });
        let feed = view.read(cx).feed.clone();
        owner = Some(view);
        Root::new(feed, window, cx)
    });
    let view = owner.unwrap();
    visual.run_until_parked();
    view.update(visual, |view, cx| {
        let item = serde_json::from_value::<okilum_t3::TimelineItem>(serde_json::json!({"position":1,"itemId":"manager-decision","type":"assistant_message","status":"completed","text":"DEC: Full **source**\r\n🧠  ","updatedAt":"now"})).unwrap();
        let mut items = vec![item];
        items.extend((2..65).map(|position| serde_json::from_value::<okilum_t3::TimelineItem>(serde_json::json!({"position":position,"itemId":format!("later-{position}"),"type":"user_message","status":"completed","text":format!("Later message {position}\n\nRetained source"),"updatedAt":"now"})).unwrap()));
        view.feed.update(cx, |feed, cx| feed.apply(items.iter(), None, cx));
        view.typed_sync(cx);
    });
    visual.run_until_parked();
    let request = view.update(visual, |view, cx| {
        let scope = view
            .typed_scope()
            .expect("positive control: live scoped parent");
        assert!(view.typed.loaded);
        let expected = view.typed.messages[0].clone();
        assert!(
            expected.metadata.is_none(),
            "a DEC prefix in a reply does not invent classification"
        );
        assert_eq!(view.typed.sources[0].author, Author::Manager);
        assert_eq!(expected.text, "DEC: Full **source**\r\n🧠  ");
        let replacement = corrected_message_with_author(
            &view.typed.messages,
            &expected,
            MessageType::Decision,
            Some(Author::Manager),
        )
        .unwrap();
        let request = RetypeRequested {
            scope,
            expected,
            replacement,
        };
        view.typed_retype(&request, cx);
        assert!(
            view.typed.messages[0].metadata.is_none(),
            "projection cannot publish before durable acknowledgement"
        );
        request
    });
    visual.run_until_parked();
    view.update(visual, |view, cx| {
        view.typed_open_message(&request.scope, &request.replacement, cx)
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-message-1").is_some(),
        "source link scrolls the real virtualized feed without a reentrant parent borrow"
    );
    let (feed, source) = view.read_with(visual, |view, _| {
        (view.feed.clone(), view.typed.sources[0].clone())
    });
    feed.update(visual, |feed, cx| {
        let mut stale_scope = request.scope.clone();
        stale_scope.generation += 1;
        assert!(!feed.reveal_typed_source(&stale_scope, &source, cx));
        let missing = Source {
            id: "missing-source".into(),
            ..source.clone()
        };
        assert!(!feed.reveal_typed_source(&request.scope, &missing, cx));
        let changed = Source {
            text: "changed bytes".into(),
            ..source.clone()
        };
        assert!(!feed.reveal_typed_source(&request.scope, &changed, cx));
        assert!(
            feed.reveal_typed_source(&request.scope, &source, cx),
            "positive control: the exact owned source remains navigable"
        );
    });
    view.update(visual, |view, cx| {
        assert_eq!(view.typed.messages[0], request.replacement);
        assert_eq!(view.typed.messages[0].metadata.unwrap().decision_meaning(), Some(okilum_t3::message_types::DecisionMeaning::AwaitingAnswer));
        let saved = read(view.typed.store.as_ref().unwrap()).unwrap();
        assert_eq!(saved.1[0].message, request.expected.key.message);
        // Same ID, changed bytes: old annotation cannot relabel the new source.
        let item = serde_json::from_value::<okilum_t3::TimelineItem>(serde_json::json!({"position":1,"itemId":"manager-decision","type":"assistant_message","status":"completed","text":"replacement","updatedAt":"later"})).unwrap();
        view.feed.update(cx, |feed, cx| feed.apply(std::iter::once(&item), None, cx));
        view.typed_sync(cx);
        assert!(view.typed.messages[0].metadata.is_none());
        view.typed_retype(&request, cx);
        assert!(!view.typed.writing, "stale full-source correction is refused");
        assert_eq!(read(view.typed.store.as_ref().unwrap()).unwrap(), saved);
        view.native.connection_epoch += 1;
        assert!(view.typed_scope().is_none(), "connection reset revokes the old owner before a late callback can publish");
        view.typed_retype(&request, cx);
        assert!(view.typed.owner.is_none());
        assert!(!view.typed.writing);
    });
}
