use super::*;

fn provider(instance: &str, driver: &str, slug: &str) -> Value {
    json!({
        "instanceId": instance,
        "driver": driver,
        "displayName": instance,
        "enabled": true,
        "models": [{"slug": slug, "name": "Visible model"}],
        "supportedRuntimeModes": ["full-access"]
    })
}

fn initial() -> Value {
    json!({"version":1,"type":"snapshot","config":{
        "environment":{"environmentId":"environment-a","label":"A"},
        "providers":[provider("account-a","codex","shared-model"),
            provider("account-b","codex","shared-model"),
            {"enabled":false}]
    }})
}

fn chunk(values: Vec<Value>) -> Value {
    json!({"_tag":"Chunk","requestId":"1","values":values})
}

#[test]
fn actual_server_config_events_keep_provider_instances_and_supported_modes() {
    let changes = decode(
        &chunk(vec![
            initial(),
            json!({"version":1,"type":"providerStatuses","payload":{
                "providers":[provider("account-b","codex","new-model")]
            }}),
            json!({"version":1,"type":"settingsUpdated","payload":{"settings":{"providerInstances":[]}}}),
            json!({"version":1,"type":"keybindingsUpdated","payload":{}}),
            json!({"version":1,"type":"environmentThemesUpdated","payload":{}}),
            json!({"version":1,"type":"usageLimitSourcesUpdated","payload":{}}),
        ]),
        false,
    )
    .unwrap();
    let Update::Snapshot(snapshot) = &changes[0] else {
        panic!("initial config snapshot is independent of thread history");
    };
    assert_eq!(snapshot.environment.environment_id, "environment-a");
    assert_eq!(snapshot.providers.len(), 2);
    assert_eq!(snapshot.providers[0].provider_instance_id, "account-a");
    assert_eq!(snapshot.providers[1].provider_instance_id, "account-b");
    assert_eq!(snapshot.providers[0].runtime_modes, ["full-access"]);
    let Update::Providers(providers) = &changes[1] else {
        panic!("providerStatuses contains the complete provider replacement");
    };
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].models[0].id, "new-model");
    assert_eq!(changes[2], Update::Refresh);
    assert!(changes[3..].iter().all(|event| *event == Update::Unchanged));
}

#[test]
fn malformed_or_uninitialized_config_batches_are_rejected_atomically() {
    let row = provider("account-a", "codex", "a");
    let mut duplicate_model = row.clone();
    duplicate_model["models"] = json!([{"slug":"same"},{"slug":"same"}]);
    for bad in [
        json!({"version":2,"type":"settingsUpdated","payload":{"settings":{}}}),
        json!({"version":1,"type":"settingsUpdated","payload":{}}),
        json!({"version":1,"type":"providerStatuses","payload":{"providers":[row.clone(),row]}}),
        json!({"version":1,"type":"providerStatuses","payload":{"providers":[duplicate_model]}}),
        json!({"version":1,"type":"snapshot","config":{"environment":{"environmentId":"","label":"A"},"providers":[]}}),
    ] {
        assert!(decode(&chunk(vec![initial(), bad]), false).is_err());
    }
    assert!(decode(&chunk(vec![]), false).is_err());
    assert!(decode(
        &chunk(vec![
            json!({"version":1,"type":"providerStatuses","payload":{"providers":[]}})
        ]),
        false
    )
    .is_err());
    assert_eq!(
        decode(
            &chunk(vec![
                json!({"version":1,"type":"providerStatuses","payload":{"providers":[]}})
            ]),
            true
        )
        .unwrap(),
        [Update::Providers(Vec::new())]
    );
}

#[test]
fn warmup_and_provider_status_snapshots_preserve_names_and_typed_option_descriptors() {
    let mut row = provider("account-a", "same-driver", "raw-model-id");
    row["models"][0]["shortName"] = json!("Readable short name");
    row["models"][0]["subProvider"] = json!("Remote vendor");
    row["models"][0]["capabilities"] = json!({"optionDescriptors":[
        {"type":"select","id":"analysis-style","label":"Analysis style","description":"Provider-specific controls","currentValue":"balanced","options":[
            {"id":"balanced","label":"Balanced","isDefault":true},{"id":"deep","label":"Deep"},{"id":"draft-only","label":"Draft mode"}],"promptInjectedValues":["draft-only"]},
        {"type":"boolean","id":"economy","label":"Economy","currentValue":true}
    ]});
    let event =
        json!({"version":1,"type":"providerStatuses","payload":{"providers":[row.clone()]}});
    let updates = decode(&chunk(vec![initial(), event]), false).unwrap();
    let Update::Providers(providers) = &updates[1] else {
        panic!("provider replacement expected");
    };
    let model = &providers[0].models[0];
    assert_eq!(model.display_name(), "Readable short name");
    assert_eq!(model.sub_provider.as_deref(), Some("Remote vendor"));
    let caps = model.capabilities.as_ref().unwrap();
    assert_eq!(caps.option_descriptors.len(), 2);
    let crate::types::ModelOptionDescriptor::Select {
        current_value,
        prompt_injected_values,
        options,
        ..
    } = &caps.option_descriptors[0]
    else {
        panic!("select remains typed");
    };
    assert_eq!(current_value.as_deref(), Some("balanced"));
    assert_eq!(prompt_injected_values, &["draft-only"]);
    assert!(options[0].is_default);
    let crate::types::ModelOptionDescriptor::Boolean { current_value, .. } =
        &caps.option_descriptors[1]
    else {
        panic!("boolean remains typed");
    };
    assert_eq!(*current_value, Some(true));
    row["models"][0]["capabilities"]["optionDescriptors"][1]["currentValue"] =
        json!("not-a-boolean");
    assert!(
        decode(
            &chunk(vec![
                initial(),
                json!({"version":1,"type":"providerStatuses","payload":{"providers":[row]}})
            ]),
            false
        )
        .is_err(),
        "malformed typed metadata rejects the batch before publication/ACK"
    );
}
