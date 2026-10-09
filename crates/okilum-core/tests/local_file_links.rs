use okilum_core::{
    document_links::{
        self,
        prepared::{LinkPreparation, LinkStatus, TargetSnapshot},
        HeadingInventory,
    },
    Vault,
};

fn fixture() -> (tempfile::TempDir, Vault) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("notes")).unwrap();
    std::fs::write(dir.path().join("notes/start.md"), "# Start").unwrap();
    std::fs::write(dir.path().join("notes/percent%23.json"), "{}").unwrap();
    let vault = Vault::scan_metadata(dir.path()).unwrap();
    (dir, vault)
}

#[test]
fn local_file_links_resolve_relative_absolute_and_file_urls_once() {
    let (dir, vault) = fixture();
    let mut prep = LinkPreparation::new(
        &vault,
        "notes/start.md",
        |_| -> Result<TargetSnapshot, String> {
            panic!("attachments must not read document source")
        },
    )
    .with_local_files();
    for link in [
        "percent%2523.json".to_owned(),
        document_links::encode(&dir.path().join("notes/percent%23.json").to_string_lossy()),
        url::Url::from_file_path(dir.path().join("notes/percent%23.json"))
            .unwrap()
            .to_string(),
    ] {
        let (resolved, state) = prep.link(&link, false);
        assert_eq!(state.status, LinkStatus::Resolved, "{link}: {state:?}");
        assert_eq!(resolved.candidates, ["notes/percent%23.json"]);
        assert_eq!(
            state.action_url.as_deref(),
            Some("tessera://attachment/notes/percent%2523.json")
        );
    }
}

#[test]
fn local_file_links_missing_and_refresh_preserve_original_rendered_identity() {
    let (dir, vault) = fixture();
    let identities = vec![document_links::prepared::LinkIdentity {
        from: "notes/start.md".into(),
        target: "missing.json".into(),
        wiki: false,
        url: "tessera://unresolved/missing.json".into(),
    }];
    let mut prep = LinkPreparation::new(
        &vault,
        "notes/start.md",
        |_| -> Result<TargetSnapshot, String> { panic!() },
    )
    .with_local_files();
    let first = prep.identities(&identities);
    let state = &first[&identities[0].url];
    assert_eq!(state.status, LinkStatus::MissingFile);
    assert_eq!(state.reason, "File not found");
    let copy = state
        .action_url
        .as_ref()
        .unwrap()
        .strip_prefix("tessera://missing-file/")
        .unwrap();
    assert_eq!(
        std::path::PathBuf::from(document_links::decode(copy)),
        dir.path()
            .canonicalize()
            .unwrap()
            .join("notes/missing.json")
    );
    std::fs::write(dir.path().join("notes/missing.json"), "{}").unwrap();
    // Preparation owns one generation; another job refreshes the same emitted URL.
    assert_eq!(
        prep.identities(&identities)[&identities[0].url].status,
        LinkStatus::MissingFile
    );
    let mut refreshed = LinkPreparation::new(
        &vault,
        "notes/start.md",
        |_| -> Result<TargetSnapshot, String> { panic!() },
    )
    .with_local_files();
    let next = refreshed.identities(&identities);
    assert_eq!(next[&identities[0].url].status, LinkStatus::Resolved);
    assert_eq!(
        next[&identities[0].url].action_url.as_deref(),
        Some("tessera://attachment/notes/missing.json")
    );
}

#[test]
fn local_file_links_outside_files_and_absence_need_no_vault_inventory() {
    let (_dir, mut vault) = fixture();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("existing.json"), "{}").unwrap();
    vault.inventory_complete = false;
    let mut prep = LinkPreparation::new(
        &vault,
        "notes/start.md",
        |_| -> Result<TargetSnapshot, String> { panic!() },
    )
    .with_local_files();
    for file_uri in [false, true] {
        for (name, expected) in [
            ("existing.json", LinkStatus::Resolved),
            ("missing.json", LinkStatus::MissingFile),
        ] {
            let path = outside.path().join(name);
            let link = if file_uri {
                url::Url::from_file_path(&path).unwrap().to_string()
            } else {
                document_links::encode(&path.to_string_lossy())
            };
            assert_eq!(prep.link(&link, false).1.status, expected);
        }
    }
    assert_eq!(
        prep.link("missing.json", false).1.status,
        LinkStatus::Unknown
    );
    assert_eq!(
        prep.link("https://example.invalid/file.json", false)
            .1
            .status,
        LinkStatus::External
    );
    assert_eq!(
        prep.link("file://server/share/file.json", false).1.status,
        LinkStatus::Unknown
    );
    assert_eq!(
        prep.link("//server/share/file.json", false).1.status,
        LinkStatus::Unknown
    );
}

#[test]
fn local_file_links_file_url_note_keeps_heading_validation_and_default_scope() {
    let (dir, vault) = fixture();
    let file = url::Url::from_file_path(dir.path().join("notes/start.md"))
        .unwrap()
        .to_string();
    let mut desktop = LinkPreparation::new(&vault, "notes/start.md", |_| {
        Ok(TargetSnapshot {
            revision: "same".into(),
            headings: HeadingInventory::new("# Start"),
            supports_setext: true,
            managed: None,
        })
    })
    .with_local_files();
    assert_eq!(
        desktop.link(&format!("{file}#Start"), false).1.status,
        LinkStatus::Resolved
    );
    assert_eq!(
        desktop.link(&format!("{file}#Absent"), false).1.status,
        LinkStatus::MissingHeading
    );
    let mut scoped = LinkPreparation::new(
        &vault,
        "notes/start.md",
        |_| -> Result<TargetSnapshot, String> { panic!() },
    );
    assert_eq!(scoped.link(&file, false).1.status, LinkStatus::Unsupported);
}

#[cfg(unix)]
#[test]
fn local_file_links_occupied_invalid_relative_entry_blocks_root_redirect() {
    let (dir, vault) = fixture();
    std::fs::write(dir.path().join("blocked.json"), "{}").unwrap();
    std::os::unix::fs::symlink("absent-target", dir.path().join("notes/blocked.json")).unwrap();
    std::fs::write(dir.path().join("notes/non-directory"), "x").unwrap();
    let mut prep = LinkPreparation::new(
        &vault,
        "notes/start.md",
        |_| -> Result<TargetSnapshot, String> { panic!() },
    )
    .with_local_files();
    assert_eq!(
        prep.link("blocked.json", false).1.status,
        LinkStatus::Unknown
    );
    assert_eq!(
        prep.link("non-directory/file.json", false).1.status,
        LinkStatus::Unknown
    );
    assert_eq!(
        prep.link("percent%2523.json", false).1.status,
        LinkStatus::Resolved,
        "readable positive control"
    );
}
