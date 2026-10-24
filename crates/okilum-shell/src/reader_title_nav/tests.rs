use super::*;
use ::core::prelude::v1::test;
use gpui::{size, Modifiers, TestAppContext, VisualTestContext};

#[test]
fn the_switcher_names_projects_or_the_active_project() {
    let (label, _, tip) = switcher_face(None);
    assert_eq!((label.as_ref(), tip), ("Projects", "Projects"));
    let project = okilum_t3::projects::Project {
        folder: "vault/Dev/Areas/okilum".into(),
        name: "okilum".into(),
        kind: okilum_t3::projects::Kind::Code,
        environment: None,
        manager_project: None,
        manager: okilum_t3::projects::Manager::NotStarted,
        appearance: Default::default(),
    };
    let (label, _, tip) = switcher_face(Some(&project));
    assert_eq!((label.as_ref(), tip), ("okilum", "Switch or leave project"));
}

#[test]
fn project_prefix_and_parent_location() {
    assert!(in_folder("Dev/App/plan.md", "Dev/App"));
    assert!(in_folder("Dev/App", "Dev/App"), "a pinned project folder");
    assert!(
        !in_folder("Dev/Apps/plan.md", "Dev/App"),
        "a sibling prefix"
    );
    assert!(!in_folder("Other/far.md", "Dev/App"));
    assert!(in_folder("Other/far.md", ""), "the whole vault");
    assert_eq!(
        location("Dev/App/design/rules.md", "Dev/App").as_deref(),
        Some("design")
    );
    assert_eq!(location("Dev/App/plan.md", "Dev/App"), None);
    assert_eq!(location("Dev/App/plan.md", "").as_deref(), Some("App"));
    assert_eq!(location("note.md", ""), None);
}

/// Opens `vault` on `note.md` at a desktop width, with keys bound.
fn open_reader<'a>(
    cx: &'a mut TestAppContext,
    fixture: &Path,
    vault: &Path,
) -> (Entity<Reader>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        gpui_component::init(cx);
        bind_keys(cx);
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.to_owned()),
                    note: Some("note.md".into()),
                    index_dir: Some(fixture.join("index")),
                    session_directory: Some(fixture.join("state")),
                    ..Opts::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(900.)));
    visual.run_until_parked();
    (reader.unwrap(), visual)
}

fn write_project(vault: &Path, folder: &str) {
    std::fs::create_dir_all(vault.join(folder).join("ops")).unwrap();
    std::fs::write(
        vault.join(folder).join("ops/project.yaml"),
        "kind: knowledge\n",
    )
    .unwrap();
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} rendered"));
    visual.simulate_click(bounds.center(), Modifiers::none());
    visual.run_until_parked();
}

fn paths(view: &Reader, menu: Menu) -> Vec<String> {
    view.nav_entries(menu, reader_sidebar::now())
        .into_iter()
        .map(|e| e.path)
        .collect()
}

#[gpui::test]
fn recent_and_pinned_menus_follow_the_project_scope_and_remember_it(cx: &mut TestAppContext) {
    let fixture = tempfile::tempdir().unwrap();
    let vault = fixture.path().join("vault");
    write_project(&vault, "Dev/Areas/App");
    for (path, body) in [
        ("note.md", "# Retained note\n"),
        ("Dev/Areas/App/plan.md", "# Plan\n"),
        ("Dev/Areas/App/design/rules.md", "# Rules\n"),
        ("Other/far.md", "# Far\n"),
        ("_Assets/hidden.md", "# Hidden\n"),
    ] {
        std::fs::create_dir_all(vault.join(path).parent().unwrap()).unwrap();
        std::fs::write(vault.join(path), body).unwrap();
    }
    let (reader, visual) = open_reader(cx, fixture.path(), &vault);
    let now = reader_sidebar::now();
    reader.update(visual, |view, cx| {
        view.sidebar.recent = vec![
            ("Dev/Areas/App/design/rules.md".into(), now - 60),
            ("Other/far.md".into(), now - 120),
            ("_Assets/hidden.md".into(), now - 150),
            ("Dev/Areas/App/plan.md".into(), now - 3_600),
        ];
        view.sidebar.pinned = vec![
            "Other/far.md".into(),
            "Dev/Areas/App".into(),
            "_Assets/hidden.md".into(),
        ];
        cx.notify();
    });
    visual.run_until_parked();

    // Variant A: panel toggle, switcher, Recent, Pinned, search; no
    // «name · Leave project» text button on the right.
    let order: Vec<_> = [
        "reader-notes",
        "title-switcher",
        "title-recent",
        "title-pinned",
        "sidebar-search",
    ]
    .iter()
    .map(|s| {
        visual
            .debug_bounds(s)
            .unwrap_or_else(|| panic!("{s}"))
            .left()
    })
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    assert!(visual.debug_bounds("leave-vault-project").is_none());

    // The vault: everything but hidden notes (#635), no scope switch.
    reader.read_with(visual, |view, _| {
        assert_eq!(view.nav_scope(), None);
        assert_eq!(
            paths(view, Menu::Recent),
            [
                "Dev/Areas/App/design/rules.md",
                "Other/far.md",
                "Dev/Areas/App/plan.md"
            ]
        );
        assert_eq!(paths(view, Menu::Pinned), ["Other/far.md", "Dev/Areas/App"]);
        let pinned = view.nav_entries(Menu::Pinned, now);
        assert!(pinned[1].folder && !pinned[0].folder);
        assert_eq!(
            view.nav_entries(Menu::Recent, now)[0].age.as_deref(),
            Some("1m")
        );
    });
    click(visual, "title-recent");
    assert!(visual.debug_bounds("title-nav-recent").is_some());
    assert!(visual.debug_bounds("title-nav-entry-2").is_some());
    assert!(visual.debug_bounds("title-nav-entry-3").is_none());
    assert!(visual.debug_bounds("title-nav-scope-project").is_none());
    let (trigger, menu) = (
        visual.debug_bounds("title-recent").unwrap(),
        visual.debug_bounds("title-nav-recent").unwrap(),
    );
    assert!(menu.top() >= trigger.bottom() && (menu.left() - trigger.left()).abs() < px(1.));
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(visual.debug_bounds("title-nav-recent").is_none());
    reader.read_with(visual, |view, _| assert_eq!(view.title_nav.menu, None));

    // ⌘K from an open menu replaces it, so Escape then closes the palette
    // instead of a menu drawn over it.
    click(visual, "title-recent");
    visual.simulate_keystrokes("ctrl-k");
    visual.run_until_parked();
    assert!(visual.debug_bounds("title-nav-recent").is_none());
    reader.read_with(visual, |view, _| {
        assert!(view.quick_open.open);
        assert_eq!(view.title_nav.menu, None);
    });
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    reader.read_with(visual, |view, _| assert!(!view.quick_open.open));

    // A project: its notes by default.
    let project = okilum_t3::projects::Project::read(&vault.join("Dev/Areas/App")).unwrap();
    visual.update(|window, cx| {
        reader.update(cx, |view, cx| {
            view.enter_vault_project(project.clone(), window, cx)
        })
    });
    visual.run_until_parked();
    reader.read_with(visual, |view, _| {
        assert_eq!(view.nav_scope(), Some(Scope::Project));
        assert_eq!(
            paths(view, Menu::Recent),
            ["Dev/Areas/App/design/rules.md", "Dev/Areas/App/plan.md"]
        );
        let entries = view.nav_entries(Menu::Recent, now);
        assert_eq!(entries[0].location.as_deref(), Some("design"));
        assert_eq!(entries[1].location, None);
        assert_eq!(paths(view, Menu::Pinned), ["Dev/Areas/App"]);
    });
    click(visual, "title-pinned");
    assert!(visual.debug_bounds("title-nav-pinned").is_some());
    assert!(visual.debug_bounds("title-nav-entry-0").is_some());
    assert!(visual.debug_bounds("title-nav-entry-1").is_none());
    // Another trigger switches menus in one click.
    click(visual, "title-recent");
    assert!(visual.debug_bounds("title-nav-pinned").is_none());
    assert!(visual.debug_bounds("title-nav-recent").is_some());

    // All vault: the menu stays open, the choice is saved for this project.
    click(visual, "title-nav-scope-vault");
    assert!(visual.debug_bounds("title-nav-recent").is_some());
    assert!(visual.debug_bounds("title-nav-entry-2").is_some());
    let saved = reader.read_with(visual, |view, _| {
        assert_eq!(view.nav_scope(), Some(Scope::Vault));
        view.sidebar_path.clone().unwrap()
    });
    visual.run_until_parked();
    assert!(reader_sidebar::State::load(&saved)
        .nav_all_vault
        .contains("Dev/Areas/App"));

    // Keyboard: ↓ then ↵ opens the second row and closes the menu.
    visual.simulate_keystrokes("down enter");
    visual.run_until_parked();
    reader.read_with(visual, |view, _| {
        assert_eq!(view.current_rel, "Other/far.md");
        assert_eq!(view.title_nav.menu, None);
    });

    // Leaving drops the scope; re-entering remembers All vault.
    visual.update(|window, cx| reader.update(cx, |view, cx| view.leave_vault_project(window, cx)));
    visual.run_until_parked();
    reader.read_with(visual, |view, _| assert_eq!(view.nav_scope(), None));
    visual.update(|window, cx| {
        reader.update(cx, |view, cx| view.enter_vault_project(project, window, cx))
    });
    visual.run_until_parked();
    reader.update(visual, |view, cx| {
        assert_eq!(view.nav_scope(), Some(Scope::Vault));
        view.set_nav_scope(Scope::Project, cx);
        assert_eq!(view.nav_scope(), Some(Scope::Project));
        assert!(view.sidebar.nav_all_vault.is_empty());
        // Show hidden files brings hidden notes back, as in the tree.
        view.set_nav_scope(Scope::Vault, cx);
        view.toggle_hidden_files(cx);
        assert!(paths(view, Menu::Recent).contains(&"_Assets/hidden.md".to_owned()));
    });
}

#[gpui::test]
fn recent_menu_trims_to_show_all_and_narrow_windows_keep_glyphs(cx: &mut TestAppContext) {
    let fixture = tempfile::tempdir().unwrap();
    let vault = fixture.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(vault.join("note.md"), "# Retained note\n").unwrap();
    let total = reader_sidebar::RECENT_SHOWN + 3;
    for i in 0..total {
        std::fs::write(vault.join(format!("n{i}.md")), format!("# N{i}\n")).unwrap();
    }
    let (reader, visual) = open_reader(cx, fixture.path(), &vault);
    reader.update(visual, |view, cx| {
        let now = reader_sidebar::now();
        view.sidebar.recent = (0..total)
            .map(|i| (format!("n{i}.md"), now - i as u64))
            .collect();
        cx.notify();
    });
    visual.run_until_parked();
    let wide = visual.debug_bounds("title-recent").unwrap();
    click(visual, "title-recent");
    let last = reader_sidebar::RECENT_SHOWN;
    assert!(visual
        .debug_bounds(leak(format!("title-nav-entry-{}", last - 1)))
        .is_some());
    assert!(visual
        .debug_bounds(leak(format!("title-nav-entry-{last}")))
        .is_none());
    click(visual, "title-nav-show-all");
    assert!(visual
        .debug_bounds(leak(format!("title-nav-entry-{}", total - 1)))
        .is_some());
    assert!(visual.debug_bounds("title-nav-show-all").is_none());
    // An outside press closes the menu.
    let body = visual.debug_bounds("reader-document-root").unwrap();
    visual.simulate_click(body.center(), Modifiers::none());
    visual.run_until_parked();
    assert!(visual.debug_bounds("title-nav-recent").is_none());

    visual.simulate_resize(size(px(800.), px(900.)));
    visual.run_until_parked();
    let narrow = visual
        .debug_bounds("title-recent")
        .expect("still reachable");
    assert!(
        narrow.size.width < wide.size.width && narrow.size.width <= px(28.5),
        "glyph only: {narrow:?} vs {wide:?}"
    );
    assert!(visual.debug_bounds("title-pinned").is_some());
}

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

#[gpui::test]
fn the_switcher_opens_the_shared_picker_enters_and_leaves(cx: &mut TestAppContext) {
    let fixture = tempfile::tempdir().unwrap();
    let vault = fixture.path().join("vault");
    for folder in ["Dev/Areas/App", "Music/Areas/Studio"] {
        write_project(&vault, folder);
        std::fs::write(vault.join(folder).join("note.md"), "# Project note\n").unwrap();
    }
    std::fs::write(vault.join("note.md"), "# Retained note\n").unwrap();
    let (reader, visual) = open_reader(cx, fixture.path(), &vault);
    let tree_before = reader.read_with(visual, |view, _| {
        view.tree
            .rows
            .iter()
            .map(|r| r.path.clone())
            .collect::<Vec<_>>()
    });

    // #1519: outside a project the switcher says «Projects», never the
    // vault's name (that lives in the sidebar header).
    let label = |visual: &mut VisualTestContext| {
        reader.read_with(visual, |view, _| {
            switcher_face(view.project_navigation.active.as_ref())
                .0
                .to_string()
        })
    };
    assert_eq!(label(visual), "Projects");
    click(visual, "title-switcher");
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let switcher = visual.debug_bounds("title-switcher").unwrap();
    let picker = visual
        .debug_bounds("projects-picker")
        .expect("the shared picker opens from the switcher");
    assert!(
        (picker.left() - switcher.left()).abs() < px(1.) && picker.top() >= switcher.bottom(),
        "left-aligned below the switcher: {picker:?} {switcher:?}"
    );
    assert!(
        visual.debug_bounds("projects-picker-leave").is_none(),
        "nothing to leave outside a project"
    );
    // A second press on the switcher closes it.
    click(visual, "title-switcher");
    assert!(visual.debug_bounds("projects-picker").is_none());
    reader.read_with(visual, |view, _| assert!(!view.title_nav.switcher_open));

    // Choose a project: it enters through the existing pipeline.
    click(visual, "title-switcher");
    click(visual, "project-choice-1");
    reader.read_with(visual, |view, _| {
        assert_eq!(
            view.project_navigation.active.as_ref().unwrap().folder,
            vault.join("Dev/Areas/App")
        );
        assert!(!view.title_nav.switcher_open);
        assert_eq!(view.sidebar.recent_projects, ["Dev/Areas/App"]);
        assert!(view
            .tree
            .rows
            .iter()
            .all(|r| r.path.starts_with("Dev/Areas/App/")));
    });
    assert!(visual.debug_bounds("projects-picker").is_none());
    assert_eq!(label(visual), "App", "inside a project: its name");

    // Switch straight to another project from the same menu.
    click(visual, "title-switcher");
    let switcher_picker = reader.read_with(visual, |view, _| {
        view.project_navigation.switcher.clone().unwrap()
    });
    switcher_picker.read_with(visual, |picker, _| {
        let groups = picker.model().groups();
        assert_eq!(groups[0].label, "Recent", "recents first");
        assert_eq!(groups[0].rows[0].project.name, "App");
    });
    visual.simulate_input("studio");
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    reader.read_with(visual, |view, _| {
        assert_eq!(
            view.project_navigation.active.as_ref().unwrap().folder,
            vault.join("Music/Areas/Studio")
        );
        assert_eq!(
            view.sidebar.recent_projects,
            ["Music/Areas/Studio", "Dev/Areas/App"]
        );
    });

    // Leave project sits at the foot of the same menu.
    click(visual, "title-switcher");
    click(visual, "projects-picker-leave");
    assert_eq!(label(visual), "Projects", "left the project");
    reader.read_with(visual, |view, _| {
        assert!(view.project_navigation.active.is_none());
        assert!(!view.title_nav.switcher_open);
        assert_eq!(
            view.tree
                .rows
                .iter()
                .map(|r| r.path.clone())
                .collect::<Vec<_>>(),
            tree_before
        );
    });
    assert!(visual.debug_bounds("projects-picker").is_none());
    // Esc closes the switcher too.
    click(visual, "title-switcher");
    assert!(visual.debug_bounds("projects-picker").is_some());
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    reader.read_with(visual, |view, _| assert!(!view.title_nav.switcher_open));
    // ⌘K from the open switcher replaces it with the palette.
    click(visual, "title-switcher");
    visual.simulate_keystrokes("ctrl-k");
    visual.run_until_parked();
    assert!(visual.debug_bounds("projects-picker").is_none());
    reader.read_with(visual, |view, _| {
        assert!(view.quick_open.open && !view.title_nav.switcher_open);
    });
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    reader.read_with(visual, |view, _| assert!(!view.quick_open.open));
}

/// #1517: the Recent menu's footer says «[Ctrl] [Tab] cycles recent» in key
/// caps, the word Ctrl on every system, so Ctrl+Tab with
/// the menu open moves its highlight (wrapping) and Enter opens that row.
/// Closed, ⌃Tab is still the Reader-wide recent switcher.
#[gpui::test]
fn ctrl_tab_cycles_the_open_recent_menu(cx: &mut TestAppContext) {
    let fixture = tempfile::tempdir().unwrap();
    let vault = fixture.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(vault.join("note.md"), "# Retained note\n").unwrap();
    for name in ["a", "b", "c"] {
        std::fs::write(vault.join(format!("{name}.md")), format!("# {name}\n")).unwrap();
    }
    let (reader, visual) = open_reader(cx, fixture.path(), &vault);
    reader.update(visual, |view, cx| {
        let now = reader_sidebar::now();
        view.sidebar.recent = vec![
            ("a.md".into(), now - 60),
            ("b.md".into(), now - 120),
            ("c.md".into(), now - 180),
        ];
        cx.notify();
    });
    visual.run_until_parked();
    // The footer hint names the key the open menu handles, as key caps.
    visual.update(|_, cx| {
        assert!(reader_shortcuts::shortcut(&NavMenuCycleNext, cx).is_some());
        assert_eq!(
            reader_shortcuts::shortcut_caps(&NavMenuCycleNext, cx).unwrap(),
            ["Ctrl", "Tab"]
        );
    });
    assert_eq!(CYCLE_HINT, "cycles recent");
    assert!(visual.debug_bounds("title-nav-cycle-hint").is_none());

    click(visual, "title-recent");
    let hint = visual
        .debug_bounds("title-nav-cycle-hint")
        .expect("the open Recent menu shows its key hint");
    assert!(
        hint.size.height <= px(20.),
        "small caps fit the footer: {hint:?}"
    );
    let rows = reader.read_with(visual, |view, _| {
        assert_eq!(view.title_nav.menu, Some(Menu::Recent));
        assert_eq!(view.title_nav.selected, 0);
        paths(view, Menu::Recent)
    });
    assert_eq!(rows, ["a.md", "b.md", "c.md"]);
    let selected = |visual: &mut VisualTestContext| {
        reader.read_with(visual, |view, _| {
            assert_eq!(view.title_nav.menu, Some(Menu::Recent), "menu stays open");
            assert!(!view.recent_switcher.open(), "no switcher behind the menu");
            view.title_nav.selected
        })
    };
    for expected in [1, 2, 0] {
        visual.simulate_keystrokes("ctrl-tab");
        visual.run_until_parked();
        assert_eq!(selected(visual), expected, "⌃Tab moves down and wraps");
    }
    visual.simulate_keystrokes("ctrl-shift-tab");
    visual.run_until_parked();
    assert_eq!(selected(visual), 2, "⌃⇧Tab moves up and wraps");
    assert!(visual.debug_bounds("recent-switcher").is_none());
    // Arrows still stop at the ends.
    visual.simulate_keystrokes("down");
    visual.run_until_parked();
    assert_eq!(selected(visual), 2);
    visual.simulate_keystrokes("ctrl-shift-tab");
    visual.run_until_parked();
    assert_eq!(selected(visual), 1);
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    reader.read_with(visual, |view, _| {
        assert_eq!(view.current_rel, "b.md");
        assert_eq!(view.title_nav.menu, None);
    });

    // Escape closes the menu without opening anything.
    click(visual, "title-recent");
    visual.simulate_keystrokes("ctrl-tab escape");
    visual.run_until_parked();
    reader.read_with(visual, |view, _| {
        assert_eq!(view.title_nav.menu, None);
        assert_eq!(view.current_rel, "b.md");
    });

    // Closed menu: ⌃Tab is the recent switcher, as before.
    reader.update_in(visual, |view, window, cx| {
        view.quick_open.remember("a.md");
        view.quick_open.remember("b.md");
        view.focus_handle.focus(window, cx);
        window.activate_window();
    });
    visual.run_until_parked();
    visual.simulate_keystrokes("ctrl-tab");
    visual.run_until_parked();
    assert!(visual.debug_bounds("recent-switcher").is_some());
    reader.read_with(visual, |view, _| {
        assert!(view.recent_switcher.open());
        assert_eq!(view.title_nav.menu, None);
    });
    visual.simulate_keystrokes("ctrl-escape");
    visual.run_until_parked();
    assert!(visual.debug_bounds("recent-switcher").is_none());
}
