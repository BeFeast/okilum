use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn exact_instance_browse_search_and_click_preserve_confirmed_current(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let gateway = Arc::new(super::super::fixture::Fake::default());
    *gateway.model_catalog.lock().unwrap() = Some(serde_json::from_value(serde_json::json!({
        "runtimeMode":"full-access", "providers":[
            {"providerInstanceId":"account-a", "driverKind":"codex", "displayName":"Work Account", "models":[{"id":"shared", "label":"Reasoning Pro"},{"id":"mini", "label":"Swift Mini"}]},
            {"providerInstanceId":"account-b", "driverKind":"codex", "displayName":"Personal Account", "models":[{"id":"shared", "label":"Reasoning Pro"}]}
        ]
    })).unwrap());
    *gateway.model_current.lock().unwrap() = Some(
        serde_json::from_value(serde_json::json!({
            "instanceId":"account-a", "model":"shared", "options":[{"id":"effort", "value":"high"}]
        }))
        .unwrap(),
    );
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            super::super::fixture::view(temp.path().to_owned(), gateway.clone(), window, cx)
        });
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    let trigger = visual.debug_bounds("project-manager-model").unwrap();
    visual.simulate_click(trigger.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-model-current-0").is_some());
    assert!(visual.debug_bounds("project-model-current-1").is_none());
    assert!(visual.debug_bounds("project-model-choice-1").is_some());
    visual.simulate_input("PERSONAL rsng");
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.model_ui
                .search
                .as_ref()
                .unwrap()
                .read(cx)
                .value()
                .as_str(),
            "PERSONAL rsng",
            "opening focuses the search, not the composer"
        );
    });
    assert!(visual.debug_bounds("project-model-choice-0").is_some());
    assert!(visual.debug_bounds("project-model-choice-1").is_none());
    assert!(visual.debug_bounds("project-model-current-0").is_none());
    view.update_in(visual, |view, window, cx| {
        view.model_ui
            .search
            .as_ref()
            .unwrap()
            .update(cx, |input, cx| input.set_value("", window, cx));
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-model-current-0").is_some());
    let provider = visual.debug_bounds("project-model-provider-1").unwrap();
    visual.simulate_click(provider.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-model-current-0").is_none());
    assert!(visual.debug_bounds("project-model-choice-1").is_none());
    let choice = visual.debug_bounds("project-model-choice-0").unwrap();
    visual.simulate_click(choice.center(), Modifiers::default());
    visual.run_until_parked();
    let writes = gateway.model_writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].instance_id, "account-b");
    assert!(
        writes[0].options.is_empty(),
        "options do not cross accounts"
    );
    drop(writes);
    assert!(visual.debug_bounds("project-model-panel").is_none());
    visual.simulate_input("Composer focus restored");
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.compose.read(cx).value().as_str(),
            "Composer focus restored"
        )
    });
    assert!(gateway.sends.lock().unwrap().is_empty());
    // A change from another client between openings must choose the fresh
    // current instance, not the instance cached by the previous popup.
    *gateway.model_current.lock().unwrap() = Some(ModelSelection {
        instance_id: "account-a".into(),
        model: "mini".into(),
        options: vec![],
    });
    let trigger = visual.debug_bounds("project-manager-model").unwrap();
    visual.simulate_click(trigger.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-model-current-1").is_some());
    view.read_with(visual, |view, _| {
        assert_eq!(view.model_ui.provider.as_deref(), Some("account-a"))
    });
}

#[gpui::test]
fn current_near_the_end_of_a_large_catalog_is_visible_when_opened(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let gateway = Arc::new(super::super::fixture::Fake {
        many_models: true,
        ..Default::default()
    });
    *gateway.model_current.lock().unwrap() = Some(ModelSelection {
        instance_id: "codex".into(),
        model: "model-39".into(),
        options: vec![],
    });
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            super::super::fixture::view(temp.path().to_owned(), gateway.clone(), window, cx)
        });
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    let trigger = visual.debug_bounds("project-manager-model").unwrap();
    visual.simulate_click(trigger.center(), Modifiers::default());
    visual.run_until_parked();
    // TestAppContext drains tasks but does not deliver display callbacks.
    // Positive control: deliver the requested post-layout frame explicitly.
    visual.update(|window, cx| {
        assert!(
            window.simulate_next_frame(cx) > 0,
            "reveal requested a frame"
        );
        window.draw(cx).clear(cx);
    });
    visual.run_until_parked();
    let list = visual.debug_bounds("project-model-choices").unwrap();
    let current = visual.debug_bounds("project-model-current-41").unwrap();
    assert!(
        list.contains(&current.center()),
        "current checkmark must be visible, not below the scroll viewport: {current:?} in {list:?}"
    );
    assert!(gateway.model_writes.lock().unwrap().is_empty());
}

#[gpui::test]
fn disconnect_clears_model_loading_and_reconnect_can_load_again(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let gateway = Arc::new(super::super::fixture::Fake {
        many_models: true,
        ..Default::default()
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            super::super::fixture::view(temp.path().to_owned(), gateway.clone(), window, cx)
        });
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    view.update_in(visual, |view, window, cx| {
        view.compose.update(cx, |input, cx| {
            input.set_value("Retain through reconnect", window, cx)
        });
        view.model_target = view.manager_model_target();
        view.model_busy = true;
        view.model_open = true;
        let environment = view.environment.clone().unwrap();
        view.clear_connection(cx);
        assert!(!view.model_busy && !view.model_open);
        assert!(view.models.is_none() && view.model_target.is_none());
        view.finish_connection(Ok((environment, gateway.clone())), window, cx);
    });
    visual.run_until_parked();
    let trigger = visual.debug_bounds("project-manager-model").unwrap();
    visual.simulate_click(trigger.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        assert!(
            !view.model_busy,
            "reopening must finish loading after reconnect"
        );
        assert!(view.models.is_some() && view.model_error.is_none());
        assert_eq!(
            view.compose.read(cx).value().as_str(),
            "Retain through reconnect"
        );
    });
    assert!(
        visual.debug_bounds("project-model-choice-0").is_some(),
        "positive control: the catalog is visible in the actual popover"
    );
    assert!(
        gateway.model_writes.lock().unwrap().is_empty(),
        "opening models must not configure a work thread"
    );
}
