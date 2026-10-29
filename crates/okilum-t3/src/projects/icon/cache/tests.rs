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
