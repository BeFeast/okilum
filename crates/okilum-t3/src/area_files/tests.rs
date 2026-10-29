use super::*;
struct Area(PathBuf);
impl Area {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("okilum-formats-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(p.join("ops")).unwrap();
        Self(p)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join("ops").join(name)
    }
}
impl Drop for Area {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn toml_has_precedence_and_only_absence_permits_yaml_fallback() {
    let a = Area::new();
    std::fs::write(a.file("project.yaml"), "name: Legacy\nkind: knowledge\n").unwrap();
    assert_eq!(crate::projects::Project::read(&a.0).unwrap().name, "Legacy");
    assert!(
        !a.file("project.toml").exists(),
        "a reader must not migrate a file"
    );
    std::fs::write(
        a.file("project.toml"),
        "name = 'TOML'\nkind = 'knowledge'\n",
    )
    .unwrap();
    assert_eq!(crate::projects::Project::read(&a.0).unwrap().name, "TOML");
    assert!(crate::projects::create::is_project(&a.0));
    let discovered = crate::projects::discover(&a.0);
    assert_eq!(discovered.projects.len(), 1);
    assert_eq!(discovered.projects[0].name, "TOML");
    std::fs::write(a.file("project.toml"), "name = [invalid").unwrap();
    assert!(
        crate::projects::Project::read(&a.0).is_err(),
        "bad TOML cannot select healthy YAML"
    );
    std::fs::remove_file(a.file("project.toml")).unwrap();
    assert_eq!(crate::projects::Project::read(&a.0).unwrap().name, "Legacy");
}

#[test]
fn toml_nullable_manager_keys_are_omitted_without_weakening_yaml() {
    let a = Area::new();
    let source="name = 'Knowledge'\nkind = 'knowledge'\n[t3]\nschema_version = 1\nenvironment = 'maestro'\n[t3.manager]\nworkspace_mode = 'local'\n";
    std::fs::write(a.file("project.toml"), source).unwrap();
    let project = crate::projects::Project::read(&a.0).unwrap();
    let binding = project.native_binding().unwrap().unwrap();
    assert!(binding.manager.thread_id.is_none() && binding.manager.project_id.is_none());
    let yaml="t3: {schema_version: 1, environment: maestro, manager: {workspace_mode: local}, executor: {project_id: p}, default_models: {}}";
    assert!(crate::manifest::AreaBinding::parse(yaml).is_err());
}

#[test]
fn selected_format_edits_preserve_comments_other_fields_and_legacy_bytes() {
    let a = Area::new();
    let legacy = "# Old readers\nname: Legacy\nkind: knowledge\n";
    std::fs::write(a.file("project.yaml"), legacy).unwrap();
    let before="# Project identity\nname = 'Before' # retained note\nkind = 'knowledge'\n[metadata]\nowner = 'Human' # leave this byte sequence\n";
    std::fs::write(a.file("project.toml"), before).unwrap();
    let doc = read(&a.0, "project").unwrap().unwrap();
    let updated = doc
        .set_toml_field("name", Some(&Value::from("After")))
        .unwrap();
    assert!(updated.contains("# Project identity"));
    assert!(updated.contains("owner = 'Human' # leave this byte sequence"));
    doc.replace(&updated).unwrap();
    assert_eq!(crate::projects::Project::read(&a.0).unwrap().name, "After");
    assert_eq!(
        std::fs::read_to_string(a.file("project.yaml")).unwrap(),
        legacy
    );
    assert!(std::fs::read_dir(a.0.join("ops")).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}

#[test]
fn delayed_edit_refuses_a_revision_or_format_change() {
    let a = Area::new();
    std::fs::write(a.file("project.yaml"), "name: Before\n").unwrap();
    let old = read(&a.0, "project").unwrap().unwrap();
    std::fs::write(a.file("project.yaml"), "name: Other writer\n").unwrap();
    assert!(old.replace("name: Stale\n").is_err());
    assert_eq!(
        std::fs::read_to_string(a.file("project.yaml")).unwrap(),
        "name: Other writer\n"
    );
    let old = read(&a.0, "project").unwrap().unwrap();
    std::fs::write(a.file("project.toml"), "name = 'Preferred'\n").unwrap();
    assert!(old.replace("name: Stale\n").is_err());
    assert_eq!(
        read(&a.0, "project").unwrap().unwrap().value["name"].as_str(),
        Some("Preferred")
    );
}

#[test]
fn toml_manual_icon_and_t3_link_read_back_without_touching_legacy() {
    let a = Area::new();
    let source = "# keep\nname = 'Example'\nkind = 'knowledge'\n[metadata]\nowner = 'Human'\n";
    std::fs::write(a.file("project.toml"), source).unwrap();
    std::fs::write(a.file("project.yaml"), "name: Legacy\n").unwrap();
    let icon = crate::projects::icon::ProjectIcon::Emoji {
        emoji: "🦉".into()
    };
    crate::projects::icon::save_manifest_icon(&a.0, Some(&icon)).unwrap();
    assert_eq!(
        crate::projects::Project::read(&a.0)
            .unwrap()
            .appearance
            .icon,
        Some(icon)
    );
    crate::projects::icon::save_manifest_icon(&a.0, None).unwrap();
    assert!(crate::projects::Project::read(&a.0)
        .unwrap()
        .appearance
        .icon
        .is_none());
    let linked = crate::projects::create::link_t3(
        &a.0,
        &crate::projects::create::T3Link {
            environment: "maestro".into(),
            project_id: "new-project".into(),
        },
    )
    .unwrap();
    assert_eq!(
        linked.appearance.executor_project.as_deref(),
        Some("new-project")
    );
    let saved = std::fs::read_to_string(a.file("project.toml")).unwrap();
    assert!(saved.contains("# keep") && saved.contains("owner = 'Human'"));
    assert_eq!(
        std::fs::read_to_string(a.file("project.yaml")).unwrap(),
        "name: Legacy\n"
    );
}

#[cfg(unix)]
#[test]
fn preferred_symlink_cannot_redirect_to_legacy_or_an_outside_manifest() {
    let a = Area::new();
    let outside = Area::new();
    std::fs::write(outside.file("project.toml"), "name = 'Outside'").unwrap();
    std::fs::write(a.file("project.yaml"), "name: Healthy fallback").unwrap();
    std::os::unix::fs::symlink(outside.file("project.toml"), a.file("project.toml")).unwrap();
    assert!(read(&a.0, "project").is_err());
}
