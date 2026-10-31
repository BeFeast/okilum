use super::*;
use ::core::prelude::v1::test;

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
