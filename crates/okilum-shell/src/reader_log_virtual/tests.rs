use super::*;
use ::core::prelude::v1::test;
use gpui::TestAppContext;
use std::time::Instant;

fn source_a(view: &LogView) -> &dyn LogData {
    (0..view.source_count())
        .filter_map(|source| view.source(source))
        .find(|source| source.path().ends_with("a.log"))
        .unwrap()
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(vault.join("logs")).unwrap();
    std::fs::write(vault.join("logs/a.log"), "first\n").unwrap();
    std::fs::write(vault.join("logs/b.log"), "second\n").unwrap();
    std::fs::write(
        vault.join("view.okilog"),
        r#"{"version":1,"name":"Combined","vault_root":true,"glob":"logs/*.log"}"#,
    )
    .unwrap();
    let path = vault.join("view.okilog");
    (dir, vault, path)
}

#[gpui::test]
fn virtual_start_reads_prepared_snapshots_and_preserves_results_on_failure_and_stale_completion(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let (_dir, vault, path) = fixture();
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut panel = Panel::new(vault.clone(), window, cx);
            panel.configure(model::load(&vault, &path).unwrap(), window, cx);
            panel.watch = false;
            panel
        });
        panel = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1200.), px(900.)));
    visual.run_until_parked();
    let panel = panel.unwrap();
    panel.read_with(visual, |panel, _| {
        assert!(
            panel.worker.is_none(),
            "opening/configuring performs no selection"
        )
    });
    let start = visual.debug_bounds("virtual-start").unwrap();
    visual.simulate_click(start.center(), Modifiers::default());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        visual.run_until_parked();
        if panel.read_with(visual, |panel, _| panel.view.is_some()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "positive-control prepared files must reach the actual LogView"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    panel.read_with(visual, |panel, cx| {
        let view = panel.view.as_ref().unwrap().read(cx);
        assert_eq!(view.source_count(), 2);
        assert_eq!(view.total(), 2);
        assert_eq!(source_a(&view).raw(0).unwrap(), b"first");
        assert!(view.source_locked);
        assert_eq!(
            panel
                .candidates
                .iter()
                .filter(|candidate| candidate.reason == Reason::Matched)
                .count(),
            2
        );
        assert!(!panel.active);
    });
    // Selected files are already open immutable snapshots. Later mutations must
    // not be silently reread by the result renderer or source controls.
    std::fs::write(vault.join("logs/a.log"), "changed on disk\n").unwrap();
    panel.update(visual, |panel, cx| {
        let view = panel.view.as_ref().unwrap().clone();
        view.update(cx, |view, cx| {
            view.add_files(vec![vault.join("logs/b.log")], cx);
            view.toggle_follow(cx);
            view.apply_regex(None, cx);
            assert!(view._indexing.is_none());
            assert!(view.follow.is_none());
            assert_eq!(view.source_count(), 2);
            assert_eq!(source_a(&view).raw(0).unwrap(), b"first");
        });
        let id = view.entity_id();
        let generation = panel.generation;
        panel.receive(
            generation,
            worker::Frame::Failed("invalid query fixture".into()),
            cx,
        );
        assert!(panel.error);
        assert_eq!(panel.view.as_ref().unwrap().entity_id(), id);
        panel.active = true;
        panel.error = false;
        panel.message = "current".into();
        panel.receive(
            generation.wrapping_sub(1),
            worker::Frame::Failed("stale".into()),
            cx,
        );
        assert!(panel.active && !panel.error);
        assert_eq!(panel.message, "current");
        panel.stopping = true;
        let definition = model::load(&vault, &path).unwrap().definition;
        let selection = definition.plan(&vault, &path, 0).unwrap().select(|_, _| {});
        panel.receive(
            generation,
            worker::Frame::Ready {
                selection,
                elapsed: Duration::ZERO,
                watching: true,
            },
            cx,
        );
        assert!(
            !panel.active && !panel.stopping,
            "late Ready becomes stopped, never reactivates watch"
        );
        assert!(panel.pending.is_none());
        assert_eq!(panel.view.as_ref().unwrap().entity_id(), id);
        panel.receive(
            generation,
            worker::Frame::Invalidated("Source removed; snapshot retained".into(), false),
            cx,
        );
        assert_eq!(panel.view.as_ref().unwrap().entity_id(), id);
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("log-follow").is_none(),
        "definition snapshots cannot enter unrelated file Follow"
    );
    assert!(
        visual.debug_bounds("log-export").is_some(),
        "export is retained for the actual merged snapshot"
    );
}

#[test]
fn candidate_reasons_distinguish_exclusion_from_partial_coverage() {
    assert_eq!(reason(&Reason::Matched), "Selected");
    assert!(reason(&Reason::Unreadable("fixture error".into())).contains("fixture error"));
    assert!(reason(&Reason::IncompleteWindow).contains("partial"));
}

#[gpui::test]
fn ordinary_file_view_keeps_follow_and_source_controls(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let (_dir, vault, _) = fixture();
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            LogView::ready(
                LogDocument {
                    rel: "logs/a.log".into(),
                    file: Arc::new(LogFile::open(&vault.join("logs/a.log")).unwrap()),
                    elapsed: Duration::ZERO,
                },
                window,
                cx,
            )
        });
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1200.), px(900.)));
    visual.run_until_parked();
    assert!(visual.debug_bounds("log-follow").is_some());
    assert!(visual.debug_bounds("log-files").is_some());
}

#[gpui::test]
fn file_open_routes_definitions_inline_without_starting_selection_and_keeps_normal_logs(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let (dir, vault, path) = fixture();
    cx.update(|cx| {
        reader_ui_state::install(&dir.path().join("preferences"), cx);
        cx.set_global(reader_history::TestSessionDirectory(
            dir.path().join("history"),
        ));
    });
    std::fs::write(vault.join("home.md"), "# Home\n").unwrap();
    std::fs::write(vault.join("broken.okilog"), "not JSON").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    note: Some("home.md".into()),
                    index_dir: Some(dir.path().join("index")),
                    session_directory: Some(dir.path().join("state")),
                    panel_settings_override: Some(dir.path().join("panels.json")),
                    ..Default::default()
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
    let reader = reader.unwrap();
    reader.update_in(visual, |reader, window, cx| {
        reader.open_note("view.okilog", None, window, cx)
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        visual.run_until_parked();
        if reader.read_with(visual, |reader, cx| {
            reader
                .file_preview
                .as_ref()
                .and_then(|preview| preview.virtual_log.as_ref())
                .is_some_and(|panel| panel.read(cx).loaded.is_some())
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "definition must load through the real Reader file route"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    reader.read_with(visual, |reader, cx| {
        assert_eq!(reader.selected_file(), "view.okilog");
        let preview = reader.file_preview.as_ref().unwrap();
        assert!(preview.log.is_none() && preview.code.is_none() && preview.text.is_none());
        let panel = preview.virtual_log.as_ref().unwrap().read(cx);
        assert!(panel.embedded);
        assert_eq!(
            panel.loaded.as_ref().unwrap(),
            &path.canonicalize().unwrap()
        );
        assert!(
            panel.worker.is_none() && !panel.active,
            "file-open never selects sources or installs a source-root watch before Start"
        );
        assert_eq!(panel.generation, 0);
        assert!(panel.view.is_none() && panel.pending.is_none());
    });
    assert!(visual.debug_bounds("virtual-definition-view").is_some());
    let inline = reader.read_with(visual, |reader, _| {
        reader
            .file_preview
            .as_ref()
            .unwrap()
            .virtual_log
            .as_ref()
            .unwrap()
            .clone()
    });
    inline.update(visual, |panel, _| panel.watch = false);
    let start = visual.debug_bounds("virtual-start").unwrap();
    visual.simulate_click(start.center(), Modifiers::default());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        visual.run_until_parked();
        if inline.read_with(visual, |panel, _| panel.view.is_some()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "positive control: explicit Start must select the embedded source"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    inline.read_with(visual, |panel, cx| {
        assert!(panel.generation > 0);
        assert_eq!(panel.view.as_ref().unwrap().read(cx).total(), 2);
    });
    drop(inline);
    reader.update_in(visual, |reader, window, cx| {
        reader.preview_file("broken.okilog", window, cx)
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        visual.run_until_parked();
        if reader.read_with(visual, |reader, cx| {
            reader
                .file_preview
                .as_ref()
                .unwrap()
                .virtual_log
                .as_ref()
                .unwrap()
                .read(cx)
                .error
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "malformed definition must remain a visible typed error"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    visual.update(|window, cx| window.draw(cx).clear(cx));
    reader.read_with(visual, |reader, cx| {
        let preview = reader.file_preview.as_ref().unwrap();
        let panel = preview.virtual_log.as_ref().unwrap().read(cx);
        assert!(panel.loaded.is_none() && panel.worker.is_none());
        assert_eq!(
            PathBuf::from(panel.path.read(cx).value().as_str()),
            vault.join("broken.okilog").canonicalize().unwrap()
        );
        assert!(!panel.message.is_empty());
    });
    reader.update_in(visual, |reader, window, cx| {
        reader.preview_file("logs/a.log", window, cx)
    });
    visual.run_until_parked();
    reader.read_with(visual, |reader, _| {
        let preview = reader.file_preview.as_ref().unwrap();
        assert!(
            preview.log.is_some() && preview.virtual_log.is_none(),
            "ordinary log route stays intact"
        );
    });
}
