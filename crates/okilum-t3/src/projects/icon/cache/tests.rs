use super::*;
use crate::projects::icon::{IconColor, ProjectIcon};
use crate::projects::{Appearance, Kind, Manager};

fn fixture() -> (
    PathBuf,
    Project,
    BTreeMap<String, String>,
    CounterpartRefresh,
) {
    let root = std::env::temp_dir().join(format!("okilum-icon-cache-{}", uuid::Uuid::new_v4()));
    let project = Project {
        folder: root.join("vault/App"),
        name: "App".into(),
        kind: Kind::Knowledge,
        environment: Some("maestro".into()),
        manager_project: Some("brain".into()),
        manager: Manager::NotStarted,
        appearance: Appearance {
            executor_project: Some("app".into()),
            ..Default::default()
        },
    };
    let origins = BTreeMap::from([("maestro".into(), "https://t3.example".into())]);
    let counterpart = Counterpart {
        environment: "maestro".into(),
        project_id: "app".into(),
        icon: Some(ProjectIcon::Lucide {
            name: "calculator".into(),
            color: IconColor::Green,
        }),
        logo: None,
    };
    let refresh = CounterpartRefresh {
        confirmed: [project.folder.clone()].into_iter().collect(),
        links: [(project.folder.clone(), counterpart)]
            .into_iter()
            .collect(),
    };
    (root, project, origins, refresh)
}

#[test]
fn last_known_icon_is_local_and_bound_to_the_server_and_project() {
    let (root, project, origins, refresh) = fixture();
    store(&root, std::slice::from_ref(&project), &origins, &refresh).unwrap();
    assert_eq!(
        load(&root, std::slice::from_ref(&project), &origins),
        refresh.links
    );
    let other = BTreeMap::from([("maestro".into(), "https://other.example".into())]);
    assert!(load(&root, std::slice::from_ref(&project), &other).is_empty());
    let mut rebound = project.clone();
    rebound.appearance.executor_project = Some("other".into());
    assert!(load(&root, &[rebound], &origins).is_empty());
    let mut moved = project.clone();
    moved.folder = root.join("another-vault/App");
    assert!(load(&root, &[moved], &origins).is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn offline_refresh_keeps_the_icon_but_confirmed_removal_clears_it() {
    let (root, project, origins, refresh) = fixture();
    store(&root, std::slice::from_ref(&project), &origins, &refresh).unwrap();
    store(
        &root,
        std::slice::from_ref(&project),
        &origins,
        &CounterpartRefresh::default(),
    )
    .unwrap();
    assert_eq!(
        load(&root, std::slice::from_ref(&project), &origins),
        refresh.links
    );
    let removed = CounterpartRefresh {
        confirmed: [project.folder.clone()].into_iter().collect(),
        ..Default::default()
    };
    store(&root, std::slice::from_ref(&project), &origins, &removed).unwrap();
    assert!(load(&root, &[project], &origins).is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cached_logos_survive_restart_and_corrupt_metadata_is_disposable() {
    let (root, project, origins, mut refresh) = fixture();
    std::fs::create_dir_all(&root).unwrap();
    let logo = root.join("logo.svg");
    std::fs::write(&logo, "<svg/>").unwrap();
    let link = refresh.links.get_mut(&project.folder).unwrap();
    link.icon = None;
    link.logo = Some(logo.clone());
    store(&root, std::slice::from_ref(&project), &origins, &refresh).unwrap();
    assert_eq!(
        load(&root, std::slice::from_ref(&project), &origins)[&project.folder].logo,
        Some(logo.clone())
    );
    std::fs::remove_file(logo).unwrap();
    assert_eq!(
        load(&root, std::slice::from_ref(&project), &origins)[&project.folder].logo,
        None
    );
    let file = Binding::new(&project, &origins).unwrap().path(&root);
    std::fs::write(file, b"partial cache entry").unwrap();
    assert!(load(&root, &[project], &origins).is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

fn stored(root: &Path, project: &Project, origins: &BTreeMap<String, String>) -> PathBuf {
    let refresh = CounterpartRefresh {
        confirmed: [project.folder.clone()].into_iter().collect(),
        links: [(
            project.folder.clone(),
            Counterpart {
                environment: "maestro".into(),
                project_id: project.name.to_lowercase(),
                icon: None,
                logo: None,
            },
        )]
        .into_iter()
        .collect(),
    };
    store(root, std::slice::from_ref(project), origins, &refresh).unwrap();
    let path = Binding::new(project, origins).unwrap().path(root);
    assert!(path.is_file(), "positive control: the entry was written");
    path
}

fn sibling(project: &Project, name: &str) -> Project {
    let mut sibling = project.clone();
    sibling.folder = project.folder.with_file_name(name);
    sibling.name = name.into();
    sibling.appearance.executor_project = Some(name.to_lowercase());
    sibling
}

#[test]
fn renamed_deleted_and_rebound_projects_lose_their_metadata() {
    let (root, project, origins, _) = fixture();
    let vault = root.join("vault");
    let deleted = sibling(&project, "Gone");
    let rebound = sibling(&project, "Rebound");
    let unbound = sibling(&project, "Unbound");
    let original = stored(&root, &project, &origins);
    let deleted_entry = stored(&root, &deleted, &origins);
    let rebound_entry = stored(&root, &rebound, &origins);
    let unbound_entry = stored(&root, &unbound, &origins);
    // `App` was renamed to `Renamed` and its entry is now stored there.
    let renamed = Project {
        folder: project.folder.with_file_name("Renamed"),
        ..project.clone()
    };
    let renamed_entry = stored(&root, &renamed, &origins);
    let mut rebound_now = rebound.clone();
    rebound_now.appearance.executor_project = Some("elsewhere".into());
    let mut unbound_now = unbound.clone();
    unbound_now.environment = None;
    let discovery = [renamed.clone(), rebound_now.clone(), unbound_now.clone()];

    assert_eq!(collect(&root, &vault, &discovery, &origins).unwrap(), 4);
    for gone in [&original, &deleted_entry, &rebound_entry, &unbound_entry] {
        assert!(!gone.exists(), "{gone:?} is obsolete");
    }
    assert!(renamed_entry.is_file());
    assert_eq!(
        load(&root, &discovery, &origins).keys().collect::<Vec<_>>(),
        [&renamed.folder]
    );
    // A second pass has nothing left to remove.
    assert_eq!(collect(&root, &vault, &discovery, &origins).unwrap(), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_partial_refresh_never_removes_another_active_binding() {
    let (root, project, origins, _) = fixture();
    let vault = root.join("vault");
    let other = sibling(&project, "Other");
    let other_entry = stored(&root, &other, &origins);
    // A one-project refresh (an icon save, an entered project) stores only
    // its own project, confirmed removal included.
    stored(&root, &project, &origins);
    let removed = CounterpartRefresh {
        confirmed: [project.folder.clone()].into_iter().collect(),
        ..Default::default()
    };
    store(&root, std::slice::from_ref(&project), &origins, &removed).unwrap();
    assert!(other_entry.is_file());
    let discovery = [project.clone(), other.clone()];
    assert_eq!(collect(&root, &vault, &discovery, &origins).unwrap(), 0);
    assert_eq!(
        load(&root, &discovery, &origins).keys().collect::<Vec<_>>(),
        [&other.folder]
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_second_vault_sharing_the_cache_keeps_its_bindings() {
    let (root, project, origins, _) = fixture();
    let vault = root.join("vault");
    // The other vault is not mounted now and is not discovered here; its
    // path even shares this vault's name as a prefix.
    let mut elsewhere = sibling(&project, "Elsewhere");
    elsewhere.folder = root.join("vault-2/Elsewhere");
    let elsewhere_entry = stored(&root, &elsewhere, &origins);
    // A folder inside this vault that discovery does not walk (a symlinked
    // vault, a broken manifest) still has its manifest on disk.
    let mut linked = sibling(&project, "Linked");
    linked.folder = vault.join("link/Linked");
    std::fs::create_dir_all(linked.folder.join("ops")).unwrap();
    std::fs::write(linked.folder.join("ops/project.yaml"), "name: Linked\n").unwrap();
    let linked_entry = stored(&root, &linked, &origins);
    let gone = stored(&root, &sibling(&project, "Gone"), &origins);

    assert_eq!(
        collect(&root, &vault, std::slice::from_ref(&project), &origins).unwrap(),
        1,
        "positive control: the deleted project in this vault is collected"
    );
    assert!(!gone.exists());
    assert!(elsewhere_entry.is_file());
    assert!(linked_entry.is_file());
    assert_eq!(load(&root, &[elsewhere, linked], &origins).len(), 2);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_unresolved_t3_origin_or_failed_refresh_keeps_cached_icons() {
    let (root, project, origins, refresh) = fixture();
    let vault = root.join("vault");
    store(&root, std::slice::from_ref(&project), &origins, &refresh).unwrap();
    store(
        &root,
        std::slice::from_ref(&project),
        &origins,
        &CounterpartRefresh::default(),
    )
    .unwrap();
    let gone = stored(&root, &sibling(&project, "Gone"), &origins);
    // The T3 connection could not be read: no origin, no binding to compare.
    assert_eq!(
        collect(
            &root,
            &vault,
            std::slice::from_ref(&project),
            &BTreeMap::new()
        )
        .unwrap(),
        1,
        "positive control: the deleted project is still collected"
    );
    assert!(!gone.exists());
    assert_eq!(
        load(&root, std::slice::from_ref(&project), &origins),
        refresh.links
    );
    // Nothing in the cache yet is not an error.
    let empty = root.join("empty");
    assert_eq!(collect(&empty, &vault, &[], &origins).unwrap(), 0);
    std::fs::remove_dir_all(root).unwrap();
}
