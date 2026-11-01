use super::super::question_progress_tests as harness;
use super::*;
use ::core::prelude::v1::test;

fn profile() -> NativeEnvironmentProfile {
    NativeEnvironmentProfile {
        identity: ServerIdentity {
            environment_id: "confirmed-environment".into(),
            label: "Fixture".into(),
        },
        capabilities: NativeEnvironmentCapabilities {
            question_attachments: Some(true),
            attachment_uploads: Some(true),
            file_attachment_max_upload_bytes: Some(okilum_t3::stream::MAX_FILE_BYTES),
        },
    }
}
fn entry(id: &str, state: AttachmentState) -> Entry {
    Entry {
        row: Attachment {
            id: id.into(),
            kind: DraftKind::Image,
            name: "Question.png".into(),
            mime_type: "image/png".into(),
            size_bytes: 68,
            source: None,
            state,
        },
        source: Source::Image(Arc::new(vec![1, 2, 3])),
        prepared: None,
        environment: "confirmed-environment".into(),
        request_row: json!("wizard-row"),
        revision: uuid::Uuid::new_v4(),
        job: None,
    }
}
fn uploaded(id: &str) -> AttachmentState {
    AttachmentState::Uploaded {
        attachment_id: id.into(),
        environment_id: "confirmed-environment".into(),
    }
}
fn request(custom: bool) -> Request {
    Request {
        id: "wizard".into(),
        item: json!({"id":"wizard-row","requestId":"wizard","threadId":"mcp:fixture-manager","type":"user_input_request","status":"pending","questions":[
            {"id":"one","question":"First answer","options":[]},
            {"id":"two","question":"Second answer","allowCustomAnswer":custom,"options":[{"label":"Keep exact","value":" exact "}]}
        ]}),
        answerable: true,
        dismissible: true,
        response_mode: Some("message".into()),
        reason: None,
    }
}

#[test]
fn references_require_all_rows_uploaded_under_the_confirmed_environment_and_aggregate_limit() {
    let mut runtime = Runtime {
        profile: Some(profile()),
        ..Default::default()
    };
    let key = ("wizard".into(), "two".into());
    runtime
        .entries
        .insert(key.clone(), vec![entry("local", AttachmentState::Pending)]);
    assert!(runtime.references("wizard", None).is_err());
    runtime.entries.get_mut(&key).unwrap()[0].row.state = AttachmentState::Error {
        message: "Upload failed".into(),
    };
    assert!(runtime.references("wizard", None).is_err());
    runtime.entries.get_mut(&key).unwrap()[0].row.state = uploaded("asset-two");
    let refs = runtime.references("wizard", None).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs["two"][0].id, "asset-two");
    assert!(!refs.contains_key("one"));
    runtime.entries.get_mut(&key).unwrap()[0].environment = "foreign-environment".into();
    assert!(runtime.references("wizard", None).is_err());
    runtime.entries.get_mut(&key).unwrap()[0].environment = "confirmed-environment".into();
    runtime.entries.get_mut(&key).unwrap()[0].row.state = AttachmentState::Uploaded {
        attachment_id: "asset-two".into(),
        environment_id: "foreign-environment".into(),
    };
    assert!(runtime.references("wizard", None).is_err());
    runtime.entries.insert(
        key.clone(),
        (0..4)
            .map(|n| {
                entry(
                    &format!("local-two-{n}"),
                    uploaded(&format!("asset-two-{n}")),
                )
            })
            .collect(),
    );
    runtime.entries.insert(
        ("wizard".into(), "one".into()),
        (0..4)
            .map(|n| {
                entry(
                    &format!("local-one-{n}"),
                    uploaded(&format!("asset-one-{n}")),
                )
            })
            .collect(),
    );
    assert!(
        runtime.references("wizard", None).is_ok(),
        "positive aggregate eight-file control"
    );
    runtime
        .entries
        .get_mut(&key)
        .unwrap()
        .push(entry("ninth", uploaded("asset-nine")));
    assert!(
        runtime.references("wizard", None).is_err(),
        "limits span all questions, not eight per question"
    );
}

#[test]
fn unsynchronized_state_keeps_files_but_replacement_row_and_resolution_do_not_rebind_them() {
    let mut runtime = Runtime {
        profile: Some(profile()),
        ..Default::default()
    };
    runtime.entries.insert(
        ("wizard".into(), "two".into()),
        vec![entry("local", uploaded("asset-two"))],
    );
    runtime.reconcile(&State::default());
    assert!(runtime
        .references("wizard", None)
        .unwrap()
        .contains_key("two"));
    let canonical = request(true);
    runtime.reconcile(&State {
        ready: true,
        requests: vec![canonical.clone()],
        ..Default::default()
    });
    assert!(runtime
        .references("wizard", None)
        .unwrap()
        .contains_key("two"));
    let mut replacement = canonical;
    replacement.item["id"] = json!("new-row-with-reused-request-and-question-ids");
    runtime.reconcile(&State {
        ready: true,
        requests: vec![replacement],
        ..Default::default()
    });
    assert!(runtime.entries.is_empty());
    runtime.entries.insert(
        ("wizard".into(), "two".into()),
        vec![entry("other", uploaded("asset-other"))],
    );
    runtime.reconcile(&State {
        ready: true,
        ..Default::default()
    });
    assert!(runtime.entries.is_empty());
}

#[gpui::test]
fn pending_question_files_block_submit_then_only_q2_references_and_empty_answer_reach_real_rpc(
    cx: &mut TestAppContext,
) {
    let (environment, wire, server) = harness::rpc(true);
    let (view, visual, _temp, _, _, _) = harness::fixture(cx, Some(environment));
    harness::show(&view, visual, request(true));
    view.update_in(visual, |view, window, cx| {
        view.compose.update(cx, |input, cx| {
            input.set_value("independent composer marker", window, cx)
        });
    });
    harness::click(visual, "answer-custom-wizard-one");
    visual.simulate_input(" first exact answer ");
    visual.run_until_parked();
    harness::click(visual, "question-next-wizard");
    harness::click(visual, "answer-custom-wizard-two");
    visual.simulate_input("valid text cannot bypass a pending file");
    visual.run_until_parked();
    let draft = harness::input(&view, visual, "two");
    view.update(visual, |view, cx| {
        view.native.actions.update(cx, |actions, cx| {
            assert!(
                actions.question_files.context.is_some(),
                "positive current origin/project/thread/lease/client scope"
            );
            actions.question_files.profile = Some(profile());
            actions.question_files.entries.insert(
                ("wizard".into(), "two".into()),
                vec![entry("local-two", AttachmentState::Pending)],
            );
            assert!(!actions.question_complete("wizard", "two", cx));
            assert!(
                actions.state.allows(&actions.intent("wizard", cx)),
                "positive text authority control: the additional file-readiness gate is necessary"
            );
            cx.notify();
        });
    });
    visual.run_until_parked();
    harness::click(visual, "respond-wizard");
    assert!(wire.lock().unwrap().is_empty());
    view.update(visual, |view, cx| {
        view.native.actions.update(cx, |actions, cx| {
            actions
                .question_files
                .entries
                .get_mut(&("wizard".into(), "two".into()))
                .unwrap()[0]
                .row
                .state = AttachmentState::Error {
                message: "retry needed".into(),
            };
            assert!(!actions.question_complete("wizard", "two", cx));
            assert_eq!(actions.deferred_dispatches.get(), 0);
            cx.notify();
        });
    });
    view.update_in(visual, |view, window, cx| {
        draft.update(cx, |input, cx| input.set_value("", window, cx));
        view.native.actions.update(cx, |actions, cx| {
            actions
                .question_files
                .entries
                .get_mut(&("wizard".into(), "two".into()))
                .unwrap()[0]
                .row
                .state = uploaded("server-q2-image");
            assert!(actions.question_complete("wizard", "two", cx));
            cx.notify();
        });
    });
    visual.run_until_parked();
    harness::tick(visual, 500);
    assert!(
        wire.lock().unwrap().is_empty(),
        "file completion never auto-submits an attachment-only answer"
    );
    harness::click(visual, "respond-wizard");
    server.join().unwrap();
    let commands = wire.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["type"], "runtime-request.respond");
    assert_eq!(
        commands[0]["answers"],
        json!({"one":" first exact answer ","two":""})
    );
    assert!(commands[0]["attachmentsByQuestionId"].get("one").is_none());
    assert_eq!(
        commands[0]["attachmentsByQuestionId"]["two"][0]["id"],
        "server-q2-image"
    );
    assert_eq!(commands[0]["threadId"], "mcp:fixture-manager");
    view.read_with(visual, |view, cx| {
        let actions = view.native.actions.read(cx);
        assert_eq!(actions.records.len(), 1);
        assert_eq!(actions.records[0].sequence, Some(1));
        assert_eq!(
            actions.records[0].command.command_id,
            commands[0]["commandId"].as_str().unwrap()
        );
        assert_eq!(
            view.compose.read(cx).value().as_str(),
            "independent composer marker"
        );
    });
    drop(commands);
    harness::click(visual, "respond-wizard");
    assert_eq!(
        wire.lock().unwrap().len(),
        1,
        "confirmed retention never creates a fresh answer/upload"
    );
}

#[gpui::test]
fn choice_required_question_allows_files_with_exact_selection_but_not_attachment_only(
    cx: &mut TestAppContext,
) {
    let (view, visual, _temp, _, _, _) = harness::fixture(cx, None);
    harness::show(&view, visual, request(false));
    view.update(visual, |view, cx| {
        view.native.actions.update(cx, |actions, cx| {
            actions.question_files.profile = Some(profile());
            actions.question_files.entries.insert(
                ("wizard".into(), "two".into()),
                vec![entry("local-two", uploaded("choice-image"))],
            );
            assert!(
                !actions.question_complete("wizard", "two", cx),
                "files do not bypass the required provider choice"
            );
            actions
                .selections
                .insert(("wizard".into(), "two".into()), vec![" exact ".into()]);
            assert!(actions.question_complete("wizard", "two", cx));
            let control = QuestionControl {
                conversation: actions.conversation.clone(),
                lease: actions.lease.clone(),
                transition: actions.question_progress["wizard"]
                    .transition("wizard")
                    .unwrap(),
            };
            actions.question_progress.get_mut("wizard").unwrap().next();
            let control = QuestionControl {
                transition: actions.question_progress["wizard"]
                    .transition("wizard")
                    .unwrap(),
                ..control
            };
            assert!(
                actions.question_file_allowed(&control),
                "custom=false still allows files alongside the provider choice"
            );
            let Intent::Respond {
                answers: Some(answers),
                attachments_by_question_id,
                ..
            } = actions.intent("wizard", cx)
            else {
                panic!("response");
            };
            assert_eq!(answers["two"], json!(" exact "));
            assert_eq!(attachments_by_question_id["two"][0].id, "choice-image");
            cx.notify();
        });
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("question-attach-wizard-two").is_some());
    assert!(visual.debug_bounds("answer-custom-wizard-two").is_none());
}

#[gpui::test]
fn upload_publication_rejects_old_job_scope_question_revision_and_confirmed_environment(
    cx: &mut TestAppContext,
) {
    let (view, visual, _temp, _, _, _) = harness::fixture(cx, None);
    harness::show(&view, visual, request(true));
    view.update(visual, |view, cx| {
        view.native.actions.update(cx, |actions, cx| {
            actions.question_files.profile = Some(profile());
            let key = ("wizard".into(), "two".into());
            let mut pending = entry("local-two", AttachmentState::Pending);
            let job_id = uuid::Uuid::new_v4();
            pending.job = Some(job_id);
            let job = UploadJob {
                context: actions.question_files.context.clone().unwrap(),
                environment: "confirmed-environment".into(),
                epoch: actions.question_files.epoch,
                key: key.clone(),
                row: json!("wizard-row"),
                question: request(true).item["questions"][1].clone(),
                id: pending.row.id.clone(),
                revision: pending.revision,
                job: job_id,
            };
            actions.question_files.entries.insert(key, vec![pending]);
            assert!(
                actions.question_upload_current(&job),
                "positive current upload publication control"
            );
            let mut stale = job.clone();
            stale.context.origin = "http://another-origin.test".into();
            assert!(!actions.question_upload_current(&stale));
            stale = job.clone();
            stale.context.project = "other-project".into();
            assert!(!actions.question_upload_current(&stale));
            stale = job.clone();
            stale.context.thread = "other-thread".into();
            assert!(!actions.question_upload_current(&stale));
            stale = job.clone();
            stale.context.lease = Arc::new(());
            assert!(!actions.question_upload_current(&stale));
            stale = job.clone();
            stale.row = json!("replacement-row");
            assert!(!actions.question_upload_current(&stale));
            stale = job.clone();
            stale.question["question"] = json!("Changed question");
            assert!(!actions.question_upload_current(&stale));
            stale = job.clone();
            stale.revision = uuid::Uuid::new_v4();
            assert!(!actions.question_upload_current(&stale));
            stale = job.clone();
            stale.job = uuid::Uuid::new_v4();
            assert!(!actions.question_upload_current(&stale));
            assert!(actions.question_upload_current(&job));
            let mut context = job.context.clone();
            context.confirmed_environment = Some("other-confirmed-environment".into());
            actions.question_files.set_context(Some(context));
            assert!(
                actions.question_files.profile.is_none(),
                "same-origin profile cannot override an independently confirmed native environment"
            );
            assert!(!actions.question_upload_current(&job));
            assert!(actions.question_files.references("wizard", None).is_err());
            assert!(
                actions
                    .question_files
                    .entries
                    .values()
                    .flatten()
                    .all(|entry| entry.environment == "confirmed-environment"),
                "old sources never acquire the replacement environment identity"
            );
            cx.notify();
        });
    });
}
