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
    // A platform delivers callbacks BEFORE draw; the second display callback
    // must reveal the row after the first layout has initialized overflow.
    for _ in 0..2 {
        visual.update(|window, cx| {
            assert!(
                window.simulate_next_frame(cx) > 0,
                "reveal requested a frame"
            );
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
    }
    let list = visual.debug_bounds("project-model-choices").unwrap();
    let current = visual.debug_bounds("project-model-current-41").unwrap();
    assert!(
        list.contains(&current.center()),
        "current checkmark must be visible, not below the scroll viewport: {current:?} in {list:?}"
    );
    assert!(gateway.model_writes.lock().unwrap().is_empty());
}

#[gpui::test]
fn catalog_read_and_refresh_do_not_require_send_readiness(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let gateway = Arc::new(super::super::fixture::Fake::default());
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view =
                super::super::fixture::view(temp.path().to_owned(), gateway.clone(), window, cx);
            view.identity = None;
            view
        });
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    // The fixture's initial connection establishes an identity asynchronously.
    // Remove it after that connection settles to exercise read-only inspection.
    view.update(visual, |view, _| view.identity = None);
    view.read_with(visual, |view, _| assert!(!view.can_send()));
    let trigger = visual.debug_bounds("project-manager-model").unwrap();
    visual.simulate_click(trigger.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-model-choice-1").is_some(),
        "positive control: the read-only catalog really loaded"
    );
    let choice = visual.debug_bounds("project-model-choice-1").unwrap();
    visual.simulate_click(choice.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        gateway.model_writes.lock().unwrap().is_empty(),
        "no identity upgrade or model mutation"
    );
    *gateway.model_read_failure.lock().unwrap() = Some(okilum_t3::GatewayError::Unreachable(
        "temporary failure".into(),
    ));
    let refresh = visual.debug_bounds("project-model-refresh").unwrap();
    visual.simulate_click(refresh.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert!(!view.model_busy && view.model_error.is_some())
    });
    *gateway.model_read_failure.lock().unwrap() = None;
    let refresh = visual.debug_bounds("project-model-refresh").unwrap();
    visual.simulate_click(refresh.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert!(!view.model_busy && view.models.is_some() && view.model_error.is_none())
    });
    assert!(gateway.sends.lock().unwrap().is_empty());
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

#[gpui::test]
#[cfg(target_os = "linux")]
fn live_instance_and_effort_changes_replace_cached_current_without_refresh(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let gateway = Arc::new(super::super::fixture::Fake::default());
    *gateway.model_catalog.lock().unwrap() = Some(serde_json::from_value(serde_json::json!({"runtimeMode":"full-access","providers":[
        {"providerInstanceId":"seat-a","driverKind":"claudeAgent","displayName":"Work","models":[{"id":"shared","label":"Work Opus"}]},
        {"providerInstanceId":"seat-b","driverKind":"claudeAgent","displayName":"Personal","models":[{"id":"shared","label":"Personal Opus"}]}
    ]})).unwrap());
    let selection = |seat: &str, effort: &str| -> ModelSelection {
        serde_json::from_value(serde_json::json!({"instanceId":seat,"model":"shared","options":[{"id":"effort","value":effort}]})).unwrap()
    };
    *gateway.model_current.lock().unwrap() = Some(selection("seat-a", "high"));
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
    let original = view.read_with(visual, |view, _| view.models.clone().unwrap());
    let thread = original.target.thread_id.clone();
    let metadata = |seat: &str, effort: &str| okilum_t3::projection::Metadata {
        thread_id: thread.clone(),
        model: "shared".into(),
        model_selection: Some(selection(seat, effort)),
        title: "Same thread".into(),
        ..Default::default()
    };
    view.update(visual, |view, cx| {
        let initial = metadata("seat-a", "high");
        view.receive_live_model(&initial);
        view.native.retain_metadata(initial);
        cx.notify();
    });
    let requested_revision = view.read_with(visual, |view, _| view.native.model_revision);
    for (seat, effort) in [("seat-b", "high"), ("seat-b", "max"), ("seat-a", "high")] {
        view.update(visual, |view, cx| {
            let next = metadata(seat, effort);
            view.receive_live_model(&next);
            view.native.retain_metadata(next);
            cx.notify();
        });
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(
                view.models.as_ref().unwrap().configuration.model_selection,
                selection(seat, effort)
            );
            assert_eq!(view.model_ui.provider.as_deref(), Some(seat));
            assert!(view.native.current_selection("some-other-thread").is_none());
        });
        assert!(
            visual.debug_bounds("project-model-current-0").is_some(),
            "confirmed current remains checked with non-empty options and same-driver duplicate slugs"
        );
    }
    view.read_with(visual, |view, _| {
        let mut slow_response = original.clone();
        slow_response.configuration.model_selection = selection("seat-b", "max");
        view.reconcile_model_read(&mut slow_response, requested_revision);
        assert_eq!(
            slow_response.configuration.model_selection,
            selection("seat-a", "high"),
            "A→B→A subscription changes still invalidate an earlier read"
        );
    });
    assert!(
        gateway.model_writes.lock().unwrap().is_empty(),
        "metadata/readback reconciliation must never configure a thread"
    );
    assert!(gateway.sends.lock().unwrap().is_empty());
}
