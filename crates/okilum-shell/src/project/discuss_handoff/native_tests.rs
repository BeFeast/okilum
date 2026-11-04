use crate::project::*;
use ::core::prelude::v1::test;
use okilum_t3::{
    draft_handoff::{ComposerPort, Refusal, Request},
    stream::{NativeClient, NativeToken},
    NetworkAccess,
};

pub(in crate::project) fn client(
    environment: &Environment,
    access: NetworkAccess,
) -> Arc<NativeClient> {
    let token = NativeToken::from_secret(environment, &serde_json::to_vec(&serde_json::json!({
        "origin":environment.origin(), "access_token":"offline-draft-fixture", "expires_at":4070908800u64,
        "scope":"orchestration:read orchestration:operate"
    })).unwrap()).unwrap();
    Arc::new(
        NativeClient::new(environment.clone(), token, Duration::from_secs(1))
            .unwrap()
            .with_access(access),
    )
}

pub(in crate::project) fn view(
    state: &Path,
    window: &mut Window,
    cx: &mut Context<ProjectView>,
) -> ProjectView {
    view_with_draft(state, "", window, cx)
}

pub(in crate::project) fn view_with_draft(
    state: &Path,
    text: &str,
    window: &mut Window,
    cx: &mut Context<ProjectView>,
) -> ProjectView {
    reader_features::enable_test_modules(cx);
    let mut view = ProjectView::empty(Some(state.to_owned()), window, cx);
    view.area = Some(state.to_owned());
    view.binding = AreaBinding::parse("t3:\n  schema_version: 1\n  environment: fixture\n  manager: {project_id: project, thread_id: thread, workspace_mode: local}\n  executor: {project_id: executors}\n  default_models:\n    manager: {instanceId: codex, model: gpt-6.1-sol}\n    executor: {instanceId: codex, model: gpt-6.1-sol}\n").unwrap();
    view.chat = Some(Conversation::new("thread"));
    view.environment = Some(Environment::new("fixture", "https://discuss.invalid").unwrap());
    let native = client(view.environment.as_ref().unwrap(), NetworkAccess::new(true));
    view.native.set_detail_test_client(native);
    view.compose
        .update(cx, |input, cx| input.set_value(text, window, cx));
    view.draft_restore(window, cx);
    view
}

#[gpui::test]
fn discuss_1784_token_only_draft_uses_full_source_separator_and_one_undo(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let state = tempfile::tempdir().unwrap();
    let mut owner = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let project = cx.new(|cx| view(state.path(), window, cx));
        let composer = project.read(cx).composer.clone();
        owner = Some(project);
        Root::new(composer, window, cx)
    });
    let project = owner.unwrap();
    visual.run_until_parked();
    let metadata = project.update_in(visual, |view, window, cx| {
        let mut draft = message_types::TypedDraft::from_text("FR ".into());
        assert!(draft.detect_token());
        let metadata = draft.retained_metadata();
        view.composer.update(cx, |composer, cx| {
            composer.set_controls(true, false, cx);
            composer
                .replace_typed_draft(draft.text, Some(metadata.clone()), window, cx)
                .unwrap();
        });
        view.draft_handoff_surface_visible(true, cx);
        assert!(
            view.compose.read(cx).value().is_empty(),
            "positive control: source exists behind an empty token body"
        );
        view.append_board_context(state.path(), "Exact context 🧠".into(), window, cx)
            .unwrap();
        assert_eq!(
            view.composer.read(cx).typed_source_text(cx),
            "FR \n\n---\nExact context 🧠"
        );
        assert_eq!(view.composer.read(cx).typed_draft_metadata(), metadata);
        assert!(view.records.is_empty());
        metadata
    });
    visual.run_until_parked();
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    });
    visual.run_until_parked();
    project.read_with(visual, |view, cx| {
        assert_eq!(view.composer.read(cx).typed_source_text(cx), "FR ");
        assert_eq!(view.composer.read(cx).typed_draft_metadata(), metadata);
        assert!(view.records.is_empty());
    });
    project.read_with(visual, |view, _| {
        let env = view.environment.as_ref().unwrap();
        assert!(
            okilum_t3::chat::MessageJournal::new(state.path(), env, "thread")
                .records()
                .unwrap()
                .is_empty()
        );
        assert!(
            okilum_t3::actions::Journal::new(state.path(), env, "thread")
                .records()
                .unwrap()
                .is_empty()
        );
    });
}

#[gpui::test]
fn discuss_1784_visibility_repaint_reconnect_and_revocation_fence_actual_port(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let mut owner = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let project = cx.new(|cx| view(state.path(), window, cx));
        let composer = project.read(cx).composer.clone();
        owner = Some(project);
        Root::new(composer, window, cx)
    });
    let project = owner.unwrap();
    visual.run_until_parked();
    project.update_in(visual, |view, window, cx| {
        view.compose
            .update(cx, |input, cx| input.set_value("Existing\n", window, cx));
        assert!(
            view.draft_handoff_target(cx).is_none(),
            "hidden native instance is not a supported fallback"
        );
        view.draft_handoff_surface_visible(true, cx);
        let first = view.draft_handoff_target(cx).unwrap();
        let old = Request {
            id: 1,
            target: first.clone(),
            text: "old context".into(),
        };
        for _ in 0..3 {
            view.draft_handoff_surface_visible(true, cx);
        }
        assert_eq!(
            view.draft_handoff_target(cx),
            Some(first.clone()),
            "identical render visibility preserves identity"
        );
        view.draft_handoff_surface_visible(false, cx);
        assert!(view.draft_handoff_port(window, cx).append(&old).is_err());
        view.draft_handoff_surface_visible(true, cx);
        let reopened = view.draft_handoff_target(cx).unwrap();
        assert_ne!(reopened.generation, first.generation);
        assert!(view.draft_handoff_port(window, cx).append(&old).is_err());
        let pending = Request {
            id: 2,
            target: reopened,
            text: "previous connection".into(),
        };
        view.native.connection_epoch += 1;
        let reconnected = view.draft_handoff_target(cx).unwrap();
        assert_ne!(reconnected.generation, pending.target.generation);
        assert!(view
            .draft_handoff_port(window, cx)
            .append(&pending)
            .is_err());
        view.select_section(ProjectSection::Rules, cx);
        assert!(view.draft_handoff_target(cx).is_none());
        view.select_section(ProjectSection::Manager, cx);
        let returned = view.draft_handoff_target(cx).unwrap();
        assert_ne!(
            returned.generation, reconnected.generation,
            "same physical panel after Rules → Manager needs a fresh target"
        );
        let stale_section = Request {
            id: 3,
            target: reconnected,
            text: "hidden section".into(),
        };
        assert!(view
            .draft_handoff_port(window, cx)
            .append(&stale_section)
            .is_err());
        let current = Request {
            id: 3,
            target: returned,
            text: "current context".into(),
        };
        view.draft_handoff_port(window, cx)
            .append(&current)
            .unwrap();
        view.draft_handoff_port(window, cx)
            .append(&current)
            .unwrap();
        assert_eq!(
            view.composer.read(cx).typed_source_text(cx),
            "Existing\n\n\ncurrent context",
            "replay inserts once with exact existing whitespace"
        );
        let current_client = view.native.model_client().unwrap();
        view.threads.handoff_revoked = Some(current_client);
        assert!(view.draft_handoff_target(cx).is_none());
        assert!(view
            .draft_handoff_port(window, cx)
            .append(&current)
            .is_err());
        view.threads.handoff_revoked = None;
        let access = NetworkAccess::new(true);
        let replacement = client(view.environment.as_ref().unwrap(), access.clone());
        view.native.set_detail_test_client(replacement);
        view.native.connection_epoch += 1;
        view.draft_handoff_connection_ready(cx);
        let next = Request {
            id: 4,
            target: view.draft_handoff_target(cx).unwrap(),
            text: "revoked".into(),
        };
        access.revoke();
        assert!(view.draft_handoff_port(window, cx).append(&next).is_err());
        assert_eq!(
            view.composer.read(cx).typed_source_text(cx),
            "Existing\n\n\ncurrent context"
        );
        assert_eq!(
            view.append_board_context(&state.path().join("foreign"), "wrong".into(), window, cx),
            Err(Refusal::WrongTarget)
        );
    });
    assert!(!state.path().join("t3-messages").exists());
}

#[gpui::test]
fn discuss_1784_restore_completes_after_visibility_but_hidden_late_restore_stays_revoked(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let environment = Environment::new("fixture", "https://discuss.invalid").unwrap();
    for name in ["visible", "hidden"] {
        okilum_t3::chat_drafts::Store::<okilum_t3::chat_drafts::Attachment>::new(
            &state.path().join(name),
            &environment,
            "project",
            "thread",
        )
        .save(
            0,
            okilum_t3::chat_drafts::Draft {
                text: format!("retained {name}\r\n🧠"),
                attachments: Vec::new(),
            },
        )
        .unwrap();
    }
    let mut visible_owner = None;
    let (_, visible_context) = cx.add_window_view(|window, cx| {
        let project = cx.new(|cx| {
            let mut project = view(&state.path().join("visible"), window, cx);
            project.draft_handoff_surface_visible(true, cx);
            assert!(
                project.draft_handoff_target(cx).is_none(),
                "loading cannot own a port even after physical visibility arrives"
            );
            project
        });
        let composer = project.read(cx).composer.clone();
        visible_owner = Some(project);
        Root::new(composer, window, cx)
    });
    let project = visible_owner.unwrap();
    visible_context.run_until_parked();
    project.update_in(visible_context, |view, window, cx| {
        assert!(
            view.draft_handoff_target(cx).is_some(),
            "current restore callback enables the already visible composer without hide/show"
        );
        assert_eq!(
            view.composer.read(cx).typed_source_text(cx),
            "retained visible\r\n🧠"
        );
        view.append_board_context(
            &state.path().join("visible"),
            "after restore".into(),
            window,
            cx,
        )
        .unwrap();
    });
    let mut hidden_owner = None;
    let (_, hidden_context) = cx.add_window_view(|window, cx| {
        let project = cx.new(|cx| {
            let mut project = view(&state.path().join("hidden"), window, cx);
            project.draft_handoff_surface_visible(true, cx);
            project.set_active(false, window, cx);
            project
        });
        let composer = project.read(cx).composer.clone();
        hidden_owner = Some(project);
        Root::new(composer, window, cx)
    });
    let hidden = hidden_owner.unwrap();
    hidden_context.run_until_parked();
    hidden.update_in(hidden_context, |view, window, cx| {
        assert!(
            view.draft_handoff_target(cx).is_none(),
            "late restore cannot re-enable a hidden or revoked visible lease"
        );
        view.active = true;
        view.draft_restore(window, cx);
        assert!(
            view.draft_handoff_target(cx).is_some(),
            "explicit reopen restores the suspended exact owner"
        );
        assert_eq!(
            view.composer.read(cx).typed_source_text(cx),
            "retained hidden\r\n🧠"
        );
    });
    let failed_state = state.path().join("failed");
    std::fs::write(&failed_state, b"unavailable draft storage").unwrap();
    let mut failed_owner = None;
    let (_, failed_context) = cx.add_window_view(|window, cx| {
        let project = cx.new(|cx| {
            let mut project = view_with_draft(&failed_state, "keep unsaved original", window, cx);
            project.draft_handoff_surface_visible(true, cx);
            project
        });
        let composer = project.read(cx).composer.clone();
        failed_owner = Some(project);
        Root::new(composer, window, cx)
    });
    let failed = failed_owner.unwrap();
    failed_context.run_until_parked();
    failed.update_in(failed_context, |view, window, cx| {
        assert!(
            view.draft_notice().is_some(),
            "positive control: the actual restore failed visibly"
        );
        assert!(view.draft_handoff_target(cx).is_none());
        view.select_section(ProjectSection::Rules, cx);
        view.select_section(ProjectSection::Manager, cx);
        assert!(
            view.draft_handoff_target(cx).is_none(),
            "section return cannot mask a failed restore"
        );
        assert!(view
            .append_board_context(&failed_state, "must not append".into(), window, cx)
            .is_err());
        assert_eq!(
            view.composer.read(cx).typed_source_text(cx),
            "keep unsaved original"
        );
    });
    assert_eq!(
        std::fs::read(&failed_state).unwrap(),
        b"unavailable draft storage"
    );
}
