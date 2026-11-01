use super::*;
use crate::models::Target;
use serde_json::json;

fn snapshot() -> Snapshot {
    Snapshot {
        target: Target { thread_id: "qa-model-options".into(), project_id: "qa-project".into() },
        configuration: serde_json::from_value(json!({"threadId":"qa-model-options", "runtimeMode":"full-access", "interactionMode":"default",
            "modelSelection":{"instanceId":"account-a","model":"provider/raw-model","options":[
                {"id":"depthProfile","value":"detailed"},{"id":"responseStyle","value":"classic"},{"id":"accelerated","value":true}]}})).unwrap(),
        catalog: serde_json::from_value(json!({"runtimeMode":"full-access","providers":[
            {"providerInstanceId":"account-a","driverKind":"same-driver","displayName":"Work account","models":[{
                "id":"provider/raw-model","label":"A full readable model name","shortName":"Readable","subProvider":"Remote vendor",
                "capabilities":{"optionDescriptors":[
                    {"type":"select","id":"depthProfile","label":"Depth","description":"How much analysis to perform","options":[
                        {"id":"quiet","label":"Quiet","isDefault":true},{"id":"detailed","label":"Detailed","description":"More analysis"},
                        {"id":"draft-special","label":"Special prompt mode"}],"promptInjectedValues":["draft-special"]},
                    {"type":"select","id":"responseStyle","label":"Response style","currentValue":"terse","options":[
                        {"id":"classic","label":"Classic","isDefault":true},{"id":"terse","label":"Terse"}]},
                    {"type":"boolean","id":"accelerated","label":"Accelerated","description":"Optional acceleration","currentValue":false},
                    {"type":"future-slider","id":"not-a-supported-control","label":"Future control"}
                ]}
            }]},
            {"providerInstanceId":"account-b","driverKind":"same-driver","displayName":"Personal account","models":[{
                "id":"provider/raw-model","label":"Other account model","shortName":"Personal","capabilities":{"optionDescriptors":[]}
            }]}
        ]})).unwrap(),
    }
}

#[test]
fn controls_keep_server_types_defaults_labels_and_explicit_overrides() {
    let snapshot = snapshot();
    let selection = &snapshot.configuration.model_selection;
    let controls = snapshot.option_controls(selection);
    assert_eq!(
        controls.len(),
        3,
        "unknown descriptor kinds are not invented as controls"
    );
    assert_eq!(controls[0].id, "depthProfile");
    assert_eq!(controls[0].current, Some(json!("detailed")));
    assert_eq!(
        controls[0].description.as_deref(),
        Some("How much analysis to perform")
    );
    assert_eq!(controls[0].choices[0].label, "Quiet");
    assert!(controls[0].choices[0].is_default);
    assert!(controls[0].choices[2].prompt_injected);
    assert_eq!(
        controls[1].current,
        Some(json!("classic")),
        "explicit choice beats server currentValue and default"
    );
    assert_eq!(controls[2].current, Some(json!(true)));
    assert_eq!(
        controls[2]
            .choices
            .iter()
            .map(|choice| &choice.value)
            .collect::<Vec<_>>(),
        [&json!(false), &json!(true)]
    );
    assert_eq!(
        snapshot.option_summary(selection),
        "Detailed · Classic · Accelerated On"
    );
    let mut inherited = selection.clone();
    inherited.options.clear();
    let controls = snapshot.option_controls(&inherited);
    assert_eq!(
        controls[0].current,
        Some(json!("quiet")),
        "isDefault is used only without an explicit or server current value"
    );
    assert_eq!(
        controls[1].current,
        Some(json!("terse")),
        "server currentValue precedes isDefault"
    );
    assert_eq!(controls[2].current, Some(json!(false)));
}

#[test]
fn changing_one_supported_option_preserves_other_explicit_choices_and_exact_account() {
    let snapshot = snapshot();
    let selection = &snapshot.configuration.model_selection;
    let changed = snapshot
        .change_option(selection, "depthProfile", &json!("quiet"))
        .unwrap();
    assert_eq!(changed.instance_id, selection.instance_id);
    assert_eq!(changed.model, selection.model);
    assert_eq!(changed.options.len(), 3);
    assert_eq!(
        changed
            .options
            .iter()
            .find(|option| option.id == "responseStyle")
            .unwrap()
            .value,
        "classic"
    );
    assert_eq!(
        changed
            .options
            .iter()
            .find(|option| option.id == "accelerated")
            .unwrap()
            .value,
        true
    );
    let changed = snapshot
        .change_option(&changed, "accelerated", &json!(false))
        .unwrap();
    assert_eq!(
        changed
            .options
            .iter()
            .find(|option| option.id == "depthProfile")
            .unwrap()
            .value,
        "quiet"
    );
    assert!(snapshot.validate(&changed).is_ok());
    assert_eq!(
        snapshot.selection(&selection.instance_id, &selection.model),
        *selection
    );
    let other = snapshot.selection("account-b", &selection.model);
    assert!(
        other.options.is_empty(),
        "same-driver accounts never inherit each other's choices"
    );
    assert!(snapshot.option_controls(&other).is_empty());
    assert!(snapshot
        .change_option(&other, "depthProfile", &json!("quiet"))
        .is_err());
}

#[test]
fn unsupported_ambiguous_and_prompt_only_values_never_become_wire_options() {
    let mut snapshot = snapshot();
    let selection = snapshot.configuration.model_selection.clone();
    for (id, value) in [
        ("depthProfile", json!(false)),
        ("depthProfile", json!("not-advertised")),
        ("depthProfile", json!("draft-special")),
        ("accelerated", json!("true")),
        ("not-a-supported-control", json!("anything")),
    ] {
        assert!(snapshot.change_option(&selection, id, &value).is_err());
        let mut unsupported = selection.clone();
        unsupported.options.retain(|option| option.id != id);
        unsupported.options.push(ModelOption {
            id: id.into(),
            value,
        });
        assert!(snapshot.validate(&unsupported).is_err());
    }
    let caps = snapshot.catalog.providers[0].models[0]
        .capabilities
        .as_mut()
        .unwrap();
    caps.option_descriptors
        .push(caps.option_descriptors[0].clone());
    assert!(snapshot
        .change_option(&selection, "depthProfile", &json!("quiet"))
        .is_err());
    assert!(snapshot.validate(&selection).is_err());
}

#[test]
fn model_names_and_search_use_server_metadata_without_losing_instance_identity() {
    let mut snapshot = snapshot();
    let rows = snapshot.picker_choices("account-a", "");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].label, "Readable");
    assert_eq!(rows[0].sub_provider.as_deref(), Some("Remote vendor"));
    assert_eq!(rows[0].provider_label, "Work account");
    assert!(rows[0].current);
    let vendor = snapshot.picker_choices("account-b", "remote vendor");
    assert_eq!(vendor.len(), 1);
    assert_eq!(vendor[0].selection.instance_id, "account-a");
    assert_eq!(
        snapshot.picker_choices("account-a", "full readable").len(),
        1
    );
    assert_eq!(
        snapshot
            .picker_choices("account-a", "provider/raw-model")
            .len(),
        2
    );
    let current = snapshot.configuration.model_selection.clone();
    let model = &mut snapshot.catalog.providers[0].models[0];
    model.short_name = Some(" ".into());
    assert_eq!(model.display_name(), "A full readable model name");
    model.label = None;
    assert_eq!(model.display_name(), current.model);
    let duplicate = snapshot.catalog.providers[0].clone();
    snapshot.catalog.providers.push(duplicate);
    assert!(
        snapshot.model_info(&current).is_none(),
        "duplicate provider instances stay ambiguous"
    );
}

#[test]
fn legacy_catalog_does_not_invent_controls_and_descriptor_absence_is_not_an_empty_catalog() {
    let mut snapshot = snapshot();
    snapshot.catalog.providers[0].models[0].capabilities = None;
    let current = &snapshot.configuration.model_selection;
    assert!(
        snapshot.validate(current).is_ok(),
        "retain legacy server options when capabilities are unavailable"
    );
    assert!(snapshot.option_controls(current).is_empty());
    assert!(snapshot
        .change_option(current, "depthProfile", &json!("quiet"))
        .is_err());
    snapshot.catalog.providers[0].models[0].capabilities = Some(Default::default());
    assert!(
        snapshot
            .validate(&snapshot.configuration.model_selection)
            .is_err(),
        "an advertised empty descriptor list exposes no ordinary options"
    );
}
