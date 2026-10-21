use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn manager_history_and_questions_render_through_the_gateway(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake::default());
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture::view(state.path().to_owned(), gateway.clone(), window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        let chat = view.chat.as_ref().unwrap();
        assert_eq!(chat.items.len(), 2);
        assert_eq!(chat.pending_questions, 1);
        assert_eq!(
            chat.items[&1].text.as_deref(),
            Some("The first slice is ready for review.")
        );
    });
    *gateway.grown.lock().unwrap() = true;
    visual.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.chat.as_mut().unwrap().poll(gateway.as_ref()).unwrap();
            cx.notify();
        })
    });
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert_eq!(view.chat.as_ref().unwrap().items.len(), 2);
        assert!(view.chat.as_ref().unwrap().items[&1]
            .text
            .as_deref()
            .unwrap()
            .contains("Next:"));
    });
    assert!(visual.debug_bounds("project-tab").is_some());
}

#[gpui::test]
fn a_revoked_connection_has_a_visible_reason_and_keeps_the_draft(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake {
        revoked: true,
        ..Default::default()
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let view = fixture::view(state.path().to_owned(), gateway.clone(), window, cx);
            view.compose.update(cx, |input, cx| {
                input.set_value("Keep this draft", window, cx)
            });
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        assert_eq!(view.compose.read(cx).value().as_ref(), "Keep this draft");
        assert!(status_note(&view.gateway.as_ref().unwrap().status())
            .unwrap()
            .contains("revoked"));
    });
    assert!(visual.debug_bounds("project-connection-status").is_some());
}

#[gpui::test]
fn send_and_retry_keep_one_retained_message_and_one_request_id(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake {
        fail_first: true,
        ..Default::default()
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let view = fixture::view(state.path().to_owned(), gateway.clone(), window, cx);
            view.compose
                .update(cx, |input, cx| input.set_value("One message", window, cx));
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual.run_until_parked();
    let send = visual
        .debug_bounds("project-send")
        .expect("connected send control");
    visual.simulate_click(send.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert_eq!(view.records.len(), 1);
        assert!(view.records[0].receipt.is_none());
    });
    let retry = visual
        .debug_bounds("project-retry-0")
        .expect("retained message retry control");
    visual.simulate_click(retry.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert_eq!(view.records.len(), 1);
        assert!(view.records[0].receipt.is_some());
    });
    let calls = gateway.sends.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
}

#[test]
fn nearest_area_is_explicit_and_connections_never_contain_token_bytes() {
    let state = tempfile::tempdir().unwrap();
    let vault = state.path().join("vault");
    let area = vault.join("Area");
    std::fs::create_dir_all(area.join("ops")).unwrap();
    std::fs::create_dir_all(area.join("notes")).unwrap();
    std::fs::write(area.join("ops/project.yaml"), "display_name: Example\n").unwrap();
    assert_eq!(
        connections::area_for(&vault, &area.join("notes/note.md")).unwrap(),
        area
    );
    let env = Environment::new("test", "https://t3.example.test").unwrap();
    connections::save(state.path(), &env).unwrap();
    let source = std::fs::read_to_string(state.path().join("t3-connections.json")).unwrap();
    assert_eq!(source, "{\"test\":\"https://t3.example.test\"}");
}

#[gpui::test]
fn a_manager_from_another_project_is_not_enabled_for_sending(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake {
        wrong_project: true,
        ..Default::default()
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture::view(state.path().to_owned(), gateway, window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert!(!view.can_send());
        assert!(view
            .error
            .as_deref()
            .unwrap()
            .contains("another T3 project"));
    });
    assert!(visual.debug_bounds("project-error").is_some());
}

#[gpui::test]
fn project_tab_opens_inside_the_reader_and_returns_to_the_same_note(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let fixture = tempfile::tempdir().unwrap();
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault).unwrap();
    std::fs::write(
        vault.join("note.md"),
        "# Retained note\n\nReader content.\n",
    )
    .unwrap();
    std::fs::write(vault.join("other.md"), "# Another note\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    note: Some("note.md".into()),
                    index_dir: Some(fixture.path().join("index")),
                    session_directory: Some(fixture.path().join("state")),
                    ..Opts::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let reader = reader.unwrap();
    visual.simulate_resize(size(px(1200.), px(800.)));
    visual.run_until_parked();
    let retained = reader.read_with(visual, |view, _| {
        assert_eq!(view.current_rel, "note.md");
        view.content.clone()
    });
    let tab = visual
        .debug_bounds("reader-project")
        .expect("Reader Project control positive control");
    visual.simulate_click(tab.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-tab").is_some());
    let project = reader.read_with(visual, |view, _| {
        assert!(view.project_open);
        view.project.clone().unwrap()
    });
    visual.update(|window, cx| {
        project.update(cx, |view, cx| {
            view.compose.update(cx, |input, cx| {
                input.set_value("Retain this draft", window, cx)
            })
        })
    });
    let tab = visual.debug_bounds("reader-project").unwrap();
    visual.simulate_click(tab.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |view, _| {
        assert!(!view.project_open);
        assert_eq!(view.current_rel, "note.md");
        assert!(view.content == retained);
    });
    assert!(visual.debug_bounds("project-tab").is_none());
    let tab = visual.debug_bounds("reader-project").unwrap();
    visual.simulate_click(tab.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |view, _| {
        assert!(view.project.as_ref().unwrap() == &project)
    });
    project.read_with(visual, |view, cx| {
        assert_eq!(view.compose.read(cx).value().as_ref(), "Retain this draft")
    });
    visual.update(|window, cx| {
        reader.update(cx, |view, cx| view.open_note("other.md", None, window, cx))
    });
    visual.run_until_parked();
    reader.read_with(visual, |view, _| {
        assert_eq!(view.current_rel, "other.md", "navigation positive control")
    });
    let tab = visual.debug_bounds("reader-project").unwrap();
    visual.simulate_click(tab.center(), Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |view, _| {
        assert!(view.project.as_ref().unwrap() == &project)
    });
    project.read_with(visual, |view, cx| {
        assert_eq!(view.compose.read(cx).value().as_ref(), "Retain this draft")
    });
}

#[cfg(unix)]
#[gpui::test]
fn a_failed_new_intent_keeps_a_draft_even_when_old_text_matches(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;

    cx.update(gpui_component::init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake::default());
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let view = fixture::view(state.path().to_owned(), gateway.clone(), window, cx);
            let journal = view.journal().unwrap();
            let old = journal.prepare("Continue").unwrap();
            journal.send(gateway.as_ref(), &old).unwrap();
            view.compose
                .update(cx, |input, cx| input.set_value("Continue", window, cx));
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual.run_until_parked();
    let directory = std::fs::read_dir(state.path().join("t3-messages"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let original_permissions = std::fs::metadata(&directory).unwrap().permissions();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
    let send = visual
        .debug_bounds("project-send")
        .expect("connected send control");
    visual.simulate_click(send.center(), Modifiers::default());
    visual.run_until_parked();
    std::fs::set_permissions(&directory, original_permissions).unwrap();
    view.read_with(visual, |view, cx| {
        assert!(!view.busy, "the failed send completed");
        assert!(view.error.is_some(), "failed persistence must be visible");
        assert_eq!(view.compose.read(cx).value().as_ref(), "Continue");
        assert_eq!(
            view.records.len(),
            1,
            "only the older operation is retained"
        );
        assert!(view.records[0].receipt.is_some());
    });
    assert_eq!(
        gateway.sends.lock().unwrap().len(),
        1,
        "only the old send reached T3"
    );
}
