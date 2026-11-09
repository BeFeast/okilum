use super::*;

fn project(root: &Path, directory: &str) {
    fs::create_dir_all(root.join(directory)).unwrap();
    fs::write(
        root.join(directory).join("project.yaml"),
        "# retained\nname: Synthetic\nkind: knowledge\n",
    )
    .unwrap();
}

#[test]
fn hidden_and_legacy_are_whole_layouts_and_icon_settings_do_not_select_one() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    assert_eq!(ServiceLayout::resolve(root).unwrap().kind, Kind::Hidden);
    project(root, "ops");
    fs::create_dir(root.join(".okilum")).unwrap();
    fs::write(root.join(".okilum/vault.json"), "settings").unwrap();
    assert_eq!(ServiceLayout::resolve(root).unwrap().kind, Kind::Legacy);
    fs::write(root.join(".okilum/board-local.toml"), "version=1\ncards=[]").unwrap();
    assert!(ServiceLayout::resolve(root)
        .unwrap_err()
        .to_string()
        .contains("Conflicting"));
    fs::remove_file(root.join(".okilum/board-local.toml")).unwrap();
    project(root, ".okilum");
    assert!(
        ServiceLayout::resolve(root).is_err(),
        "identical duplicate manifests are still a conflict"
    );
}

#[test]
fn invalid_preferred_files_or_orphans_never_fall_back_or_enable_creation() {
    let t = tempfile::tempdir().unwrap();
    project(t.path(), ".okilum");
    fs::write(t.path().join(".okilum/project.toml"), "not = [ valid").unwrap();
    assert!(ServiceLayout::resolve(t.path()).is_err());
    assert!(
        crate::projects::create::is_project(t.path()),
        "invalid metadata cannot invite competing creation"
    );
    fs::remove_file(t.path().join(".okilum/project.toml")).unwrap();
    fs::remove_file(t.path().join(".okilum/project.yaml")).unwrap();
    fs::write(t.path().join(".okilum/board.toml"), "version=1").unwrap();
    assert!(ServiceLayout::resolve(t.path())
        .unwrap_err()
        .to_string()
        .contains("Orphan"));
}

#[test]
fn conflict_copies_and_stale_location_writes_are_visible() {
    let t = tempfile::tempdir().unwrap();
    project(t.path(), "ops");
    let doc = crate::area_files::read(t.path(), "project")
        .unwrap()
        .unwrap();
    fs::rename(t.path().join("ops"), t.path().join(".okilum")).unwrap();
    assert!(doc.replace("name: Changed\n").is_err());
    assert!(!t.path().join("ops").exists());
    fs::write(
        t.path().join(".okilum/project.sync-conflict-other.yaml"),
        "name: Other\n",
    )
    .unwrap();
    assert!(ServiceLayout::resolve(t.path())
        .unwrap_err()
        .to_string()
        .contains("Conflicting service file"));
}

#[cfg(unix)]
#[test]
fn symlink_preferred_file_or_directory_is_not_a_legacy_fallback() {
    use std::os::unix::fs::symlink;
    let t = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    project(t.path(), "ops");
    symlink(outside.path(), t.path().join(".okilum")).unwrap();
    assert!(ServiceLayout::resolve(t.path()).is_err());
    fs::remove_file(t.path().join(".okilum")).unwrap();
    fs::write(outside.path().join("project.toml"), "kind='knowledge'").unwrap();
    symlink(
        outside.path().join("project.toml"),
        t.path().join("ops/project.toml"),
    )
    .unwrap();
    assert!(ServiceLayout::resolve(t.path()).is_err());
}

#[test]
fn nested_hidden_discovery_retains_registry_authority_and_does_not_make_root_ops() {
    let t = tempfile::tempdir().unwrap();
    let area = t.path().join("Areas/work-system");
    fs::create_dir_all(&area).unwrap();
    project(&area, ".okilum");
    fs::write(
        area.join(".okilum/thread-ids.md"),
        "# Threads\n\n| Role | Thread |\n|---|---|\n| Manager | `mcp:existing-manager` |\n",
    )
    .unwrap();
    let discovery = crate::projects::discover(t.path());
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    assert_eq!(discovery.projects.len(), 1);
    assert_eq!(discovery.projects[0].folder, area);
    assert_eq!(
        discovery.projects[0].manager,
        crate::projects::Manager::Thread("mcp:existing-manager".into())
    );
    assert!(!t.path().join("ops").exists());
}

#[test]
fn new_creation_uses_hidden_metadata_and_preserves_existing_icon() {
    let t = tempfile::tempdir().unwrap();
    fs::create_dir(t.path().join(".okilum")).unwrap();
    fs::write(t.path().join(".okilum/icon.png"), b"existing icon").unwrap();
    crate::projects::create::create(
        t.path(),
        t.path(),
        &crate::projects::create::Draft::for_folder(t.path()),
    )
    .unwrap();
    assert!(t.path().join(".okilum/project.toml").is_file());
    assert_eq!(
        fs::read(t.path().join(".okilum/icon.png")).unwrap(),
        b"existing icon"
    );
    assert!(!t.path().join("ops").exists());
}
