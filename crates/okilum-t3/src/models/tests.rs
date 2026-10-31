use super::*;
use serde_json::json;

fn snapshot() -> Snapshot {
    Snapshot {
        target: Target { thread_id: "qa".into(), project_id: "project".into() },
        configuration: serde_json::from_value(json!({"threadId":"qa","modelSelection":{"instanceId":"account-a","model":"shared","options":[{"id":"effort","value":"high"}]},"runtimeMode":"full-access","interactionMode":"default"})).unwrap(),
        catalog: serde_json::from_value(json!({"runtimeMode":"full-access","providers":[
            {"providerInstanceId":"account-a","driverKind":"codex","displayName":"Work Account","models":[{"id":"shared","label":"Reasoning Pro"},{"id":"other","label":"Swift Mini"}]},
            {"providerInstanceId":"account-b","driverKind":"codex","displayName":"Personal Account","models":[{"id":"shared","label":"Reasoning Pro"}]}
        ]})).unwrap(),
    }
}

#[test]
fn browse_preserves_exact_instance_and_current_options() {
    let snapshot = snapshot();
    let rows = snapshot.picker_choices("account-a", "");
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|row| row.selection.instance_id == "account-a"));
    assert_eq!(rows.iter().filter(|row| row.current).count(), 1);
    assert_eq!(rows[0].selection.options[0].value, "high");
    let other = snapshot.picker_choices("account-b", "");
    assert_eq!(other.len(), 1);
    assert!(!other[0].current);
    assert!(other[0].selection.options.is_empty());
}

#[test]
fn current_missing_from_one_account_is_not_taken_from_another() {
    let mut snapshot = snapshot();
    snapshot.catalog.providers[0].models.clear();
    let rows = snapshot.picker_choices("account-a", "");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].current && !rows[0].selectable);
    assert_eq!(rows[0].selection.instance_id, "account-a");
    snapshot.catalog.providers.remove(0);
    let rows = snapshot.picker_choices("account-a", "");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].current && !rows[0].selectable);
}

#[test]
fn search_spans_instances_with_provider_tokens_and_short_fuzzy_control() {
    let snapshot = snapshot();
    let rows = snapshot.picker_choices("account-a", "PERSONAL   rsng");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].selection.instance_id, "account-b");
    assert!(snapshot
        .picker_choices("account-a", "no such model")
        .is_empty());
    assert_eq!(snapshot.picker_choices("account-a", "").len(), 2);
    assert!(
        search::score("rp", &["Reasoning Pro"], false).is_none(),
        "two-character subsequences must not match"
    );
    assert!(search::score("rpro", &["Reasoning Pro"], false).is_some());
    assert!(
        search::score("reasoning", &["Reasoning"], false).unwrap()
            < search::score("reasoning", &["Reasoning Pro"], false).unwrap()
    );
}
