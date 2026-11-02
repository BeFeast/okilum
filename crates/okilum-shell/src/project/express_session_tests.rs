use super::*;
use ::core::prelude::v1::test;
use std::sync::atomic::AtomicUsize;

const CHAT_DRAFT: &str = "FR Keep independent 🧠\r\n  source  \n";
fn init(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
}
fn fixture_view(
    state: &Path,
    gateway: Option<Arc<fixture::Fake>>,
    window: &mut Window,
    cx: &mut Context<ProjectView>,
) -> ProjectView {
    reader_features::enable_test_modules(cx);
    let mut view = ProjectView::empty(Some(state.into()), window, cx);
    let area = state.join("source");
    std::fs::create_dir_all(area.join("ops")).unwrap();
    let binding = AreaBinding::parse("t3:\n  schema_version: 1\n  environment: fixture\n  manager: {project_id: fixture-managers, thread_id: 'mcp:fixture-manager', workspace_mode: local}\n  executor: {project_id: fixture-executors}\n  default_models:\n    manager: {instanceId: codex, model: gpt-6.1-sol}\n    executor: {instanceId: codex, model: gpt-6.1-sol}\n").unwrap().unwrap();
    std::fs::write(
        area.join("ops/project.yaml"),
        format!(
            "name: Offline Express\nkind: code\nt3:\n{}",
            serde_yaml::to_string(&binding)
                .unwrap()
                .lines()
                .map(|line| format!("  {line}\n"))
                .collect::<String>()
        ),
    )
    .unwrap();
    view.area = Some(area);
    view.binding = Some(binding);
    view.chat = Some(Conversation::new("mcp:fixture-manager"));
    view.environment = Some(Environment::new("fixture", "https://express.invalid").unwrap());
    view.identity = Some(ServerIdentity {
        environment_id: "environment:fixture".into(),
        label: "Offline".into(),
    });
    let client = discuss_handoff::native_tests::client(
        view.environment.as_ref().unwrap(),
        okilum_t3::NetworkAccess::new(true),
    );
    view.native.set_detail_test_client(client);
    view.gateway = gateway.map(|gateway| gateway as Arc<dyn ManagerGateway>);
    view.compose
        .update(cx, |input, cx| input.set_value(CHAT_DRAFT, window, cx));
    view.draft_restore(window, cx);
    view
}
fn ready(view: &mut ProjectView, cx: &mut Context<ProjectView>) {
    view.typed_sync(cx);
    view.draft_handoff_surface_visible(true, cx);
    assert!(
        OwnerTicket::capture(view, cx).is_ok(),
        "positive control: real typed owner is ready"
    );
}
fn active(view: &ProjectView) -> Entity<Form> {
    view.express.forms[view.express.active.unwrap()]
        .form
        .clone()
}
fn draft() -> Draft {
    Draft {
        goal: "Urgent offline fixture 🧠".into(),
        recipient: "Explicit recipient".into(),
        platforms: vec![Platform::Linux],
        source: None,
    }
}
fn seed(
    view: &ProjectView,
    gateway: &fixture::Fake,
    cx: &App,
) -> (SessionTarget, SessionSelection) {
    let owner = OwnerTicket::capture(view, cx).unwrap();
    let journal = view.journal().unwrap();
    let target = owner.target;
    let request = Request::prepare(
        gateway,
        &target.manager_project_id,
        &target.manager_thread_id,
        draft(),
    )
    .unwrap();
    journal.prepare(&request.message().unwrap()).unwrap();
    let page = model::list_retained(
        gateway,
        &journal,
        &owner.state,
        view.environment.as_ref().unwrap(),
        &target,
        None,
    )
    .unwrap();
    let selection = page.rows[0].selection.clone().unwrap();
    (target, selection)
}

#[gpui::test]
async fn express_1493_stale_menu_same_entity_rebind_and_client_refuse_fresh_route_opens(
    cx: &mut TestAppContext,
) {
    init(cx);
    let state = tempfile::tempdir().unwrap();
    let mut owner = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let project = cx.new(|cx| fixture_view(state.path(), None, window, cx));
        let composer = project.read(cx).composer.clone();
        owner = Some(project);
        Root::new(composer, window, cx)
    });
    let project = owner.unwrap();
    visual.run_until_parked();
    project.update_in(visual, |view, window, cx| {
        ready(view, cx);
        let stale = OwnerTicket::capture(view, cx).unwrap();
        view.draft_handoff_connection_changing();
        view.generation += 1;
        ready(view, cx);
        stale.open(view, window, cx);
        assert!(!view.express.is_open());
        assert!(view.error.is_some());
        let stale = OwnerTicket::capture(view, cx).unwrap();
        let replacement = discuss_handoff::native_tests::client(
            view.environment.as_ref().unwrap(),
            okilum_t3::NetworkAccess::new(true),
        );
        view.native.set_detail_test_client(replacement);
        stale.open(view, window, cx);
        assert!(!view.express.is_open());
        let hidden_menu = OwnerTicket::capture(view, cx).unwrap();
        view.draft_handoff_surface_visible(false, cx);
        ready(view, cx);
        hidden_menu.open(view, window, cx);
        assert!(
            !view.express.is_open(),
            "hide/show cannot revive a menu captured with no form open"
        );
        let fresh = OwnerTicket::capture(view, cx).unwrap();
        fresh.open(view, window, cx);
        assert!(
            view.express.is_open(),
            "positive control: exact fresh menu opens the actual retained form"
        );
    });
    visual
        .condition(&project, |view, _| view.express.pending.is_none())
        .await;
    project.read_with(visual, |view, cx| {
        assert!(
            active(view).read(cx).selection_revision.is_some(),
            "local restore succeeded with NO gateway available"
        );
        assert_eq!(view.composer.read(cx).typed_source_text(cx), CHAT_DRAFT);
        assert!(view.journal().unwrap().records().unwrap().is_empty());
        assert!(
            !state.path().join("t3-express-selection").exists(),
            "read must not publish a choice"
        );
    });
}

#[gpui::test]
async fn express_1493_local_restore_exact_saved_operation_has_no_gateway_or_resend(
    cx: &mut TestAppContext,
) {
    init(cx);
    let state = tempfile::tempdir().unwrap();
    let fake = Arc::new(fixture::Fake::default());
    let mut owner = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let project = cx.new(|cx| fixture_view(state.path(), None, window, cx));
        let composer = project.read(cx).composer.clone();
        owner = Some(project);
        Root::new(composer, window, cx)
    });
    let project = owner.unwrap();
    visual.run_until_parked();
    let (selection, reads) = project.update_in(visual, |view, window, cx| {
        ready(view, cx);
        let (target, selection) = seed(view, &fake, cx);
        let store = SelectionStore::new(state.path(), &target).unwrap();
        store
            .remember(
                &view.journal().unwrap(),
                view.environment.as_ref().unwrap(),
                &store.load().unwrap().revision,
                &selection,
            )
            .unwrap();
        let reads = fake.reads.lock().unwrap().len();
        view.open_express_session(window, cx);
        (selection, reads)
    });
    visual
        .condition(&project, |view, _| view.express.pending.is_none())
        .await;
    project.read_with(visual, |view, cx| {
        let chosen = active(view).read(cx).selected_session().unwrap();
        assert_eq!(chosen.selection, selection);
        assert_eq!(
            view.express.forms[view.express.active.unwrap()]
                .recovered
                .as_ref()
                .unwrap()
                .record
                .message
                .client_request_id,
            selection.operation_id()
        );
        assert!(view.gateway.is_none());
        assert_eq!(fake.reads.lock().unwrap().len(), reads);
        assert!(fake.sends.lock().unwrap().is_empty());
        assert_eq!(view.composer.read(cx).typed_source_text(cx), CHAT_DRAFT);
    });
}

#[gpui::test]
async fn express_1493_explicit_chooser_remember_reconcile_forget_serial_handlers(
    cx: &mut TestAppContext,
) {
    init(cx);
    let state = tempfile::tempdir().unwrap();
    let fake = Arc::new(fixture::Fake::default());
    let mut owner = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let project = cx.new(|cx| fixture_view(state.path(), Some(fake.clone()), window, cx));
        let composer = project.read(cx).composer.clone();
        owner = Some(project);
        Root::new(composer, window, cx)
    });
    let project = owner.unwrap();
    visual.run_until_parked();
    let selection = project.update_in(visual, |view, window, cx| {
        ready(view, cx);
        let (_, selection) = seed(view, &fake, cx);
        view.open_express_session(window, cx);
        selection
    });
    visual
        .condition(&project, |view, _| view.express.pending.is_none())
        .await;
    let reads = fake.reads.lock().unwrap().len();
    project.update_in(visual, |view, _, cx| {
        let form = active(view);
        let target = view.express.forms[view.express.active.unwrap()]
            .target
            .clone();
        form.update(cx, |form, cx| form.begin_session_chooser(target, cx));
    });
    visual
        .condition(&project, |view, cx| {
            view.express.pending.is_none() && active(view).read(cx).session_page.is_some()
        })
        .await;
    assert!(
        fake.reads.lock().unwrap().len() > reads,
        "positive control: explicit chooser actually reads the fake gateway"
    );
    project.update_in(visual, |view, _, cx| {
        active(view).update(cx, |form, cx| form.choose_session(selection.clone(), cx))
    });
    visual
        .condition(&project, |view, cx| {
            view.express.pending.is_none() && active(view).read(cx).request_id.is_some()
        })
        .await;
    project.update_in(visual, |view, window, cx| {
        assert_eq!(
            active(view).read(cx).selected_session().unwrap().selection,
            selection
        );
        active(view).update(cx, |form, cx| form.resume_draft(window, cx));
    });
    visual
        .condition(&project, |view, cx| {
            view.express.pending.is_none() && !active(view).read(cx).locked
        })
        .await;
    project.read_with(visual, |view, cx| {
        let target = &view.express.forms[view.express.active.unwrap()].target;
        assert!(SelectionStore::new(state.path(), target)
            .unwrap()
            .load()
            .unwrap()
            .selection
            .is_none());
        assert_eq!(
            view.journal().unwrap().records().unwrap().len(),
            1,
            "forget retains the original intent"
        );
        assert_eq!(view.composer.read(cx).typed_source_text(cx), CHAT_DRAFT);
        assert!(fake.sends.lock().unwrap().is_empty());
    });
}

#[gpui::test]
async fn express_1493_close_hide_rebind_refuse_queued_completion_with_fresh_positive_control(
    cx: &mut TestAppContext,
) {
    init(cx);
    for action in ["close", "hide", "rebind"] {
        let state = tempfile::tempdir().unwrap();
        let mut owner = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let project = cx.new(|cx| fixture_view(state.path(), None, window, cx));
            let composer = project.read(cx).composer.clone();
            owner = Some(project);
            Root::new(composer, window, cx)
        });
        let project = owner.unwrap();
        visual.run_until_parked();
        project.update_in(visual, |view, window, cx| {
            ready(view, cx);
            view.open_express_session(window, cx);
        });
        visual
            .condition(&project, |view, _| view.express.pending.is_none())
            .await;
        let applied = Arc::new(AtomicUsize::new(0));
        let late = applied.clone();
        project.update_in(visual, |view, window, cx| {
            let form = active(view);
            view.express_work(
                form,
                window,
                cx,
                |_| Ok(()),
                move |_, _, _, _, _| {
                    late.fetch_add(1, Ordering::SeqCst);
                },
            );
            match action {
                "close" => view.express.revoke(),
                "hide" => view.draft_handoff_surface_visible(false, cx),
                _ => {
                    view.draft_handoff_connection_changing();
                    view.generation += 1;
                }
            }
            cx.notify();
        });
        visual
            .condition(&project, |view, _| view.express.pending.is_none())
            .await;
        assert_eq!(applied.load(Ordering::SeqCst), 0);
        let fresh = applied.clone();
        project.update_in(visual, |view, window, cx| {
            ready(view, cx);
            view.open_express_session(window, cx);
        });
        visual
            .condition(&project, |view, _| view.express.pending.is_none())
            .await;
        project.update_in(visual, |view, window, cx| {
            let epoch = view.express.epoch;
            view.draft_handoff_surface_visible(true, cx);
            view.draft_handoff_surface_visible(true, cx);
            assert_eq!(
                view.express.epoch, epoch,
                "identical visibility cannot mint Express authority"
            );
            let form = active(view);
            view.express_work(
                form,
                window,
                cx,
                |_| Ok(()),
                move |_, _, result, _, _| {
                    result.unwrap();
                    fresh.fetch_add(1, Ordering::SeqCst);
                },
            );
        });
        visual
            .condition(&project, |view, _| view.express.pending.is_none())
            .await;
        assert_eq!(
            applied.load(Ordering::SeqCst),
            1,
            "positive control: unchanged current owner receives its completion"
        );
        project.read_with(visual, |view, cx| {
            assert_eq!(view.composer.read(cx).typed_source_text(cx), CHAT_DRAFT);
            assert!(view.journal().unwrap().records().unwrap().is_empty());
        });
    }
}

#[gpui::test]
async fn express_1493_actual_form_annotation_failure_keeps_draft_and_uncertain_retry_identity(
    cx: &mut TestAppContext,
) {
    init(cx);
    let state = tempfile::tempdir().unwrap();
    let fake = Arc::new(fixture::Fake {
        fail_first: true,
        ..Default::default()
    });
    let mut owner = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let project = cx.new(|cx| fixture_view(state.path(), Some(fake.clone()), window, cx));
        let composer = project.read(cx).composer.clone();
        owner = Some(project);
        Root::new(composer, window, cx)
    });
    let project = owner.unwrap();
    visual.run_until_parked();
    project.update_in(visual, |view, window, cx| {
        ready(view, cx);
        view.open_express_session(window, cx);
    });
    visual
        .condition(&project, |view, _| view.express.pending.is_none())
        .await;
    // A regular file at the annotation namespace forces a real durable-store
    // failure before the immutable journal intent, not an injected send failure.
    let poison = state.path().join("t3-express-annotations");
    std::fs::write(&poison, b"blocked").unwrap();
    project.update_in(visual, |view, window, cx| {
        active(view).update(cx, |form, cx| {
            form.resume_draft_unchecked(window, cx);
            let value = draft();
            form.goal
                .update(cx, |input, cx| input.set_value(value.goal, window, cx));
            form.recipient
                .update(cx, |input, cx| input.set_value(value.recipient, window, cx));
            form.platforms = value.platforms;
            form.request(cx);
        })
    });
    visual
        .condition(&project, |view, _| view.express.pending.is_none())
        .await;
    project.read_with(visual, |view, cx| {
        let form = active(view);
        let form = form.read(cx);
        assert!(!form.locked);
        assert_eq!(form.goal.read(cx).value().to_string(), draft().goal);
        assert!(view.journal().unwrap().records().unwrap().is_empty());
        assert!(fake.sends.lock().unwrap().is_empty());
    });
    std::fs::remove_file(poison).unwrap();
    project.update_in(visual, |view, _, cx| {
        active(view).update(cx, |form, cx| form.request(cx))
    });
    visual
        .condition(&project, |view, _| view.express.pending.is_none())
        .await;
    project.read_with(visual, |view, cx| {
        let journal = view.journal().unwrap();
        let records = journal.records().unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            active(view).read(cx).locked,
            "uncertain delivery remains retained"
        );
        assert_eq!(fake.sends.lock().unwrap()[0], records[0].message);
        journal.send(fake.as_ref(), &records[0]).unwrap();
        assert_eq!(fake.sends.lock().unwrap()[1], records[0].message);
        assert_eq!(journal.records().unwrap().len(), 1);
        assert_eq!(view.composer.read(cx).typed_source_text(cx), CHAT_DRAFT);
    });
}
