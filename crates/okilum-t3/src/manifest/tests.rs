use super::*;

const VALID: &str = include_str!("../../tests/fixtures/area/ops/project.yaml");

#[test]
fn a_binding_uses_only_the_explicit_manager_pointer() {
    let binding = AreaBinding::parse(VALID).unwrap().unwrap();
    assert_eq!(
        binding.manager.thread_id.as_deref(),
        Some("mcp:poc-manager")
    );
    assert_eq!(binding.executor.project_id, "poc-executors");
    assert_eq!(binding.default_models.manager.model, "gpt-6.1-sol");
    let connections = [Environment::new("maestro", "https://t3.oklabs.uk").unwrap()];
    assert_eq!(binding.connection(&connections).unwrap(), &connections[0]);
    assert!(binding
        .connection(&[])
        .unwrap_err()
        .to_string()
        .contains("not connected"));
    assert!(binding
        .connection(&[connections[0].clone(), connections[0].clone()])
        .is_err());
}

#[test]
fn an_area_without_t3_is_unconnected() {
    assert_eq!(
        AreaBinding::parse("display_name: Example\nrepo: { full_name: example/repo }\n").unwrap(),
        None
    );
}

#[test]
fn an_explicitly_unbound_manager_is_valid_without_guessing_one() {
    let source = VALID
        .replace("poc-managers", "null")
        .replace("'mcp:poc-manager'", "null");
    let binding = AreaBinding::parse(&source).unwrap().unwrap();
    assert_eq!(binding.manager.project_id, None);
    assert_eq!(binding.manager.thread_id, None);
}

#[test]
fn unsupported_versions_and_partial_or_invalid_bindings_are_visible_errors() {
    for source in [
        VALID.replace("schema_version: 1", "schema_version: 2"),
        VALID.replace("  environment: maestro\n", ""),
        VALID.replace("    thread_id: 'mcp:poc-manager'\n", ""),
        VALID.replace("'mcp:poc-manager'", "null"),
        VALID.replace("maestro", "https://t3.oklabs.uk"),
        VALID.replace("workspace_mode: local", "workspace_mode: guess"),
        VALID.replace("poc-executors", "' '"),
        VALID.replace(
            "    executor: {instanceId: codex, model: gpt-6.1-sol}\n",
            "",
        ),
        "t3: null\n".into(),
        "t3: { schema_version: 1 }\n".into(),
        "[not, a, mapping]\n".into(),
    ] {
        assert!(AreaBinding::parse(&source).is_err(), "accepted {source}");
    }
}

#[test]
fn loading_an_area_preserves_its_manifest_and_does_not_create_missing_files() {
    let area = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/area");
    let path = area.join("ops/project.yaml");
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        AreaBinding::read(&area).unwrap(),
        AreaBinding::parse(VALID).unwrap()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let absent = area.join("not-an-area");
    assert!(!absent.exists());
    assert_eq!(AreaBinding::read(&absent).unwrap(), None);
    assert!(!absent.exists());
}
