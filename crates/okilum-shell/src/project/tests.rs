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
