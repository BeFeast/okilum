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
    assert!(
        view.read_with(visual, |view, _| view.model_ui.option_scroll.offset().y) < px(0.),
        "positive control: the settings content actually scrolled"
    );
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
    let close = bounds(visual, "project-model-close");
    visual.simulate_click(close.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-model-panel").is_none(),
        "the stacked settings scrollbar cannot intercept Close after scrolling"
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
    let logo = bounds(visual, "project-model-provider-logo-account-0");
    let account = bounds(visual, "project-model-account-label-0");
    let suffix = bounds(visual, "project-model-account-suffix-0");
    assert_eq!(
        logo.size,
        size(px(18.), px(18.)),
        "brand artwork uses an optical 18px slot"
    );
    assert!(
        logo.right() <= account.left() && account.right() <= suffix.left(),
        "account identity is beside the name, never stamped over original artwork"
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
    assert!(visual
        .debug_bounds("project-model-inspector-title")
        .is_none());
    assert!(visual
        .debug_bounds("project-model-settings-title")
        .is_none());
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

fn painted_surface(view: &Entity<ProjectView>, visual: &mut VisualTestContext) -> Bounds<Pixels> {
    view.read_with(visual, |view, _| {
        view.model_ui
            .painted_surface
            .get()
            .expect("positive control: this transition reached surface paint")
            .0
    })
}
fn redraw_picker(
    view: &Entity<ProjectView>,
    visual: &mut VisualTestContext,
    change: impl FnOnce(&mut ProjectView, &mut Window, &mut Context<ProjectView>),
) {
    view.update_in(visual, |view, window, cx| {
        view.model_ui.painted_surface.set(None);
        change(view, window, cx);
        cx.notify();
    });
    visual.run_until_parked();
}
fn assert_surface_stable(
    view: &Entity<ProjectView>,
    visual: &mut VisualTestContext,
    expected: Bounds<Pixels>,
    transition: &str,
) {
    assert_eq!(
        painted_surface(view, visual),
        expected,
        "outer painted width, height and position must stay fixed across {transition}"
    );
    inside(
        bounds(visual, "project-model-close"),
        expected,
        "close action",
    );
}

#[gpui::test]
fn provider_search_options_and_loading_keep_one_surface_and_long_catalog_tail_reachable(
    cx: &mut TestAppContext,
) {
    let (view, visual, _state, gateway) = fixture(cx, size(px(1000.), px(760.)), 0, Some(px(320.)));
    redraw_picker(&view, visual, |view, _, cx| {
        view.active = true;
        let snapshot = view.models.as_mut().unwrap();
        let model = snapshot.catalog.providers[0].models[0].clone();
        snapshot.catalog.providers[0].models = (0..120)
            .map(|index| {
                let mut model = model.clone();
                model.id = format!("model-{index}");
                model.short_name = Some(format!("Codex model {index}"));
                model
            })
            .collect();
        snapshot.catalog.providers[1].models.truncate(1);
        snapshot.catalog.providers[1].models[0].capabilities = None;
        cx.notify();
    });
    let reads = gateway.reads.lock().unwrap().len();
    interleaved_open(visual);
    let expected = painted_surface(&view, visual);
    assert!(
        expected.size.height <= px(530.),
        "balanced viewport-capped surface, not an expanding catalog-sized window"
    );
    let target = view.read_with(visual, |view, _| view.model_target.clone());
    let click = |visual: &mut VisualTestContext, selector| {
        let area = bounds(visual, selector);
        visual.simulate_click(area.center(), Modifiers::default());
        visual.run_until_parked();
    };
    click(visual, "project-model-favorites");
    assert_surface_stable(&view, visual, expected, "short favorites");
    assert_eq!(
        view.read_with(visual, |view, cx| view.visible_model_choices(cx).len()),
        1
    );
    click(visual, "project-model-provider-1");
    assert_surface_stable(&view, visual, expected, "single-model provider");
    assert_eq!(
        view.read_with(visual, |view, cx| view.visible_model_choices(cx).len()),
        1
    );
    for query in ["Codex model 119", "no-such-model-at-all", ""] {
        redraw_picker(&view, visual, |view, window, cx| {
            view.model_ui
                .search
                .as_ref()
                .unwrap()
                .update(cx, |input, cx| input.set_value(query, window, cx));
        });
        assert_surface_stable(&view, visual, expected, "search/empty-search");
        view.read_with(visual, |view, cx| {
            let choices = view.visible_model_choices(cx);
            match query {
                "" => assert_eq!(
                    choices.len(),
                    1,
                    "clearing search returns the selected one-model account"
                ),
                "no-such-model-at-all" => {
                    assert!(choices.is_empty(), "unmatched query is visibly empty")
                }
                _ => assert!(
                    choices
                        .iter()
                        .any(|choice| choice.selection.model == "model-119"),
                    "fuzzy search must include the actual requested last Codex model"
                ),
            }
        });
        assert!(
            visual.debug_bounds("project-model-option-column").is_some(),
            "settings stay discoverable while searching"
        );
    }
    let with_options = view.read_with(visual, |view, _| view.models.clone().unwrap());
    redraw_picker(&view, visual, |view, _, _| {
        let snapshot = view.models.as_mut().unwrap();
        snapshot.configuration.model_selection.instance_id = "account-1".into();
        snapshot.configuration.model_selection.model = "model-0".into();
        snapshot.configuration.model_selection.options.clear();
    });
    assert_surface_stable(
        &view,
        visual,
        expected,
        "server-confirmed model without descriptors",
    );
    assert!(visual.debug_bounds("project-model-no-options").is_some());
    assert!(visual.debug_bounds("project-model-option-column").is_some());
    redraw_picker(&view, visual, |view, _, _| {
        view.models = None;
        view.model_busy = true;
    });
    assert_surface_stable(&view, visual, expected, "initial loading without catalog");
    redraw_picker(&view, visual, |view, _, _| {
        view.models = Some(with_options);
        view.model_busy = false;
        view.model_error = Some("Read-only fixture catalog failure".into());
        view.connection_details = true;
    });
    assert_surface_stable(
        &view,
        visual,
        expected,
        "restored catalog with error/details",
    );
    let settings = bounds(visual, "project-model-option-column");
    scroll(visual, settings, -10000.);
    view.read_with(visual, |view, _| {
        let painted = view.model_ui.diagnostics_painted.borrow();
        assert!(
            painted
                .iter()
                .any(|text| text == "Read-only fixture catalog failure"),
            "expanded diagnostics must paint the actual error bytes, not only a generic status"
        );
        assert!(
            painted
                .iter()
                .any(|text| text == okilum_t3::models::SCHEDULED_TASK_LIMIT),
            "expanded diagnostics must paint the existing scheduler help"
        );
    });
    inside(
        bounds(visual, "project-model-error-detail"),
        settings,
        "actual expanded error",
    );
    inside(
        bounds(visual, "project-model-scheduler-limit"),
        settings,
        "actual expanded help",
    );
    assert_surface_stable(&view, visual, expected, "diagnostics scroll");
    redraw_picker(&view, visual, |view, _, _| {
        view.connection_details = false;
        view.model_ui.diagnostics_painted.borrow_mut().clear();
    });
    assert_surface_stable(
        &view,
        visual,
        expected,
        "collapsed diagnostics with error retained",
    );
    assert!(visual.debug_bounds("project-model-error-detail").is_none());
    assert!(visual
        .debug_bounds("project-model-scheduler-limit")
        .is_none());
    view.read_with(visual, |view, _| {
        assert!(
            view.model_ui.diagnostics_painted.borrow().is_empty(),
            "collapsed diagnostics cannot paint private error/help text"
        )
    });
    redraw_picker(&view, visual, |view, _, _| {
        view.model_error = None;
    });
    click(visual, "project-model-provider-0");
    assert_surface_stable(&view, visual, expected, "120-model Codex provider");
    let viewport = bounds(visual, "project-model-choices");
    scroll(visual, viewport, -10000.);
    inside(
        bounds(visual, "project-model-choice-119"),
        viewport,
        "last Codex row after physical wheel",
    );
    click(visual, "project-model-provider-0");
    redraw_picker(&view, visual, |view, window, cx| {
        view.model_ui.highlight = 0;
        view.model_ui
            .search
            .as_ref()
            .unwrap()
            .update(cx, |input, cx| input.focus(window, cx));
    });
    visual.simulate_keystrokes("up");
    visual.run_until_parked();
    assert_eq!(
        view.read_with(visual, |view, _| view.model_ui.highlight),
        119
    );
    inside(
        bounds(visual, "project-model-choice-119"),
        bounds(visual, "project-model-choices"),
        "last Codex row after keyboard navigation",
    );
    assert_surface_stable(&view, visual, expected, "keyboard tail reveal");
    assert_eq!(
        view.read_with(visual, |view, _| view.model_target.clone()),
        target
    );
    assert!(
        gateway.model_writes.lock().unwrap().is_empty(),
        "scrolling and navigation never choose a model"
    );
    assert!(gateway.sends.lock().unwrap().is_empty());
    assert_eq!(
        gateway.reads.lock().unwrap().len(),
        reads,
        "all visual transitions issue zero RPC"
    );
    visual.simulate_resize(size(px(900.), px(620.)));
    redraw_picker(&view, visual, |_, _, _| {});
    let resized = painted_surface(&view, visual);
    assert!(
        resized.size.width < expected.size.width,
        "only the changed viewport alters dimensions"
    );
    inside(
        resized,
        Bounds::new(point(px(0.), px(0.)), size(px(900.), px(620.))),
        "resized painted surface",
    );
    visual.simulate_resize(size(px(600.), px(700.)));
    redraw_picker(&view, visual, |_, _, _| {});
    let logo = bounds(visual, "project-model-provider-logo-account-0");
    let suffix = bounds(visual, "project-model-account-suffix-0");
    assert_eq!(logo.size, size(px(18.), px(18.)));
    assert!(
        suffix.left() >= logo.right(),
        "compact accounts remain distinct beside unobscured logos"
    );
    inside(
        suffix,
        bounds(visual, "project-model-providers"),
        "compact account suffix",
    );
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    view.update_in(visual, |view, window, cx| {
        assert!(!view.model_open);
        assert!(view.compose.read(cx).focus_handle(cx).is_focused(window));
    });
}

#[gpui::test]
fn compact_header_and_aligned_rail_fit_real_provider_names_without_ellipsis(
    cx: &mut TestAppContext,
) {
    let (view, visual, _state, _gateway) = fixture(cx, size(px(1000.), px(760.)), 0, None);
    redraw_picker(&view, visual, |view, _, _| {
        let providers = &mut view.models.as_mut().unwrap().catalog.providers;
        providers[0].display_name = Some("Claude".into());
        providers[1].display_name = Some("Claude → Astra".into());
    });
    interleaved_open(visual);
    let header = bounds(visual, "project-model-header");
    let title = bounds(visual, "project-model-title");
    let search = bounds(visual, "project-model-search");
    inside(title, header, "compact title");
    inside(search, header, "inline search");
    assert!(title.right() <= search.left());
    assert!((title.center().y - search.center().y).abs() <= px(1.));
    assert!(header.size.height <= px(40.));
    assert!(
        search.size.width >= px(300.),
        "search retains a useful text width"
    );
    assert!(search.size.width <= px(320.), "search has a finite width");
    assert_eq!(search.size.height, px(32.));
    assert!(visual.debug_bounds("project-model-status").is_none());
    let account = bounds(visual, "project-model-account-label-0");
    for selector in [
        "project-model-rail-label-favorites",
        "project-model-rail-label-current",
    ] {
        assert_eq!(
            bounds(visual, selector).left(),
            account.left(),
            "{selector} shares the provider text column"
        );
    }
    let logo = bounds(visual, "project-model-provider-logo-account-0");
    for selector in [
        "project-model-rail-icon-favorites",
        "project-model-rail-icon-current",
    ] {
        assert_eq!(
            bounds(visual, selector).left(),
            logo.left(),
            "{selector} shares the provider icon column"
        );
    }
    for (selector, text) in [
        ("project-model-rail-label-favorites", "Favorites"),
        ("project-model-rail-label-current", "Selected model"),
        ("project-model-account-label-1", "Claude → Astra"),
    ] {
        let intrinsic = visual.update(|window, _| {
            window
                .text_system()
                .shape_line(
                    text.into(),
                    px(super::super::design::LABEL),
                    &[gpui::TextRun {
                        len: text.len(),
                        font: gpui::font(brand::SANS_FONT),
                        color: gpui::black(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                )
                .width()
        });
        let label = bounds(visual, selector);
        assert!(
            label.size.width >= intrinsic,
            "{text} must fit without truncation: {label:?}, needs {intrinsic:?}"
        );
    }
}

#[gpui::test]
fn full_pilot_account_name_wraps_inside_the_rail_instead_of_truncating(cx: &mut TestAppContext) {
    let (view, visual, _state, _gateway) = fixture(cx, size(px(1000.), px(760.)), 0, None);
    redraw_picker(&view, visual, |view, _, _| {
        view.models.as_mut().unwrap().catalog.providers[1].display_name =
            Some("Claude → Astra subagents · 7-day pilot".into());
    });
    interleaved_open(visual);
    let label = bounds(visual, "project-model-account-label-1");
    let row = bounds(visual, "project-model-provider-1");
    assert!(
        label.size.height >= px(40.),
        "the complete account name needs multiple text lines: {label:?}"
    );
    inside(label, row, "wrapped account name");
    inside(
        row,
        bounds(visual, "project-model-providers"),
        "account row",
    );
}

#[gpui::test]
fn seven_reasoning_choices_and_fast_mode_fit_without_clipping_at_default_size(
    cx: &mut TestAppContext,
) {
    let (view, visual, _state, gateway) = fixture(cx, size(px(1000.), px(760.)), 0, None);
    redraw_picker(&view, visual, |view, _, cx| {
        brand::apply_theme(cx);
        view.models.as_mut().unwrap().catalog.providers[0].models[0].capabilities = Some(
            serde_json::from_value(json!({"optionDescriptors":[
                {"type":"select","id":"reasoning","label":"Reasoning","options":[
                    {"id":"low","label":"Low"},
                    {"id":"medium","label":"Medium","isDefault":true},
                    {"id":"high","label":"High"},
                    {"id":"xhigh","label":"Extra High"},
                    {"id":"max","label":"Max"},
                    {"id":"ultracode","label":"Ultracode"},
                    {"id":"ultrathink","label":"Ultrathink"}
                ]},
                {"type":"boolean","id":"fastMode","label":"Fast Mode","currentValue":false}
            ]}))
            .unwrap(),
        );
    });
    interleaved_open(visual);
    // The normal 880 x 500 dialog and a shorter, ordinary window both expose
    // the entire advertised set before any wheel input. Extra settings use the
    // independent scroll viewport exercised by the short-window regression.
    for viewport in [size(px(1000.), px(760.)), size(px(900.), px(650.))] {
        visual.simulate_resize(viewport);
        visual.run_until_parked();
        let panel = bounds(visual, "project-model-panel");
        let title = bounds(visual, "project-model-title");
        let search = bounds(visual, "project-model-search");
        assert_eq!(search.size.height, px(32.));
        assert!((title.center().y - search.center().y).abs() <= px(1.));
        let column = bounds(visual, "project-model-option-column");
        inside(column, panel, "complete settings viewport");
        assert!(visual
            .debug_bounds("project-model-settings-title")
            .is_none());
        assert!(visual
            .debug_bounds("project-model-inspector-title")
            .is_none());
        let heading = bounds(visual, "project-model-option-heading-0");
        assert_eq!(
            heading.top(),
            column.top() + px(super::super::design::INSET)
        );
        for selector in [
            "project-model-option-0-0",
            "project-model-option-0-6",
            "project-model-option-heading-1",
            "project-model-option-1-0",
            "project-model-option-1-1",
        ] {
            inside(bounds(visual, selector), column, selector);
        }
        let first = bounds(visual, "project-model-option-0-0");
        assert_eq!(first.size.height, px(28.));
        // Neither list can be clipped by the panel's overflow mask.
        let choices = bounds(visual, "project-model-choices");
        let rail = bounds(visual, "project-model-provider-rail");
        inside(choices, panel, "models");
        inside(rail, panel, "providers");
        assert_eq!(rail.bottom(), choices.bottom());
        assert_eq!(column.bottom(), choices.bottom());
        inside(
            bounds(visual, "project-model-show-current"),
            rail,
            "selected model action",
        );
        assert_eq!(
            view.read_with(visual, |view, _| view.model_ui.option_scroll.offset().y),
            px(0.),
            "ordinary settings need no scroll to reveal Fast Mode"
        );
    }
    assert!(gateway.model_writes.lock().unwrap().is_empty());
    assert!(gateway.sends.lock().unwrap().is_empty());
}
