use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn shortcuts_preserve_input_conflicts_and_are_shown_in_keyboard_settings(cx: &mut TestAppContext) {
    let keymap = cx.update(|cx| {
        crate::test_init(cx);
        crate::bind_keys(cx);
        gpui::Keymap::new(cx.key_bindings().borrow().bindings().cloned().collect())
    });
    let matches = |key: &str, contexts: &[&str]| {
        let keys = KeyBinding::new(key, OpenTerminal, None);
        let contexts: Vec<gpui::KeyContext> = contexts
            .iter()
            .map(|c| gpui::KeyContext::parse(c).unwrap())
            .collect();
        keymap
            .bindings_for_input(keys.keystrokes(), &contexts)
            .0
            .into_iter()
            .map(|b| b.action().name())
            .collect::<Vec<_>>()
    };
    for key in ["secondary-j", "ctrl-`"] {
        assert_eq!(
            matches(key, &["Reader"]),
            [OpenTerminal::name_for_type()],
            "no other global Reader/toolkit binding may be stolen"
        );
        for contexts in [
            vec!["Reader", "Input"],
            vec!["Reader", "Textarea"],
            vec!["Reader", "ReaderSource", "Input"],
        ] {
            assert!(
                !matches(key, &contexts).contains(&OpenTerminal::name_for_type()),
                "{key} must retain its input meaning"
            );
        }
    }
    assert!(
        matches("ctrl-`", &["Reader", "ReaderSource", "Input"])
            .contains(&reader_editor::format::ToggleInlineCode::name_for_type()),
        "positive control: the editor conflict remains bound"
    );
    let rows = reader_shortcuts::rows(&keymap, platform::labels::Os::CURRENT);
    let row = rows
        .iter()
        .find(|row| row.label == "Open terminal")
        .unwrap();
    assert_eq!(row.keys.len(), 2);
}

#[gpui::test]
fn keys_menu_and_palette_reach_the_project_launcher_fake(cx: &mut TestAppContext) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    std::fs::create_dir_all(root.join("ops")).unwrap();
    std::fs::write(root.join("ops/project.yaml"), "kind: knowledge\n").unwrap();
    std::fs::write(root.join("One.md"), "# One\n").unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        crate::bind_keys(cx);
        cx.set_global(Launches::default());
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("One.md".into()),
                    index_dir: Some(fixture.path().join("index")),
                    session_directory: Some(fixture.path().join("state")),
                    panel_settings_override: Some(fixture.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let reader = reader.unwrap();
    visual.run_until_parked();
    reader.update_in(visual, |this, window, cx| {
        this.project_navigation.active = Some(okilum_t3::projects::Project::read(&root).unwrap());
        this.focus_handle.focus(window, cx);
    });
    for (ix, key) in ["secondary-j", "ctrl-`"].iter().enumerate() {
        visual.simulate_keystrokes(key);
        visual.run_until_parked();
        visual.update(|_, cx| assert_eq!(cx.global::<Launches>().0.len(), ix + 1));
    }
    // Manager menu's path-based entry uses the same project launcher.
    visual.update(|window, cx| open_project(root.clone(), window, cx));
    visual.update(|_, cx| assert_eq!(cx.global::<Launches>().0.len(), 3));
    reader.update_in(visual, |this, window, cx| {
        this.open_quick_open(false, window, cx);
        this.quick_open
            .input
            .update(cx, |input, cx| input.set_value("terminal", window, cx));
        this.refresh_quick_open(cx);
    });
    visual.run_until_parked();
    reader.read_with(visual, |this, cx| {
        let items = this.palette_items();
        assert!(items
            .iter()
            .any(|(_, item)| matches!(item, quick_open::PaletteItem::OpenTerminal)));
        assert!(items
            .iter()
            .any(|(_, item)| matches!(item, quick_open::PaletteItem::TerminalSettings)));
        assert!(!reader_shortcuts::all_shortcuts(&OpenTerminal, cx).is_empty());
    });
    // The palette's focused Input retains both keys. Entering its terminal
    // row explicitly still invokes the launch handler.
    visual.simulate_keystrokes("secondary-j ctrl-`");
    visual.run_until_parked();
    visual.update(|_, cx| assert_eq!(cx.global::<Launches>().0.len(), 3));
    reader.update_in(visual, |this, window, cx| {
        let ix = this
            .palette_items()
            .iter()
            .position(|(_, item)| matches!(item, quick_open::PaletteItem::OpenTerminal))
            .unwrap();
        this.open_palette_item(ix, window, cx);
    });
    visual.update(|_, cx| assert_eq!(cx.global::<Launches>().0, vec![root.clone(); 4]));
}

#[test]
fn project_settings_connections_and_per_device_overrides_are_resolved_without_launching() {
    let fixture = tempfile::tempdir().unwrap();
    let area = fixture.path().join("area");
    let state = fixture.path().join("state");
    std::fs::create_dir_all(area.join("ops")).unwrap();
    std::fs::create_dir(&state).unwrap();
    let manifest = area.join("ops/project.yaml");
    std::fs::write(
        &manifest,
        "kind: knowledge\nt3: {environment: remote}\nexecution: {source_checkout: /checkout}\n",
    )
    .unwrap();
    let project = okilum_t3::projects::Project::read(&area).unwrap();
    assert!(initial_for_project(&project, &state).is_err());
    std::fs::write(
        state.join("t3-connections.json"),
        r#"{"remote":"https://maestro:3773"}"#,
    )
    .unwrap();
    assert_eq!(
        initial_for_project(&project, &state).unwrap().argv,
        ["ssh", "--", "maestro"]
    );
    std::fs::write(&manifest, "kind: knowledge\nt3: {environment: remote, origin: 'http://[::1]:3773'}\nexecution: {source_checkout: /checkout}\n").unwrap();
    assert_eq!(
        initial_for_project(&project, &state)
            .unwrap()
            .directory
            .as_deref(),
        Some("/checkout")
    );
    std::fs::write(&manifest, "kind: knowledge\nt3: {environment: remote}\nexecution: {source_checkout: /checkout, terminal_host: ssh-alias}\n").unwrap();
    std::fs::write(state.join("t3-connections.json"), "broken registry").unwrap();
    assert_eq!(
        initial_for_project(&project, &state).unwrap().argv,
        ["ssh", "--", "ssh-alias"]
    );
    std::fs::write(
        &manifest,
        "kind: knowledge\nexecution: {terminal_command: 'ssh manifest-host'}\n",
    )
    .unwrap();
    assert_eq!(
        initial_for_project(&project, &state).unwrap().argv,
        ["ssh", "manifest-host"]
    );
    // A local project does not inherit an unrelated app connection, even if
    // the cached Project still names its old remote alias.
    std::fs::write(
        &manifest,
        "kind: knowledge\nexecution: {source_checkout: /new-checkout}\n",
    )
    .unwrap();
    assert_eq!(
        initial_for_project(&project, &state)
            .unwrap()
            .directory
            .as_deref(),
        Some("/new-checkout")
    );
    std::fs::write(
        &manifest,
        "kind: knowledge\nexecution: {terminal_command: 'ssh manifest-host'}\n",
    )
    .unwrap();
    let setting = setting_file(&area, &state);
    std::fs::create_dir_all(setting.parent().unwrap()).unwrap();
    std::fs::write(&setting, serde_json::to_vec("ssh device-host").unwrap()).unwrap();
    assert_eq!(
        initial_for_project(&project, &state).unwrap().argv,
        ["ssh", "device-host"]
    );
    std::fs::remove_file(&setting).unwrap();
    assert_eq!(
        initial_for_project(&project, &state).unwrap().argv,
        ["ssh", "manifest-host"]
    );
    std::fs::write(
        &manifest,
        "kind: knowledge\nexecution: {terminal_host: 'maestro;evil'}\n",
    )
    .unwrap();
    assert!(initial_for_project(&project, &state).is_err());
    std::fs::write(
        &manifest,
        "kind: knowledge\nexecution: {terminal_host: 42}\n",
    )
    .unwrap();
    assert!(initial_for_project(&project, &state).is_err());
}

/// #2837 review: the launcher never starts inside the selected folder (Windows
/// would resolve `powershell.exe` there first); the folder is an argument and
/// launchers come from trusted absolute locations only.
#[cfg(target_os = "linux")]
#[test]
fn folder_launch_runs_outside_the_folder_with_trusted_absolute_programs() {
    let root = tempfile::tempdir().unwrap();
    let vault = root.path().join("vault");
    let folder = vault.join("Notes");
    std::fs::create_dir_all(&folder).unwrap();
    for decoy in [
        "xdg-terminal-exec",
        "foot",
        "wt.exe",
        "powershell.exe",
        "open",
    ] {
        std::fs::write(folder.join(decoy), b"decoy").unwrap();
    }
    // Every name resolves to the decoy unless it is excluded.
    let decoys = |excluded: Vec<PathBuf>| {
        let folder = folder.clone();
        move |name: &str| {
            let candidate = folder.join(name);
            (!excluded.iter().any(|e| candidate.starts_with(e))).then_some(candidate)
        }
    };
    let plan = folder_launch(&folder, decoys(vec![])).unwrap();
    assert!(
        plan.program.starts_with(&folder),
        "positive control: an unfiltered lookup would pick the decoy"
    );
    assert!(folder_launch(&folder, decoys(excluded(&folder, &vault))).is_err());
    let plan = folder_launch(&folder, |name| {
        Some(Path::new("/trusted").join(name.trim_start_matches('/')))
    })
    .unwrap();
    assert!(plan.program.starts_with("/trusted"));
    let process = process(&plan, |name| {
        (name == "APPDIR").then(|| "/tmp/.mount_X".to_owned())
    });
    assert_eq!(process.get_current_dir(), None, "no cwd inside the vault");
    assert!(process
        .get_args()
        .any(|arg| Path::new(arg).ends_with("Notes") || arg.to_string_lossy().contains("Notes")));
    let removed: Vec<_> = process
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key.to_owned())
        .collect();
    assert!(removed.contains(&"APPDIR".into()) && removed.contains(&"APPIMAGE".into()));
    // excluded() covers the vault and the folder, raw and canonical.
    let list = excluded(&folder, &vault);
    assert!(list.contains(&vault.canonicalize().unwrap()));
    assert!(list.contains(&folder.canonicalize().unwrap()));
}

#[test]
fn launcher_processes_never_inherit_a_vault_working_directory() {
    let plan = command::Plan {
        program: std::env::temp_dir().join("terminal"),
        args: vec!["-d".into(), std::env::temp_dir().join("Notes").into()],
        new_console: true,
    };
    let process = process(&plan, |_| None);
    assert_eq!(process.get_current_dir(), None);
    assert_eq!(process.get_program(), plan.program.as_os_str());
    assert_eq!(
        process.get_envs().count(),
        0,
        "outside an AppImage nothing changes"
    );
}
