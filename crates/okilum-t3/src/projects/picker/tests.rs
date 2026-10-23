use super::*;

struct Vault(PathBuf);
impl Vault {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("okilum-picker-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn manifest(&self, relative: &str, source: &str) -> PathBuf {
        let folder = self.0.join(relative);
        std::fs::create_dir_all(folder.join("ops")).unwrap();
        std::fs::write(folder.join("ops/project.yaml"), source).unwrap();
        folder
    }
    fn model(&self, recent: Vec<PathBuf>) -> PickerModel {
        PickerModel::new(self.0.clone(), super::super::discover(&self.0), recent)
    }
}
impl Drop for Vault {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn manifests_group_by_domain_across_para_and_keep_parser_kind_defaults() {
    let vault = Vault::new();
    vault.manifest(
        "Dev/Areas/Editor",
        "name: Editor\nrepo: {full_name: example/editor}\n",
    );
    vault.manifest("Music/Areas/Studio", "name: Studio\n");
    vault.manifest("Music/Projects/Album", "name: Album\nkind: hybrid\n");
    vault.manifest("HomeLab/Projects/Mesh", "name: Mesh\nkind: knowledge\n");
    let model = vault.model(Vec::new());
    let groups = model.groups();
    assert_eq!(
        groups
            .iter()
            .map(|g| (g.label.as_str(), g.count))
            .collect::<Vec<_>>(),
        [("Dev", 1), ("HomeLab", 1), ("Music", 2)]
    );
    assert_eq!(groups[0].rows[0].project.kind, Kind::Code);
    assert_eq!(groups[2].rows[0].project.name, "Album");
    assert_eq!(groups[2].rows[1].project.kind, Kind::Knowledge);
}

#[test]
fn recents_are_most_recent_first_bounded_and_distinct_from_domain_rows() {
    let vault = Vault::new();
    let alpha = vault.manifest("Dev/Areas/A", "name: alpha\n");
    let beta = vault.manifest("Dev/Areas/B", "name: Beta\n");
    let gamma = vault.manifest("Music/Projects/G", "name: Gamma\n");
    let mut model = vault.model(vec![
        gamma.clone(),
        beta.clone(),
        gamma.clone(),
        vault.0.join("missing"),
    ]);
    let groups = model.groups();
    assert_eq!(groups[0].label, "Recent");
    assert_eq!(
        groups[0]
            .rows
            .iter()
            .map(|r| r.project.name.as_str())
            .collect::<Vec<_>>(),
        ["Gamma", "Beta"]
    );
    assert_eq!(
        groups[1]
            .rows
            .iter()
            .map(|r| r.project.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "Beta"]
    );
    assert_ne!(groups[0].rows[1].key, groups[1].rows[1].key);
    assert!(!model.record_opened(&vault.0.join("unknown")));
    assert!(model.record_opened(&alpha));
    assert_eq!(model.recent()[0], alpha);
    assert_eq!(model.groups()[0].rows[0].project.name, "alpha");
    for i in 0..8 {
        vault.manifest(&format!("Life/Projects/{i}"), &format!("name: Item{i}\n"));
    }
    model.refresh(super::super::discover(&vault.0));
    for i in 0..8 {
        assert!(model.record_opened(&vault.0.join(format!("Life/Projects/{i}"))));
    }
    assert_eq!(model.groups()[0].rows.len(), RECENT_LIMIT);
    assert!(
        model.groups()[0]
            .rows
            .iter()
            .map(|r| &r.key.folder)
            .collect::<BTreeSet<_>>()
            .len()
            == RECENT_LIMIT
    );
}

#[test]
fn search_and_kind_filter_apply_to_recents_and_every_domain() {
    let vault = Vault::new();
    let studio = vault.manifest("Music/Areas/Studio", "name: Студия\nkind: knowledge\n");
    vault.manifest("Music/Projects/Album", "name: Album\nkind: hybrid\n");
    vault.manifest("Dev/Areas/Editor", "name: Editor\nkind: code\n");
    let mut model = vault.model(vec![studio]);
    model.set_query("MUSIC студ");
    assert_eq!(model.groups().len(), 2);
    assert_eq!(model.groups()[1].rows[0].project.name, "Студия");
    model.set_filter(KindFilter::Hybrid);
    assert!(model.groups().is_empty());
    model.set_query("music projects");
    let groups = model.groups();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].rows[0].project.name, "Album");
    model.set_query("  ");
    model.set_filter(KindFilter::Code);
    assert_eq!(model.groups()[0].label, "Dev");
}

#[test]
fn search_reveals_collapsed_matches_and_restores_the_preference() {
    let vault = Vault::new();
    vault.manifest("Dev/Areas/Editor", "name: Editor\n");
    let mut model = vault.model(Vec::new());
    model.toggle_domain("Dev");
    assert!(model.groups()[0].collapsed);
    assert!(model.groups()[0].rows.is_empty());
    assert_eq!(model.groups()[0].count, 1);
    model.set_query("editor");
    assert!(!model.groups()[0].collapsed);
    assert_eq!(model.groups()[0].rows.len(), 1);
    model.set_query("");
    assert!(model.groups()[0].collapsed);
}

#[test]
fn status_needs_a_live_observation_of_the_current_manager_pointer() {
    let vault = Vault::new();
    let project = vault.manifest(
        "Dev/Areas/App",
        "name: App\nt3:\n  manager: {thread_id: 'mcp:new'}\n",
    );
    let mut model = vault.model(Vec::new());
    let indicator = |m: &PickerModel| m.groups()[0].rows[0].manager;
    assert_eq!(indicator(&model), ManagerIndicator::Checking);
    model.set_observations(BTreeMap::from([(
        project.clone(),
        ManagerObservation {
            manager: Manager::Thread("mcp:old".into()),
            status: Status::Running("running".into()),
        },
    )]));
    assert_eq!(indicator(&model), ManagerIndicator::Checking);
    for (status, expected) in [
        (Status::Running("queued".into()), ManagerIndicator::Running),
        (Status::NotRunning("idle".into()), ManagerIndicator::Idle),
        (
            Status::Unreachable("offline".into()),
            ManagerIndicator::Unreachable,
        ),
        (Status::Settled, ManagerIndicator::NotStarted),
    ] {
        model.set_observations(BTreeMap::from([(
            project.clone(),
            ManagerObservation {
                manager: Manager::Thread("mcp:new".into()),
                status,
            },
        )]));
        assert_eq!(indicator(&model), expected);
    }
    std::fs::write(
        project.join("ops/thread-ids.md"),
        "| Manager | — (not started) |\n",
    )
    .unwrap();
    model.refresh(super::super::discover(&vault.0));
    assert_eq!(indicator(&model), ManagerIndicator::NotStarted);
}

#[test]
fn broken_manifests_stay_visible_and_external_paths_never_become_rows() {
    let vault = Vault::new();
    vault.manifest("Dev/Areas/Broken", "kind: [code]\n");
    vault.manifest("Dev/Areas/intake", "kind: operating-area\n");
    let mut discovery = super::super::discover(&vault.0);
    discovery.projects.push(Project {
        folder: vault.0.join("../foreign"),
        name: "Foreign".into(),
        kind: Kind::Code,
        environment: None,
        manager_project: None,
        manager: Manager::NotStarted,
    });
    let mut model = PickerModel::new(vault.0.clone(), discovery, Vec::new());
    assert_eq!(model.errors().len(), 2);
    let groups = model.groups();
    assert!(groups.iter().all(|g| g.rows.is_empty()), "no project rows");
    // Each problem is a muted row inside its own domain, never a footer;
    // the non-project intake area is skipped without a problem row.
    let problems: Vec<_> = groups
        .iter()
        .flat_map(|g| {
            g.problems
                .iter()
                .map(move |p| (g.label.as_str(), p.name.as_str()))
        })
        .collect();
    assert_eq!(problems, [("Dev", "Broken"), ("Vault", "foreign")]);
    assert!(groups[0].problems[0]
        .detail
        .contains("invalid project kind"));
    model.set_query("broken");
    assert_eq!(model.groups().len(), 1);
    model.set_query("");
    model.set_filter(KindFilter::Code);
    assert!(
        model.groups().is_empty(),
        "unknown kind hides under a kind filter"
    );
}
