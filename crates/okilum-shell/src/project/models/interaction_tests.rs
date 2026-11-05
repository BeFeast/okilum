//! Real popup interactions over the existing read-only fixture transport. These
//! verify controls and confirmed state; they make no native/compositor claim.
use super::*;
use ::core::prelude::v1::test;
use serde_json::{json, Value};

fn model(id: &str, short_name: &str, descriptors: Value) -> Value {
    json!({"id":id,"label":format!("Upstream · {id}"),"shortName":short_name,"subProvider":"OpenAI",
        "capabilities":{"optionDescriptors":descriptors}})
}
fn catalog(driver: &str, descriptors: Value) -> Value {
    json!({"runtimeMode":"full-access","providers":[
        {"providerInstanceId":"work-account","driverKind":driver,"displayName":"Work account",
            "models":[model("same-model","Reasoning Pro",descriptors),model("swift-model","Swift Mini",json!([]))]},
        {"providerInstanceId":"personal-account","driverKind":driver,"displayName":"Personal account",
            "models":[model("same-model","Personal Pro",json!([]))]}
    ]})
}
fn selection(instance: &str, model: &str, options: Value) -> ModelSelection {
    serde_json::from_value(json!({"instanceId":instance,"model":model,"options":options})).unwrap()
}
fn reasoning(prompt_injected: bool) -> Value {
    json!([
        {"type":"select","id":"reasoningEffort","label":"Reasoning","currentValue":"medium",
            "promptInjectedValues":if prompt_injected {json!(["ultrathink"])}else{json!([])},
            "options":[{"id":"low","label":"Low"},{"id":"high","label":"High"},
                {"id":"ultrathink","label":"Ultracode"}]},
        {"type":"boolean","id":"fastMode","label":"Fast","currentValue":false}
    ])
}
fn fixture(
    cx: &mut TestAppContext,
    catalog: Value,
    current: ModelSelection,
) -> (
    Entity<ProjectView>,
    &mut VisualTestContext,
    tempfile::TempDir,
    Arc<super::super::fixture::Fake>,
) {
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(super::super::fixture::Fake::default());
    *gateway.model_catalog.lock().unwrap() = Some(serde_json::from_value(catalog).unwrap());
    *gateway.model_current.lock().unwrap() = Some(current);
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            super::super::fixture::view(state.path().to_owned(), gateway.clone(), window, cx)
        });
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1000.), px(1000.)));
    visual.run_until_parked();
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    view.read_with(visual, |view, _| {
        assert!(
            view.models.is_some(),
            "positive control: connection warmup completed before opening"
        );
    });
    view.update_in(visual, |view, window, cx| {
        view.compose.update(cx, |input, cx| input.focus(window, cx));
    });
    visual.run_until_parked();
    (view, visual, state, gateway)
}
fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("visible control missing: {selector}"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
}
fn key(visual: &mut VisualTestContext, binding: &str) {
    visual.simulate_keystrokes(binding);
    visual.run_until_parked();
}
fn primary_key(visual: &mut VisualTestContext, binding: &str) {
    let primary = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    key(visual, &format!("{primary}-{binding}"));
}

#[gpui::test]
fn readable_rows_stars_and_keyboard_keep_exact_accounts_and_durable_favorites(
    cx: &mut TestAppContext,
) {
    let current = selection("work-account", "same-model", json!([]));
    let (view, visual, state, gateway) = fixture(cx, catalog("codex", json!([])), current);
    let warmed_reads = gateway.reads.lock().unwrap().len();
    // The advertised platform opening shortcut reaches the actual popover and
    // search field; open/close itself does not request model/thread reads.
    primary_key(visual, "shift-m");
    assert!(visual.debug_bounds("project-model-panel").is_some());
    assert!(visual.debug_bounds("project-model-label-0").is_some());
    assert!(visual
        .debug_bounds("project-model-provider-label-0")
        .is_some());
    view.read_with(visual, |view, cx| {
        let choices = view.visible_model_choices(cx);
        assert_eq!(
            choices[0].label, "Reasoning Pro",
            "server displayShortName replaces raw slug"
        );
        assert_eq!(choices[0].provider_label, "Work account");
        assert!(choices[0].current);
    });
    click(visual, "project-model-favorite-1");
    assert!(
        visual.debug_bounds("project-model-panel").is_some(),
        "starring leaves the popup open"
    );
    assert!(
        gateway.model_writes.lock().unwrap().is_empty(),
        "starring never chooses a model"
    );
    let restored = preferences::Prefs::load(state.path(), "https://t3.example.test").unwrap();
    assert!(restored.is_favorite("work-account", "swift-model"));
    assert!(!restored.is_favorite("personal-account", "swift-model"));
    click(visual, "project-model-favorites");
    view.read_with(visual, |view, cx| {
        let visible = view.visible_model_choices(cx);
        assert_eq!(
            visible.len(),
            2,
            "favorites retain quick access to the current model"
        );
        assert!(visible
            .iter()
            .any(|choice| choice.selection.model == "swift-model"));
    });
    key(visual, "escape");
    primary_key(visual, "shift-m");
    click(visual, "project-model-provider-1");
    view.read_with(visual, |view, cx| {
        let visible = view.visible_model_choices(cx);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].selection.instance_id, "personal-account");
        assert_eq!(visible[0].label, "Personal Pro");
        assert!(
            !view.model_is_favorite(&visible[0].selection),
            "same-driver accounts do not share stars"
        );
    });
    assert_eq!(
        gateway.reads.lock().unwrap().len(),
        warmed_reads,
        "two open/close cycles start zero reads"
    );
    primary_key(visual, "1");
    let writes = gateway.model_writes.lock().unwrap();
    assert_eq!(
        writes.len(),
        1,
        "the first visible selectable row gets Ctrl+1"
    );
    assert_eq!(writes[0].instance_id, "personal-account");
    assert_eq!(writes[0].model, "same-model");
    drop(writes);
    assert!(visual.debug_bounds("project-model-panel").is_none());
    primary_key(visual, "shift-m");
    assert!(
        visual.debug_bounds("project-model-current-0").is_some(),
        "confirmed current is checked after reopening"
    );
    assert!(
        gateway.sends.lock().unwrap().is_empty(),
        "model actions never send a chat message"
    );
}

#[gpui::test]
fn supported_reasoning_changes_preserve_other_options_and_confirm_the_compact_chip(
    cx: &mut TestAppContext,
) {
    let (view, visual, _state, gateway) = fixture(
        cx,
        catalog("codex", reasoning(false)),
        selection(
            "work-account",
            "same-model",
            json!([
                {"id":"reasoningEffort","value":"low"},{"id":"fastMode","value":true}
            ]),
        ),
    );
    click(visual, "project-manager-model");
    let column = visual
        .debug_bounds("project-model-option-column")
        .expect("model has a visible supported option column");
    let choices = visual.debug_bounds("project-model-choices").unwrap();
    assert!(
        column.left() >= choices.right(),
        "wide popup places reasoning to the right of models"
    );
    assert!(
        visual.debug_bounds("project-model-option-0-1").is_some(),
        "High comes from this model's descriptor"
    );
    click(visual, "project-model-option-0-1");
    let writes = gateway.model_writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0]
            .options
            .iter()
            .find(|option| option.id == "reasoningEffort")
            .unwrap()
            .value,
        "high"
    );
    assert_eq!(
        writes[0]
            .options
            .iter()
            .find(|option| option.id == "fastMode")
            .unwrap()
            .value,
        true,
        "effort does not discard another advertised option"
    );
    drop(writes);
    view.read_with(visual, |view, cx| {
        let snapshot = view.models.as_ref().unwrap();
        assert_eq!(
            snapshot.configuration.model_selection,
            gateway.model_current.lock().unwrap().clone().unwrap(),
            "server readback owns the current selection"
        );
        assert_eq!(view.model_chip_summary(snapshot, cx), "High · Fast");
        assert!(view.model_error.is_none());
    });
    assert!(visual.debug_bounds("project-model-current-0").is_some());
    key(visual, "escape");
    assert!(visual.debug_bounds("project-manager-model").is_some());
    primary_key(visual, "shift-m");
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.model_chip_summary(view.models.as_ref().unwrap(), cx),
            "High · Fast"
        )
    });
    assert_eq!(
        gateway.model_writes.lock().unwrap().len(),
        1,
        "reopening does not mutate configuration"
    );
    assert!(gateway.sends.lock().unwrap().is_empty());
}

#[gpui::test]
fn prompt_injected_reasoning_edits_the_visible_draft_and_never_becomes_a_wire_option(
    cx: &mut TestAppContext,
) {
    let (view, visual, _state, gateway) = fixture(
        cx,
        catalog("claudeAgent", reasoning(true)),
        selection(
            "work-account",
            "same-model",
            json!([
                {"id":"reasoningEffort","value":"low"},{"id":"fastMode","value":true}
            ]),
        ),
    );
    view.update_in(visual, |view, window, cx| {
        view.compose.update(cx, |input, cx| {
            input.set_value("Explain the result", window, cx)
        });
    });
    click(visual, "project-manager-model");
    click(visual, "project-model-option-0-2");
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.compose.read(cx).value().as_str(),
            "Ultrathink:\nExplain the result"
        );
        assert_eq!(
            view.model_chip_summary(view.models.as_ref().unwrap(), cx),
            "Ultracode · Fast"
        );
        assert!(
            view.model_open,
            "reasoning stays available for another choice"
        );
    });
    assert!(
        gateway.model_writes.lock().unwrap().is_empty(),
        "prompt-only value is not sent to server configuration"
    );
    assert!(
        gateway.sends.lock().unwrap().is_empty(),
        "editing reasoning does not submit the draft"
    );
    click(visual, "project-model-option-0-0");
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.compose.read(cx).value().as_str(),
            "Explain the result",
            "ordinary reasoning removes the injected prefix visibly"
        );
        assert_eq!(
            view.model_chip_summary(view.models.as_ref().unwrap(), cx),
            "Low · Fast"
        );
    });
    let writes = gateway.model_writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert!(!writes[0]
        .options
        .iter()
        .any(|option| option.value == "ultrathink"));
    assert_eq!(
        writes[0]
            .options
            .iter()
            .find(|option| option.id == "fastMode")
            .unwrap()
            .value,
        true
    );
    assert!(gateway.sends.lock().unwrap().is_empty());
}

#[gpui::test]
fn escape_dismisses_the_picker_when_model_access_or_sending_becomes_unavailable(
    cx: &mut TestAppContext,
) {
    let current = selection("work-account", "same-model", json!([]));
    let (view, visual, _state, gateway) = fixture(cx, catalog("codex", json!([])), current.clone());
    let draft = "Keep this **draft** while the connection changes.";
    for blocked in ["sending", "offline", "inactive", "model-loading"] {
        view.update_in(visual, |view, window, cx| {
            view.active = true;
            view.busy = false;
            view.model_busy = false;
            view.gateway = Some(gateway.clone());
            view.compose
                .update(cx, |input, cx| input.set_value(draft, window, cx));
            let target = view.manager_model_target().unwrap();
            view.open_models(target, window, cx);
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("project-model-panel").is_some(),
            "positive control: mounted picker before {blocked}"
        );
        view.update(visual, |view, cx| {
            match blocked {
                "sending" => view.busy = true,
                "offline" => view.gateway = None,
                "inactive" => view.active = false,
                "model-loading" => view.model_busy = true,
                _ => unreachable!(),
            }
            cx.notify();
        });
        visual.run_until_parked();
        key(visual, "escape");
        view.update_in(visual, |view, window, cx| {
            assert!(
                !view.model_open,
                "Escape must close the picker during {blocked}"
            );
            assert!(
                view.model_ui.overlay.is_none(),
                "deferred overlay lease must be released"
            );
            assert!(
                view.compose.read(cx).focus_handle(cx).is_focused(window),
                "Escape returns focus to the composer during {blocked}"
            );
            assert_eq!(view.compose.read(cx).value().as_str(), draft);
            assert_eq!(
                view.models.as_ref().unwrap().configuration.model_selection,
                current
            );
            assert!(view.model_candidate.is_none());
        });
        assert!(gateway.sends.lock().unwrap().is_empty());
        assert!(gateway.model_writes.lock().unwrap().is_empty());
    }
}

#[gpui::test]
fn reasoning_rows_follow_each_model_descriptor_and_disappear_without_reasoning(
    cx: &mut TestAppContext,
) {
    let effort = |values: &[&str]| {
        json!([
            {"type":"select","id":"effort","label":"Reasoning", "options":values.iter().map(|value|
                json!({"id":value,"label":value,"isDefault":*value=="high"})).collect::<Vec<_>>()},
            {"type":"boolean","id":"fastMode","label":"Fast","currentValue":false}
        ])
    };
    let mut data = catalog(
        "codex",
        effort(&["low", "medium", "high", "xhigh", "ultra"]),
    );
    data["providers"][0]["models"] = json!([
        model(
            "same-model",
            "Reasoning Pro",
            effort(&["low", "medium", "high", "xhigh", "ultra"])
        ),
        model("limited", "Limited effort", effort(&["low", "high"])),
        model(
            "plain",
            "No reasoning",
            json!([{"type":"boolean","id":"fastMode","label":"Fast","currentValue":false}])
        ),
        model(
            "empty",
            "Empty settings",
            json!([{"type":"select","id":"effort","label":"Reasoning","options":[]}])
        )
    ]);
    let (view, visual, _state, gateway) =
        fixture(cx, data, selection("work-account", "same-model", json!([])));
    click(visual, "project-manager-model");
    for (ordinal, expected) in [
        (1, vec!["low", "medium", "high", "xhigh", "ultra"]),
        (2, vec!["low", "high"]),
        (3, vec![]),
        (4, vec![]),
    ] {
        if ordinal > 1 {
            primary_key(visual, &ordinal.to_string());
            assert!(visual.debug_bounds("project-model-panel").is_none());
            primary_key(visual, "shift-m");
        }
        let controls = view.read_with(visual, |view, _| {
            ProjectView::picker_controls(view.models.as_ref().unwrap())
        });
        let values: Vec<_> = controls
            .iter()
            .find(|control| control.id == "effort")
            .map(|control| {
                control
                    .choices
                    .iter()
                    .map(|choice| choice.value.as_str().unwrap())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            values, expected,
            "same-provider models must use their own option values"
        );
        if ordinal < 3 {
            let last = format!("project-model-option-0-{}", expected.len() - 1);
            assert!(
                visual.debug_bounds(&last).is_some(),
                "positive control: last supported choice is rendered"
            );
            let extra = format!("project-model-option-0-{}", expected.len());
            assert!(
                visual.debug_bounds(&extra).is_none(),
                "no provider-wide extra reasoning levels"
            );
            let default = format!(
                "project-model-default-0-{}",
                expected.iter().position(|value| *value == "high").unwrap()
            );
            assert!(visual.debug_bounds(&default).is_some());
        } else if ordinal == 3 {
            assert_eq!(controls.len(), 1);
            assert_eq!(
                controls[0].label, "Fast",
                "unrelated settings survive without a Reasoning section"
            );
            assert!(visual.debug_bounds("project-model-option-0-1").is_some());
            assert!(visual
                .debug_bounds("project-model-option-heading-1")
                .is_none());
        } else {
            assert!(visual
                .debug_bounds("project-model-option-heading-0")
                .is_none());
            assert!(visual.debug_bounds("project-model-no-options").is_some());
        }
    }
    assert_eq!(gateway.model_writes.lock().unwrap().len(), 3);
    assert!(gateway.sends.lock().unwrap().is_empty());
}

#[gpui::test]
fn invalid_prompt_choices_are_hidden_instead_of_becoming_ultrathink(cx: &mut TestAppContext) {
    let descriptors = json!([{"type":"select","id":"effort","label":"Reasoning",
        "promptInjectedValues":["ultrathink", "future-prompt-mode"], "options":[
            {"id":"high","label":"High","isDefault":true},
            {"id":"ultracode","label":"Ultracode"},
            {"id":"ultrathink","label":"Ultrathink"},
            {"id":"future-prompt-mode","label":"Future prompt mode"}]}]);
    let (view, visual, _state, gateway) = fixture(
        cx,
        catalog("claudeAgent", descriptors),
        selection("work-account", "same-model", json!([])),
    );
    click(visual, "project-manager-model");
    assert!(
        visual.debug_bounds("project-model-option-0-2").is_some(),
        "advertised Claude Ultrathink is available"
    );
    assert!(
        visual.debug_bounds("project-model-option-0-3").is_none(),
        "unknown prompt semantics cannot edit the draft"
    );
    view.update(visual, |view, cx| {
        view.models.as_mut().unwrap().catalog.providers[0].driver_kind = "codex".into();
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-model-option-0-1").is_some(),
        "ordinary advertised values remain selectable"
    );
    assert!(
        visual.debug_bounds("project-model-option-0-2").is_none(),
        "Claude prompt injection is unavailable on another driver"
    );
    assert!(gateway.model_writes.lock().unwrap().is_empty());
    assert!(gateway.sends.lock().unwrap().is_empty());
}
