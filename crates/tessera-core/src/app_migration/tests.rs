use super::*;
use std::collections::BTreeMap;

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Relative path → contents for every regular file under `root` (empty when
/// `root` does not exist).
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    if !root.exists() {
        return BTreeMap::new();
    }
    walkdir::WalkDir::new(root)
        .into_iter()
        .map(Result::unwrap)
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            (
                e.path().strip_prefix(root).unwrap().to_path_buf(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

const DRAFT: &[u8] = "שלום עולם\r\nПривет — draft e\u{301} 😀\r\n".as_bytes();

/// What a stable build leaves: layout, appearance, history and a reminder ledger.
fn stable_layout(state: &Path, config: &Path) {
    write(
        &state.join("update-session.json"),
        br#"{"schema":1,"channel":"stable"}"#,
    );
    write(
        &state.join("reminders/root-1.json"),
        br#"{"delivered":["a","b"]}"#,
    );
    write(&state.join("reader-diagnostic.log"), b"started\n");
    write(&config.join("reader-layout.json"), br#"{"sidebar":280}"#);
    write(&config.join("appearance.json"), br#"{"theme":"dark"}"#);
}

/// A beta build adds run history, recovery drafts and Brain outboxes.
fn beta_layout(state: &Path, config: &Path, brain: &Path) {
    stable_layout(state, config);
    write(
        &state.join("update-session.json"),
        br#"{"schema":1,"channel":"beta"}"#,
    );
    write(
        &state.join("reader-runs/2026-10-09.json"),
        br#"{"ok":true}"#,
    );
    write(&brain.join("workspace.json"), br#"{"brain_id":"x"}"#);
    write(&brain.join("editor-recovery/brain-1/draft.md"), DRAFT);
    write(&brain.join("prepared-operations/brain-1/op-1.json"), b"{}");
    write(&brain.join("discussion-outbox/item-1.json"), b"{}");
    // Transient files a running or crashed app leaves behind.
    write(&state.join(LEGACY_INSTANCE_LOCK), b"");
    write(
        &state.join("reader-instance.json"),
        br#"{"port":1,"pid":2}"#,
    );
    write(&state.join("reader-runs/1.pending"), b"");
    write(&state.join("reminders/.ledger-0f.json"), b"partial");
    write(&brain.join(".workspace-1.tmp"), b"partial");
}

fn linux_env(home: &Path) -> Environment {
    Environment {
        os: Some(Os::Linux),
        home: Some(home.to_path_buf()),
        ..Environment::default()
    }
}

fn root(roots: &[Root], kind: &str) -> Root {
    roots.iter().find(|r| r.kind == kind).unwrap().clone()
}

#[test]
fn roots_cover_every_legacy_tree_per_os_without_duplicates() {
    let home = PathBuf::from("/home/u");
    let linux = roots(&linux_env(&home));
    assert_eq!(
        linux
            .iter()
            .map(|r| (r.kind, r.old.clone(), r.new.clone()))
            .collect::<Vec<_>>(),
        [
            (
                "state",
                home.join(".local/state/tessera"),
                home.join(".local/state/okilum")
            ),
            (
                "config",
                home.join(".config/tessera"),
                home.join(".config/okilum")
            ),
            (
                "data",
                home.join(".local/share/tessera"),
                home.join(".local/share/okilum")
            ),
        ]
    );
    let xdg = roots(&Environment {
        xdg_state_home: Some("/xdg/state".into()),
        xdg_config_home: Some("/xdg/config".into()),
        xdg_data_home: Some("/xdg/data".into()),
        ..linux_env(&home)
    });
    assert_eq!(root(&xdg, "state").old, PathBuf::from("/xdg/state/tessera"));
    assert_eq!(
        root(&xdg, "config").new,
        PathBuf::from("/xdg/config/okilum")
    );
    assert_eq!(root(&xdg, "data").old, PathBuf::from("/xdg/data/tessera"));

    let mac_home = PathBuf::from("/Users/u");
    let mac = roots(&Environment {
        os: Some(Os::MacOs),
        home: Some(mac_home.clone()),
        ..Environment::default()
    });
    let support = mac_home.join("Library/Application Support");
    assert_eq!(
        mac.iter()
            .map(|r| (r.kind, r.old.clone(), r.new.clone()))
            .collect::<Vec<_>>(),
        [
            (
                "state",
                support.join("uk.oklabs.tessera"),
                support.join("com.befeast.okilum")
            ),
            ("config", support.join("tessera"), support.join("okilum")),
            (
                "brain",
                mac_home.join(".config/tessera"),
                mac_home.join(".config/okilum")
            ),
        ]
    );
    // With XDG_CONFIG_HOME set, Reader config and Brain share one tree.
    let mac_xdg = roots(&Environment {
        os: Some(Os::MacOs),
        home: Some(mac_home.clone()),
        xdg_config_home: Some("/xdg/config".into()),
        ..Environment::default()
    });
    assert_eq!(mac_xdg.len(), 2);
    assert_eq!(
        root(&mac_xdg, "config").old,
        PathBuf::from("/xdg/config/tessera")
    );

    let windows = roots(&Environment {
        os: Some(Os::Windows),
        local_app_data: Some("C:/Users/u/AppData/Local".into()),
        roaming_app_data: Some("C:/Users/u/AppData/Roaming".into()),
        ..Environment::default()
    });
    assert_eq!(
        windows
            .iter()
            .map(|r| (r.kind, r.new.clone()))
            .collect::<Vec<_>>(),
        [
            ("state", PathBuf::from("C:/Users/u/AppData/Local/okilum")),
            ("config", PathBuf::from("C:/Users/u/AppData/Roaming/okilum")),
        ]
    );
    // An explicit isolated state dir is the caller's choice: never migrated.
    let isolated = roots(&Environment {
        explicit_state_dir: Some("/tmp/isolated".into()),
        ..linux_env(&home)
    });
    assert!(isolated.iter().all(|r| r.kind != "state"));
}

fn run(roots: &[Root], lock: &Path) -> Report {
    import(roots, lock, &Options::default()).unwrap()
}

#[test]
fn stable_and_beta_layouts_copy_exactly_and_leave_the_legacy_tree_untouched() {
    for beta in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let roots = roots(&linux_env(home.path()));
        let (state, config, data) = (
            root(&roots, "state"),
            root(&roots, "config"),
            root(&roots, "data"),
        );
        if beta {
            beta_layout(&state.old, &config.old, &config.old);
        } else {
            stable_layout(&state.old, &config.old);
        }
        let before: Vec<_> = roots.iter().map(|r| snapshot(&r.old)).collect();
        let lock = home.path().join(".local/state/okilum-import.lock");
        let report = run(&roots, &lock);

        let after: Vec<_> = roots.iter().map(|r| snapshot(&r.old)).collect();
        assert_eq!(before, after, "legacy trees are read-only (beta={beta})");
        assert!(matches!(report.roots[2].1, Outcome::NoLegacyData));
        assert!(!data.new.exists());
        for r in [&state, &config] {
            let Outcome::Copied { files, .. } = report
                .roots
                .iter()
                .find(|(root, _)| root.kind == r.kind)
                .unwrap()
                .1
            else {
                panic!("fresh root must be copied: {:?}", report.roots);
            };
            let mut new = snapshot(&r.new);
            assert!(new.remove(Path::new(MARKER)).is_some());
            let mut expected = snapshot(&r.old);
            expected.retain(|path, _| !transient(&path.file_name().unwrap().to_string_lossy()));
            assert_eq!(new, expected, "{} (beta={beta})", r.kind);
            assert_eq!(files, expected.len());
        }
        if beta {
            assert_eq!(
                fs::read(config.new.join("editor-recovery/brain-1/draft.md")).unwrap(),
                DRAFT
            );
            assert!(!state.new.join(LEGACY_INSTANCE_LOCK).exists());
            assert!(!state.new.join("reminders/.ledger-0f.json").exists());
            assert_eq!(
                fs::read_to_string(state.new.join("update-session.json")).unwrap(),
                r#"{"schema":1,"channel":"beta"}"#
            );
        }
    }
}

#[test]
fn a_second_run_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let roots = roots(&linux_env(home.path()));
    let (state, config) = (root(&roots, "state"), root(&roots, "config"));
    beta_layout(&state.old, &config.old, &config.old);
    let lock = home.path().join("import.lock");
    run(&roots, &lock);
    // A setting changed in Okilum after the import must survive a rerun.
    write(&config.new.join("appearance.json"), br#"{"theme":"light"}"#);
    let accepted: Vec<_> = roots.iter().map(|r| snapshot(&r.new)).collect();
    let report = run(&roots, &lock);
    assert!(report
        .roots
        .iter()
        .all(|(_, o)| matches!(o, Outcome::AlreadyImported | Outcome::NoLegacyData)));
    assert_eq!(
        accepted,
        roots.iter().map(|r| snapshot(&r.new)).collect::<Vec<_>>()
    );
}

#[test]
fn an_interrupted_fresh_copy_leaves_no_partial_root_and_resumes() {
    let home = tempfile::tempdir().unwrap();
    let roots = roots(&linux_env(home.path()));
    let (state, config) = (root(&roots, "state"), root(&roots, "config"));
    beta_layout(&state.old, &config.old, &config.old);
    let lock = home.path().join("import.lock");
    let error = import(
        &roots,
        &lock,
        &Options {
            fail_after: Some(2),
        },
    )
    .unwrap_err();
    assert!(matches!(error, ImportError::Io { .. }), "{error}");
    // The interrupted root is absent, its staging is what is left behind.
    assert!(!state.new.exists());
    let staging: Vec<_> = fs::read_dir(state.new.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(STAGING_PREFIX))
        .collect();
    assert_eq!(staging.len(), 1);

    run(&roots, &lock);
    assert!(state.new.join(MARKER).is_file());
    let leftovers = fs::read_dir(state.new.parent().unwrap())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(STAGING_PREFIX)
        })
        .count();
    assert_eq!(leftovers, 0, "stale staging is discarded on resume");
    let mut new = snapshot(&state.new);
    new.remove(Path::new(MARKER));
    let mut expected = snapshot(&state.old);
    expected.retain(|p, _| !transient(&p.file_name().unwrap().to_string_lossy()));
    assert_eq!(new, expected);
}

#[test]
fn populated_old_and_new_roots_merge_without_overwriting() {
    let home = tempfile::tempdir().unwrap();
    let roots = roots(&linux_env(home.path()));
    let config = root(&roots, "config");
    write(&config.old.join("appearance.json"), br#"{"theme":"dark"}"#);
    write(
        &config.old.join("reader-layout.json"),
        br#"{"sidebar":280}"#,
    );
    write(&config.old.join("editor-recovery/b/draft.md"), DRAFT);
    // A read-only legacy file still copies (and keeps its permissions).
    let readonly = config.old.join("editor-recovery/b/sealed.json");
    write(&readonly, b"{}");
    let mut permissions = fs::metadata(&readonly).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&readonly, permissions).unwrap();
    // Okilum already wrote its own settings before the import ran.
    write(&config.new.join("appearance.json"), br#"{"theme":"light"}"#);
    write(
        &config.new.join("reader-layout.json"),
        br#"{"sidebar":280}"#,
    );
    let lock = home.path().join("import.lock");

    // A hard kill mid-copy in an earlier merge left our temporary behind.
    write(
        &config.new.join("editor-recovery/.okilum-copy-dead.tmp"),
        b"half",
    );
    // Interrupt the merge once, then let it finish: nothing doubles.
    assert!(import(
        &roots,
        &lock,
        &Options {
            fail_after: Some(1)
        }
    )
    .is_err());
    assert!(!config.new.join(MARKER).exists());
    let report = run(&roots, &lock);
    let outcome = &report
        .roots
        .iter()
        .find(|(r, _)| r.kind == "config")
        .unwrap()
        .1;
    assert_eq!(
        outcome,
        &Outcome::Merged {
            copied: 2,
            conflicts: vec![PathBuf::from("appearance.json")]
        },
        "files go in name order: the interrupted run kept the conflict copy, the resume adds the draft and the read-only file"
    );
    assert_eq!(
        fs::read_to_string(config.new.join("appearance.json")).unwrap(),
        r#"{"theme":"light"}"#,
        "the new setting wins"
    );
    assert_eq!(
        fs::read_to_string(config.new.join(CONFLICTS_DIR).join("appearance.json")).unwrap(),
        r#"{"theme":"dark"}"#,
        "the old one is kept, not lost"
    );
    assert_eq!(
        fs::read(config.new.join("editor-recovery/b/draft.md")).unwrap(),
        DRAFT
    );
    assert!(
        !config
            .new
            .join("editor-recovery/.okilum-copy-dead.tmp")
            .exists(),
        "stale temporaries from a killed merge are removed"
    );
    let sealed = config.new.join("editor-recovery/b/sealed.json");
    assert_eq!(fs::read(&sealed).unwrap(), b"{}");
    assert!(fs::metadata(&sealed).unwrap().permissions().readonly());
    assert!(matches!(
        run(&roots, &lock)
            .roots
            .iter()
            .find(|(r, _)| r.kind == "config")
            .unwrap()
            .1,
        Outcome::AlreadyImported
    ));
}

#[test]
fn a_running_tessera_blocks_the_import_and_its_lock_is_never_created() {
    let home = tempfile::tempdir().unwrap();
    let roots = roots(&linux_env(home.path()));
    let (state, config) = (root(&roots, "state"), root(&roots, "config"));
    stable_layout(&state.old, &config.old);
    let lock = home.path().join("import.lock");
    let held = File::create(state.old.join(LEGACY_INSTANCE_LOCK)).unwrap();
    held.try_lock().unwrap();
    assert!(matches!(
        import(&roots, &lock, &Options::default()),
        Err(ImportError::LegacyRunning(_))
    ));
    assert!(!state.new.exists() && !config.new.exists());
    drop(held);
    // A live run's `.active` marker is the second signal.
    let active = state.old.join("reader-runs/live.active");
    write(&active, b"");
    let run_marker = File::open(&active).unwrap();
    run_marker.try_lock().unwrap();
    assert!(legacy_instance_running(&state.old));
    drop(run_marker);
    fs::remove_file(&active).unwrap();
    fs::remove_file(state.old.join(LEGACY_INSTANCE_LOCK)).unwrap();
    // Positive control above; with no legacy lock file the probe creates none.
    assert!(!legacy_instance_running(&state.old));
    assert!(!state.old.join(LEGACY_INSTANCE_LOCK).exists());
    run(&roots, &lock);
    assert!(state.new.join(MARKER).is_file());
}

#[test]
fn a_second_concurrent_import_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let roots = roots(&linux_env(home.path()));
    stable_layout(&root(&roots, "state").old, &root(&roots, "config").old);
    let lock = home.path().join("import.lock");
    let held = File::create(&lock).unwrap();
    held.try_lock().unwrap();
    assert!(matches!(
        import(&roots, &lock, &Options::default()),
        Err(ImportError::Busy(_))
    ));
}

#[cfg(unix)]
#[test]
fn symlinks_and_sockets_are_skipped_and_recorded() {
    let home = tempfile::tempdir().unwrap();
    let roots = roots(&linux_env(home.path()));
    let state = root(&roots, "state");
    write(&state.old.join("update-session.json"), b"{}");
    std::os::unix::fs::symlink("/etc/hostname", state.old.join("link")).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(state.old.join("s-1.sock")).unwrap();
    // A crash marker from a run that ended is evidence and is imported.
    write(&state.old.join("reader-runs/7.active"), b"");
    run(&roots, &home.path().join("import.lock"));
    assert!(state.new.join("update-session.json").is_file());
    assert!(!state.new.join("link").exists() && !state.new.join("s-1.sock").exists());
    assert!(state.new.join("reader-runs/7.active").is_file());
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(state.new.join(MARKER)).unwrap()).unwrap();
    assert_eq!(
        manifest.skipped,
        [PathBuf::from("link"), PathBuf::from("s-1.sock")]
    );
}

#[test]
fn the_new_environment_variable_wins_over_the_legacy_one() {
    // Unique names: tests run in parallel within one process.
    let (new, legacy) = ("OKILUM_TEST_967_DIR", "TESSERA_TEST_967_DIR");
    std::env::remove_var(new);
    std::env::set_var(legacy, "/legacy");
    assert_eq!(env_var(new, legacy).unwrap(), "/legacy");
    std::env::set_var(new, "/new");
    assert_eq!(env_var(new, legacy).unwrap(), "/new");
    std::env::remove_var(new);
    std::env::remove_var(legacy);
    assert_eq!(env_var(new, legacy), None);
}
