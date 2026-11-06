use super::*;
use ::core::prelude::v1::test;
use gpui::TestAppContext;

gpui::actions!(keymap_fixture, [First, Second]);

fn setup(cx: &mut App, path: &Path) {
    crate::test_init(cx);
    cx.bind_keys([
        KeyBinding::new("f8", First, None),
        KeyBinding::new("f9", First, Some("ReaderRead && !Input && !ReaderTree")),
        KeyBinding::new("f10", First, Some("ReaderSource > Input")),
    ]);
    install_at(Some(path.to_owned()), cx);
}

fn override_for(scope: &str, key: &str, action: Option<&str>) -> Override {
    Override {
        context: scope.into(),
        key: key.into(),
        action: action.map(String::from),
        args: None,
    }
}

fn bytes(entries: Vec<Override>) -> Vec<u8> {
    serde_json::to_vec(&Document {
        version: 1,
        bindings: entries,
    })
    .unwrap()
}

fn actions(key: &str, context: &[&str], cx: &App) -> Vec<String> {
    let key = gpui::Keystroke::parse(key).unwrap();
    let contexts: Vec<_> = context
        .iter()
        .map(|v| gpui::KeyContext::parse(v).unwrap())
        .collect();
    cx.key_bindings()
        .borrow()
        .bindings_for_input(&[key], &contexts)
        .0
        .iter()
        .map(|binding| binding.action().name().into())
        .collect()
}

#[gpui::test]
fn keymap_2626_overrides_null_aliases_and_multiple_bindings_preserve_other_contexts(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keymap.json");
    cx.update(|cx| {
        setup(cx, &path);
        adopt(
            Ok(Some(bytes(vec![
                override_for("ReadMode", "f9", Some(Second::name_for_type())),
                override_for("ReadMode", "f11", Some(Second::name_for_type())),
                override_for("Editor", "f10", None),
            ]))),
            cx,
        );
        assert_eq!(
            actions("f9", &["ReaderRead"], cx)[0],
            Second::name_for_type()
        );
        assert_eq!(
            actions("f11", &["ReaderRead"], cx)[0],
            Second::name_for_type()
        );
        assert!(actions("f10", &["ReaderSource", "Input"], cx).is_empty());
        assert_eq!(
            actions("f8", &["ReaderRead"], cx)[0],
            First::name_for_type()
        );
        assert!(actions("f9", &["ReaderSource", "Input"], cx).is_empty());
        adopt(Ok(None), cx);
        assert_eq!(
            actions("f9", &["ReaderRead"], cx)[0],
            First::name_for_type()
        );
        assert_eq!(
            actions("f10", &["ReaderSource", "Input"], cx)[0],
            First::name_for_type()
        );
    });
}

#[gpui::test]
fn keymap_2626_invalid_reload_retains_last_good_bindings_and_visible_error(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keymap.json");
    cx.update(|cx| {
        setup(cx, &path);
        let good = bytes(vec![override_for(
            "Global",
            "f8",
            Some(Second::name_for_type()),
        )]);
        adopt(Ok(Some(good)), cx);
        for bad in [
            b"{".to_vec(),
            bytes(vec![override_for("Global", "f8", Some("unknown::Action"))]),
            bytes(vec![override_for(
                "Global",
                "",
                Some(First::name_for_type()),
            )]),
            bytes(vec![override_for(
                "Reader &&",
                "f8",
                Some(First::name_for_type()),
            )]),
            bytes(vec![
                override_for("Global", "f8", None),
                override_for("Global", "f8", Some(First::name_for_type())),
            ]),
        ] {
            std::fs::write(&path, &bad).unwrap();
            adopt(read(&path), cx);
            assert!(cx
                .global::<Keymap>()
                .error
                .as_ref()
                .unwrap()
                .contains("last working"));
            assert_eq!(
                actions("f8", &["ReaderRead"], cx)[0],
                Second::name_for_type()
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bad,
                "invalid authored configuration stays intact"
            );
        }
        std::fs::remove_file(&path).unwrap();
        adopt(read(&path), cx);
        assert!(cx.global::<Keymap>().error.is_none());
        assert_eq!(
            actions("f8", &["ReaderRead"], cx)[0],
            First::name_for_type()
        );
    });
}

#[gpui::test]
fn keymap_2626_external_file_changes_reload_without_restart(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keymap.json");
    cx.update(|cx| {
        setup(cx, &path);
        poll_file(path.clone(), cx);
    });
    let (_, visual) = cx.add_window_view(|_, _| gpui::Empty);
    visual.run_until_parked();
    std::fs::write(
        &path,
        bytes(vec![override_for(
            "Global",
            "f8",
            Some(Second::name_for_type()),
        )]),
    )
    .unwrap();
    visual.executor().advance_clock(Duration::from_secs(1));
    visual.run_until_parked();
    visual.update(|_, cx| {
        assert_eq!(
            actions("f8", &["ReaderRead"], cx)[0],
            Second::name_for_type()
        )
    });
    std::fs::write(&path, b"{").unwrap();
    visual.executor().advance_clock(Duration::from_secs(1));
    visual.run_until_parked();
    visual.update(|_, cx| {
        assert!(cx.global::<Keymap>().error.is_some());
        assert_eq!(
            actions("f8", &["ReaderRead"], cx)[0],
            Second::name_for_type()
        );
    });
    std::fs::remove_file(&path).unwrap();
    visual.executor().advance_clock(Duration::from_secs(1));
    visual.run_until_parked();
    visual.update(|_, cx| {
        assert_eq!(
            actions("f8", &["ReaderRead"], cx)[0],
            First::name_for_type()
        )
    });
}

#[gpui::test]
fn keymap_2626_context_menu_hints_and_parameterized_input_actions_follow_overrides(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keymap.json");
    cx.update(|cx| {
        setup(cx, &path);
        cx.bind_keys([
            KeyBinding::new(
                "f6",
                reader_read_menu::CopyAsMarkdown,
                Some("ReaderRead && !Input && !ReaderTree"),
            ),
            KeyBinding::new(
                "f7",
                reader_read_menu::CopyAsMarkdown,
                Some("ReaderSource > Input"),
            ),
        ]);
        install_at(Some(path), cx);
        let enter = Override {
            context: "Input".into(),
            key: "f12".into(),
            action: Some(gpui_component::input::Enter::name_for_type().into()),
            args: Some(serde_json::json!({"secondary": false, "shift": true})),
        };
        adopt(
            Ok(Some(bytes(vec![
                override_for("Editor", "f7", None),
                override_for(
                    "Editor",
                    "f8",
                    Some(reader_read_menu::CopyAsMarkdown::name_for_type()),
                ),
                enter,
            ]))),
            cx,
        );
        assert_eq!(
            reader_shortcuts::content_shortcuts(&reader_read_menu::CopyAsMarkdown, false, cx),
            key_label("f6")
        );
        assert_eq!(
            reader_shortcuts::content_shortcuts(&reader_read_menu::CopyAsMarkdown, true, cx),
            key_label("f8")
        );
        let map = cx.key_bindings();
        let map = map.borrow();
        let key = gpui::Keystroke::parse("f12").unwrap();
        let (bindings, _) =
            map.bindings_for_input(&[key], &[gpui::KeyContext::parse("Input").unwrap()]);
        let enter = bindings[0]
            .action()
            .as_any()
            .downcast_ref::<gpui_component::input::Enter>()
            .unwrap();
        assert!(
            enter.shift && !enter.secondary,
            "parameterized input semantics survive remapping"
        );
    });
}

#[test]
fn keymap_2626_atomic_save_refuses_external_edits_and_round_trips_null() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keymap.json");
    let document = Document {
        version: 1,
        bindings: vec![override_for("Editor", "f8", None)],
    };
    let initial = save(&path, &None, &document).unwrap();
    assert_eq!(
        serde_json::from_slice::<Document>(&initial).unwrap(),
        document
    );
    let external = b"{\"version\":1,\"bindings\":[],\"unfinished\":true}";
    std::fs::write(&path, external).unwrap();
    assert!(save(&path, &Some(initial), &Document::default()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), external);
}

#[test]
fn keymap_2626_concurrent_settings_saves_preserve_the_first_accepted_revision() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keymap.json");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let jobs: Vec<_> = ["f6", "f7"]
        .into_iter()
        .map(|key| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let document = Document {
                    version: 1,
                    bindings: vec![override_for("Global", key, None)],
                };
                barrier.wait();
                save(&path, &None, &document).ok()
            })
        })
        .collect();
    let accepted: Vec<_> = jobs
        .into_iter()
        .filter_map(|job| job.join().unwrap())
        .collect();
    assert_eq!(
        accepted.len(),
        1,
        "only one Settings window can replace the absent revision"
    );
    assert_eq!(std::fs::read(path).unwrap(), accepted[0]);
}

#[gpui::test]
fn keymap_2626_conflicts_distinguish_same_disjoint_and_global_contexts(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keymap.json");
    cx.update(|cx| {
        setup(cx, &path);
        let row = Row {
            action: Second::name_for_type().into(),
            args: None,
            label: "Second".into(),
            context: context("ReadMode").unwrap().unwrap(),
            keys: Vec::new(),
            defaults: Vec::new(),
            changed: false,
        };
        assert!(!conflicts(&row, "f9", cx).0.is_empty());
        assert_eq!(conflicts(&row, "f10", cx), (Vec::new(), Vec::new()));
        let (same, warnings) = conflicts(&row, "f8", cx);
        assert!(same.is_empty());
        assert!(warnings.iter().any(|v| v.contains("shadowed")));
    });
}

#[gpui::test]
fn keymap_2626_record_cancel_reassign_dispatch_reset_and_search_use_the_live_map(
    cx: &mut TestAppContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keymap.json");
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    cx.update(|cx| {
        setup(cx, &path);
        let first = events.clone();
        cx.on_action(move |_: &First, _| first.lock().unwrap().push("first"));
        let second = events.clone();
        cx.on_action(move |_: &Second, _| second.lock().unwrap().push("second"));
    });
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| KeyboardSettings::new(None, window, cx));
        panel = Some(view.clone());
        Root::new(view, window, cx)
    });
    let panel = panel.unwrap();
    visual.simulate_resize(size(px(1000.), px(760.)));
    panel.update_in(visual, |panel, window, cx| {
        panel.search.update(cx, |input, cx| {
            input.set_value(Second::name_for_type(), window, cx)
        })
    });
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let record = visual
        .debug_bounds("keymap-record-0")
        .expect("search finds the actual unbound action");
    visual.simulate_click(record.center(), Modifiers::default());
    visual.run_until_parked();
    visual.simulate_keystrokes("f8");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("keymap-conflict").is_some());
    assert!(
        events.lock().unwrap().is_empty(),
        "recording cannot dispatch the existing shortcut"
    );
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(!path.exists(), "Cancel never saves a recorded assignment");
    visual.simulate_click(record.center(), Modifiers::default());
    visual.run_until_parked();
    visual.simulate_keystrokes("f8");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let confirm = visual.debug_bounds("keymap-confirm").unwrap();
    visual.simulate_click(confirm.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(path.exists());
    visual.simulate_keystrokes("f8");
    visual.run_until_parked();
    assert_eq!(*events.lock().unwrap(), ["second"]);
    panel.update_in(visual, |panel, window, cx| {
        let row = cx
            .global::<Keymap>()
            .rows
            .iter()
            .find(|r| r.action == Second::name_for_type() && r.context == "Global")
            .unwrap()
            .clone();
        assert!(row.changed && panel::matches_query(&row, "f8"));
        panel.reset_row(row, window, cx);
    });
    visual.run_until_parked();
    visual.simulate_keystrokes("f8");
    visual.run_until_parked();
    assert_eq!(*events.lock().unwrap(), ["second", "first"]);
}
