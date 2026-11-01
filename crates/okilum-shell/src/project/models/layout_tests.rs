//! Actual GPUI popup geometry and physical pointer/scroll regressions. These
//! headless layout checks make no native screenshot or compositor claim.
use super::*;
use ::core::prelude::v1::test;
use serde_json::json;

fn catalog() -> serde_json::Value {
    let descriptors = json!([
        {"type":"select","id":"reasoning","label":"Reasoning","currentValue":"reason-4",
            "options":(0..10).map(|index| json!({"id":format!("reason-{index}"),"label":format!("Reasoning choice {index}"),"isDefault":index==4})).collect::<Vec<_>>()},
        {"type":"select","id":"serviceTier","label":"Service tier","currentValue":"tier-2",
            "options":(0..8).map(|index| json!({"id":format!("tier-{index}"),"label":format!("Service tier {index}"),"isDefault":index==2})).collect::<Vec<_>>()},
        {"type":"boolean","id":"acceleration","label":"Acceleration","currentValue":false}
    ]);
    json!({"runtimeMode":"full-access","providers":(0..8).map(|provider|json!({
        "providerInstanceId":format!("account-{provider}"),"driverKind":"codex",
        "displayName":format!("Very long provider account label {provider}"),
        "models":(0..24).map(|model|json!({
            "id":format!("model-{model}"),"label":format!("Long upstream model name {model}"),
            "shortName":format!("Readable model {model}"),"subProvider":"Remote upstream vendor",
            "capabilities":{"optionDescriptors":descriptors}
        })).collect::<Vec<_>>()
    })).collect::<Vec<_>>()})
}

struct PickerHost {
    view: Entity<ProjectView>,
    manager_width: Option<Pixels>,
}

impl Render for PickerHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex().size_full().child(div().flex_1()).child(
            div()
                .h_full()
                .overflow_hidden()
                .when_some(self.manager_width, |host, width| host.w(width))
                .when(self.manager_width.is_none(), |host| host.flex_1())
                .child(self.view.clone()),
        )
    }
}

fn fixture(
    cx: &mut TestAppContext,
    viewport: Size<Pixels>,
    current_model: usize,
    manager_width: Option<Pixels>,
) -> (
    Entity<ProjectView>,
    &mut VisualTestContext,
    tempfile::TempDir,
    Arc<super::super::fixture::Fake>,
) {
    cx.update(|cx| {
        crate::test_init(cx);
        cx.set_reduce_motion(true);
    });
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(super::super::fixture::Fake::default());
    *gateway.model_catalog.lock().unwrap() = Some(serde_json::from_value(catalog()).unwrap());
    *gateway.model_current.lock().unwrap() = Some(
        serde_json::from_value(json!({
            "instanceId":"account-0","model":format!("model-{current_model}"),"options":[]
        }))
        .unwrap(),
    );
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            super::super::fixture::view(state.path().to_owned(), gateway.clone(), window, cx)
        });
        let host = cx.new(|_| PickerHost {
            view,
            manager_width,
        });
        Root::new(host, window, cx)
    });
    visual.simulate_resize(viewport);
    visual.run_until_parked();
    let view = root.read_with(visual, |root, cx| {
        root.view()
            .clone()
            .downcast::<PickerHost>()
            .unwrap()
            .read(cx)
            .view
            .clone()
    });
    view.read_with(visual, |view, _| {
        assert!(
            view.models.is_some(),
            "positive control: fixture catalog completed connection warmup"
        );
    });
    (view, visual, state, gateway)
}

fn bounds(visual: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("visible positive control missing: {selector}"))
}

fn inside(inner: Bounds<Pixels>, outer: Bounds<Pixels>, label: &str) {
    let tolerance = px(1.);
    assert!(
        inner.left() >= outer.left() - tolerance
            && inner.top() >= outer.top() - tolerance
            && inner.right() <= outer.right() + tolerance
            && inner.bottom() <= outer.bottom() + tolerance,
        "{label} escaped its visible viewport: inner={inner:?}; outer={outer:?}"
    );
}

fn interleaved_open(visual: &mut VisualTestContext) {
    let position = bounds(visual, "project-manager-model").center();
    visual.simulate_event(gpui::MouseDownEvent {
        position,
        button: gpui::MouseButton::Left,
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-model-panel").is_none(),
        "the popup must not appear under the trigger's in-flight pointer release"
    );
    visual.simulate_event(gpui::MouseUpEvent {
        position,
        button: gpui::MouseButton::Left,
        modifiers: Modifiers::default(),
        click_count: 1,
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-model-panel").is_some(),
        "completed trigger click opens an actual popup after an intervening frame"
    );
    // Real platform display callbacks run before draw. Deliver both queued
    // frames so the current-row reveal uses the initialized virtual viewport.
    for _ in 0..2 {
        visual.update(|window, cx| {
            assert!(
                window.simulate_next_frame(cx) > 0,
                "positive control: the reveal scheduled a display callback"
            );
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
    }
}

fn scroll(visual: &mut VisualTestContext, viewport: Bounds<Pixels>, delta: f32) {
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: viewport.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(delta))),
        ..Default::default()
    });
    visual.run_until_parked();
}

#[gpui::test]
fn short_window_picker_opens_after_release_and_all_columns_remain_reachable(
    cx: &mut TestAppContext,
) {
    let viewport = size(px(760.), px(520.));
    let (view, visual, _state, gateway) = fixture(cx, viewport, 12, None);
    let reads = gateway.reads.lock().unwrap().len();
    // Let the normal closed view pass through idle display frames before the
    // completed click. The trigger must not require an extra capture-open cycle.
    for _ in 0..3 {
        visual.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
    }
    interleaved_open(visual);
    let panel = bounds(visual, "project-model-panel");
    let window = Bounds {
        origin: point(px(0.), px(0.)),
        size: viewport,
    };
    inside(panel, window, "popup");
    let (surface, mask) = view.read_with(visual, |view, _| {
        view.model_ui
            .painted_surface
            .get()
            .expect("positive control: surface reached paint, not just render/layout")
    });
    assert!(surface.size.width > px(0.) && surface.size.height > px(0.));
    inside(surface, window, "painted popup including padding");
    inside(
        surface,
        mask,
        "painted surface is not clipped by its narrow ancestor",
    );
    inside(panel, surface, "panel is within actual painted surface");
    let choices = bounds(visual, "project-model-choices");
    let options = bounds(visual, "project-model-option-column");
    let providers = bounds(visual, "project-model-providers");
    inside(choices, panel, "model viewport");
    inside(options, panel, "option viewport");
    inside(providers, panel, "provider viewport");
    let current = bounds(visual, "project-model-current-12");
    assert!(
        choices.contains(&current.center()),
        "current middle model is actually visible after opening"
    );
    let model_scroll_before = view.read_with(visual, |view, _| {
        view.model_ui.list_scroll.0.borrow().base_handle.offset()
    });
    scroll(visual, options, -10000.);
    let last_option = bounds(visual, "project-model-option-2-1");
    inside(last_option, options, "last boolean option");
    assert_eq!(
        view.read_with(visual, |view, _| {
            view.model_ui.list_scroll.0.borrow().base_handle.offset()
        }),
        model_scroll_before,
        "option wheel scroll cannot move the model list"
    );
    scroll(visual, providers, -10000.);
    let last_provider = bounds(visual, "project-model-provider-7");
    inside(last_provider, providers, "last provider account");
    visual.simulate_click(last_provider.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        assert_eq!(view.model_ui.provider.as_deref(), Some("account-7"));
        assert!(view
            .visible_model_choices(cx)
            .iter()
            .all(|choice| choice.selection.instance_id == "account-7"));
    });
    let choices = bounds(visual, "project-model-choices");
    scroll(visual, choices, -10000.);
    let last_model = bounds(visual, "project-model-choice-23");
    inside(last_model, choices, "last selectable model");
    assert_eq!(
        gateway.reads.lock().unwrap().len(),
        reads,
        "open and local column navigation send zero network reads"
    );
    assert!(
        gateway.model_writes.lock().unwrap().is_empty(),
        "opening, scrolling and changing the provider rail never select a model"
    );
    assert!(gateway.sends.lock().unwrap().is_empty());
}

#[gpui::test]
fn last_current_model_and_option_heading_lineboxes_fit_large_window_popup(cx: &mut TestAppContext) {
    let viewport = size(px(1000.), px(760.));
    let (view, visual, _state, gateway) = fixture(cx, viewport, 23, Some(px(320.)));
    let reads = gateway.reads.lock().unwrap().len();
    view.update_in(visual, |view, window, cx| {
        view.compose.update(cx, |input, cx| {
            input.set_value("Keep the Manager draft", window, cx)
        });
    });
    visual.run_until_parked();
    // Let the normal closed view pass through idle display frames before the
    // completed click. The trigger must not require an extra capture-open cycle.
    for _ in 0..3 {
        visual.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
    }
    interleaved_open(visual);
    let panel = bounds(visual, "project-model-panel");
    let window = Bounds {
        origin: point(px(0.), px(0.)),
        size: viewport,
    };
    inside(panel, window, "popup");
    let (surface, mask) = view.read_with(visual, |view, _| {
        view.model_ui
            .painted_surface
            .get()
            .expect("positive control: surface reached paint, not just render/layout")
    });
    assert!(surface.size.width > px(0.) && surface.size.height > px(0.));
    inside(surface, window, "painted popup including padding");
    inside(
        surface,
        mask,
        "painted surface is not clipped by its narrow ancestor",
    );
    inside(panel, surface, "panel is within actual painted surface");
    assert!(
        panel.size.width > px(320.),
        "the popup has a workspace-width budget, not a narrow Manager-panel budget"
    );
    inside(
        bounds(visual, "project-model-title"),
        panel,
        "model-selector identity",
    );
    inside(
        bounds(visual, "project-model-close"),
        panel,
        "visible close action",
    );
    let rail = bounds(visual, "project-model-provider-rail");
    assert!(
        rail.size.width >= px(120.),
        "a spacious window shows readable account identities"
    );
    let providers = bounds(visual, "project-model-providers");
    inside(
        bounds(visual, "project-model-account-label-0"),
        providers,
        "readable account label",
    );
    let choices = bounds(visual, "project-model-choices");
    assert!(
        choices.size.width >= px(300.),
        "provider labels must not squeeze readable model names"
    );
    let current = bounds(visual, "project-model-current-23");
    assert!(
        choices.contains(&current.center()),
        "the current last model is visible without an offscreen click"
    );
    let options = bounds(visual, "project-model-option-column");
    inside(
        bounds(visual, "project-model-inspector-title"),
        options,
        "current-model inspector identity",
    );
    inside(
        bounds(visual, "project-model-settings-title"),
        options,
        "supported settings heading",
    );
    let reasoning = bounds(visual, "project-model-option-heading-0");
    assert!(
        reasoning.size.height >= px(18.),
        "Reasoning needs a full 13px text linebox with descender room: {reasoning:?}"
    );
    inside(reasoning, options, "Reasoning heading");
    scroll(visual, options, -10000.);
    let service = bounds(visual, "project-model-option-heading-1");
    assert!(
        service.size.height >= px(18.),
        "Service tier needs the same complete linebox: {service:?}"
    );
    // Bring the second heading into the visible middle of its own viewport,
    // using the same physical wheel route as a user rather than a direct offset.
    let offset = service.top() - options.top();
    if offset < px(0.) || service.bottom() > options.bottom() {
        scroll(
            visual,
            options,
            f32::from(options.top() + px(8.) - service.top()),
        );
    }
    let service = bounds(visual, "project-model-option-heading-1");
    inside(service, options, "Service tier heading");
    view.read_with(visual, |view, _| assert!(view.model_open));
    assert!(gateway.model_writes.lock().unwrap().is_empty());
    assert!(gateway.sends.lock().unwrap().is_empty());
    let close = bounds(visual, "project-model-close");
    visual.simulate_click(close.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-model-panel").is_none());
    view.update_in(visual, |view, window, cx| {
        assert!(!view.model_open);
        assert_eq!(
            view.compose.read(cx).value().as_str(),
            "Keep the Manager draft"
        );
        assert!(
            view.compose.read(cx).focus_handle(cx).is_focused(window),
            "closing restores composer focus"
        );
    });
    assert_eq!(
        gateway.reads.lock().unwrap().len(),
        reads,
        "visual selector navigation and close issue zero RPC"
    );
}
