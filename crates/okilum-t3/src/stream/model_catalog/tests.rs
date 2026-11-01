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
