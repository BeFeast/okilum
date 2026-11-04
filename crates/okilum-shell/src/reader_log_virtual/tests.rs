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
        assert_eq!(source_a(view).raw(0).unwrap(), b"first");
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
            assert_eq!(source_a(view).raw(0).unwrap(), b"first");
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
            panel.active && panel.stopping,
            "late Ready cannot acknowledge worker exit"
        );
        panel.receive(generation, worker::Frame::Stopped, cx);
        assert!(!panel.active && !panel.stopping);
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
    // The ordinary Source command must acquire the same writer, not conflict
    // with the typed view's lock or bypass the Reader editor/recovery contract.
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx);
        assert!(
            reader.editing.is_some(),
            "explicit .okilog Source must open"
        );
        let panel = reader
            .file_preview
            .as_ref()
            .unwrap()
            .virtual_log
            .as_ref()
            .unwrap()
            .read(cx);
        assert!(panel.session.is_none());
        reader.toggle_source(window, cx);
        assert!(reader.editing.is_none());
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
                .session
                .is_some()
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "returning Read reopens canonical Save"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
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
    std::fs::remove_file(&path).unwrap();
    reader.update(visual, |reader, cx| {
        reader.file_preview.as_mut().unwrap().unavailable = Some("Deleted original".into());
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("virtual-definition-view").is_some(),
        "deleted original must not hide the retained criteria form"
    );
    assert!(visual.debug_bounds("reader-file-unavailable").is_none());
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

#[gpui::test]
fn canonical_save_button_keeps_original_identity_snapshot_draft_and_explicit_start(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let (dir, vault, path) = fixture();
    let state = dir.path().join("state");
    let other = vault.join("other.okilog");
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&other, &original).unwrap();
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut panel = Panel::new(vault.clone(), window, cx);
            panel.state = Some(state.clone());
            let mut loaded = model::load(&vault, &path).unwrap();
            loaded.session = Some(save::Session::open(&loaded, &state).unwrap());
            panel.configure(loaded, window, cx);
            panel.watch = false;
            panel
        });
        panel = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(900.)));
    visual.run_until_parked();
    let panel = panel.unwrap();
    let start = visual.debug_bounds("virtual-start").unwrap();
    visual.simulate_click(start.center(), Modifiers::default());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        visual.run_until_parked();
        if panel.read_with(visual, |panel, _| panel.view.is_some()) {
            break;
        }
        assert!(Instant::now() < deadline, "explicit Start positive control");
        std::thread::sleep(Duration::from_millis(1));
    }
    let (snapshot, generation) = panel.read_with(visual, |panel, _| {
        (panel.view.as_ref().unwrap().entity_id(), panel.generation)
    });
    panel.update_in(visual, |panel, window, cx| {
        panel.path.update(cx, |input, cx| {
            input.set_value(other.to_string_lossy().to_string(), window, cx)
        });
        panel.name.update(cx, |input, cx| {
            input.set_value("Saved by button", window, cx)
        });
    });
    visual.run_until_parked();
    let save = visual.debug_bounds("virtual-save").unwrap();
    visual.simulate_click(save.center(), Modifiers::default());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        visual.run_until_parked();
        if panel.read_with(visual, |panel, _| !panel.saving) {
            break;
        }
        assert!(Instant::now() < deadline, "Save must finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    panel.read_with(visual, |panel, cx| {
        assert!(!panel.error, "{}", panel.message);
        assert_eq!(panel.view.as_ref().unwrap().entity_id(), snapshot);
        assert!(panel.view.as_ref().unwrap().read(cx).source_locked);
        assert_eq!(panel.generation, generation);
        assert!(panel.worker.is_none() && !panel.active);
    });
    assert_eq!(
        model::load(&vault, &path).unwrap().definition.name,
        "Saved by button"
    );
    assert_eq!(std::fs::read(&other).unwrap(), original);
    assert_eq!(
        std::fs::read_to_string(vault.join("logs/a.log")).unwrap(),
        "first\n"
    );
    // Assert in-flight guards in the dispatching UI turn. A fast completed
    // button click above is allowed to finish before the next test update.
    panel.update(visual, |panel, cx| {
        panel.save(cx);
        assert!(panel.saving);
        panel.save(cx);
        panel.load_definition(other.clone(), cx);
        panel.start(cx);
        assert_eq!(
            panel.loaded.as_ref().unwrap(),
            &path.canonicalize().unwrap()
        );
        assert_eq!(panel.generation, generation);
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        visual.run_until_parked();
        if panel.read_with(visual, |panel, _| !panel.saving) {
            break;
        }
        assert!(Instant::now() < deadline, "no-op Save must finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    let saved = std::fs::read(&path).unwrap();
    panel.update_in(visual, |panel, window, cx| {
        panel.time = model::TimeMode::Last;
        panel
            .seconds
            .update(cx, |input, cx| input.set_value("not seconds", window, cx));
        panel.save(cx);
        assert!(panel.error && !panel.saving);
        assert_eq!(panel.seconds.read(cx).value().as_str(), "not seconds");
    });
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    panel.update_in(visual, |panel, window, cx| {
        panel.time = model::TimeMode::All;
        panel.name.update(cx, |input, cx| {
            input.set_value("Conflict draft", window, cx)
        });
    });
    std::fs::write(&path, "malformed external content").unwrap();
    panel.update(visual, |panel, cx| panel.save(cx));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        visual.run_until_parked();
        if panel.read_with(visual, |panel, _| !panel.saving) {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    panel.read_with(visual, |panel, cx| {
        assert!(panel.error && panel.message.contains("externally"));
        assert_eq!(panel.name.read(cx).value().as_str(), "Conflict draft");
        assert_eq!(panel.view.as_ref().unwrap().entity_id(), snapshot);
    });
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "malformed external content"
    );
    panel.update(visual, |panel, cx| assert!(panel.release_for_source(cx)));
    let recovered =
        okilum_core::file_editor::FileEditor::open(&path, &state.join("editor-drafts")).unwrap();
    assert_eq!(
        Definition::parse(recovered.text()).unwrap().name,
        "Conflict draft"
    );
}

#[gpui::test]
fn membership_follow_keeps_view_record_selection_marks_and_rejects_stale_or_stopped_batches(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let (_dir, vault, path) = fixture();
    let definition = model::load(&vault, &path).unwrap().definition;
    let plan = definition.plan(&vault, &path, 0).unwrap();
    let selection = plan.select(|_, _| {});
    let mut session = follow::Session::new(&selection).unwrap();
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut panel = Panel::new(vault.clone(), window, cx);
            panel.configure(model::load(&vault, &path).unwrap(), window, cx);
            panel
        });
        panel = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(900.)));
    visual.run_until_parked();
    let panel = panel.unwrap();
    panel.read_with(visual, |panel, _| {
        assert!(!panel.following && panel.worker.is_none())
    });
    let follow = visual.debug_bounds("virtual-follow").unwrap();
    visual.simulate_click(follow.center(), Modifiers::default());
    panel.update(visual, |panel, cx| {
        assert!(
            panel.following && !panel.watch && panel.worker.is_none(),
            "opting in never starts I/O"
        );
        panel.generation = 1;
        panel.run_name = "Follow fixture".into();
        panel.receive(
            1,
            worker::Frame::Ready {
                selection,
                elapsed: Duration::ZERO,
                watching: true,
            },
            cx,
        );
    });
    visual.run_until_parked();
    let identity = panel.update(visual, |panel, cx| {
        let view = panel.view.as_ref().unwrap();
        let identity = view.entity_id();
        view.update(cx, |view, cx| {
            view.selected = view.id_of(1, 0);
            view.anchor = view.selected;
            view.marks.insert((1, 0));
            view.set_text("second".into(), cx);
        });
        identity
    });
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(vault.join("logs/a.log"))
        .unwrap()
        .write_all(b"appended\n")
        .unwrap();
    std::fs::write(vault.join("logs/c.log"), "new member\n").unwrap();
    let changes = vec![okilum_core::log::virtual_log::watcher::Change::Changed(
        vec![vault.join("logs/a.log"), vault.join("logs/c.log")],
    )];
    let update = session
        .refresh(&plan, &changes, &std::sync::atomic::AtomicBool::new(false))
        .unwrap();
    panel.update(visual, |panel, cx| {
        panel.receive(1, worker::Frame::Updated(update), cx)
    });
    visual.run_until_parked();
    panel.read_with(visual, |panel, cx| {
        let view = panel.view.as_ref().unwrap();
        assert_eq!(view.entity_id(), identity);
        let view = view.read(cx);
        assert_eq!(view.total(), 4);
        assert_eq!(view.visible_len(), 1);
        assert!(!view.filtering);
        assert_eq!(view.locate(view.selected.unwrap()), Some((1, 0)));
        assert_eq!(view.locate(view.anchor.unwrap()), Some((1, 0)));
        assert!(view.marks.contains(&(1, 0)) && view.source_locked);
    });
    std::fs::write(vault.join("logs/d.log"), "late member\n").unwrap();
    let update = session
        .refresh(&plan, &changes, &std::sync::atomic::AtomicBool::new(false))
        .unwrap();
    let pending_sources = update.sources.clone();
    panel.update(visual, |panel, cx| {
        panel.receive(
            0,
            worker::Frame::Updated(follow::Update {
                sources: update.sources.clone(),
                candidates: update.candidates.clone(),
                more: update.more,
                notice: update.notice.clone(),
            }),
            cx,
        );
        assert!(panel.pending_follow.is_none());
        panel.stopping = true;
        panel.receive(1, worker::Frame::Updated(update.clone()), cx);
        panel.receive(1, worker::Frame::Updated(update.clone()), cx);
        assert!(panel.pending_follow.is_none() && panel.active && panel.stopping);
        panel.receive(1, worker::Frame::Stopped, cx);
        assert!(!panel.active && !panel.stopping);
        panel.receive(1, worker::Frame::Updated(update), cx);
        assert!(
            panel.pending_follow.is_none(),
            "terminal generation rejects every later queued batch"
        );
    });
    visual.run_until_parked();
    panel.read_with(visual, |panel, cx| {
        assert_eq!(panel.view.as_ref().unwrap().entity_id(), identity);
        assert_eq!(panel.view.as_ref().unwrap().read(cx).total(), 4);
    });
    panel.update(visual, |panel, cx| {
        panel.pending_follow = Some(pending_sources);
        panel.following = false;
        panel.watch = false; // Snapshot fixture: no real watcher is installed.
        panel.start(cx);
        assert!(
            panel.pending_follow.is_none() && panel.pending.is_none(),
            "new Start fences previous unpainted batches"
        );
        panel.stop(cx);
    });
}

#[gpui::test]
fn observed_rotation_keeps_old_record_marks_and_labels_history_without_replacing_the_view(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let (_dir, vault, path) = fixture();
    let definition = model::load(&vault, &path).unwrap().definition;
    let plan = definition.plan(&vault, &path, 0).unwrap();
    let selection = plan.select(|_, _| {});
    let mut session = follow::Session::new(&selection).unwrap();
    let mut panel = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut panel = Panel::new(vault.clone(), window, cx);
            panel.configure(model::load(&vault, &path).unwrap(), window, cx);
            panel.following = true;
            panel.generation = 1;
            panel.receive(
                1,
                worker::Frame::Ready {
                    selection,
                    elapsed: Duration::ZERO,
                    watching: true,
                },
                cx,
            );
            panel
        });
        panel = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(900.)));
    visual.run_until_parked();
    let panel = panel.unwrap();
    let id = panel.update(visual, |panel, cx| {
        let view = panel.view.as_ref().unwrap();
        let id = view.entity_id();
        view.update(cx, |view, _| {
            view.selected = view.id_of(0, 0);
            view.anchor = view.selected;
            view.marks.insert((0, 0));
        });
        id
    });
    std::fs::rename(vault.join("logs/a.log"), vault.join("logs/a.log.1")).unwrap();
    std::fs::write(vault.join("logs/a.log"), "replacement\n").unwrap();
    let update = session
        .refresh(
            &plan,
            &[okilum_core::log::virtual_log::watcher::Change::Changed(
                vec![vault.join("logs/a.log")],
            )],
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
    panel.update(visual, |panel, cx| {
        panel.receive(1, worker::Frame::Updated(update), cx)
    });
    visual.run_until_parked();
    panel.read_with(visual, |panel, cx| {
        let entity = panel.view.as_ref().unwrap();
        assert_eq!(entity.entity_id(), id);
        let view = entity.read(cx);
        assert_eq!(view.total(), 3);
        assert_eq!(view.locate(view.selected.unwrap()), Some((0, 0)));
        assert_eq!(view.locate(view.anchor.unwrap()), Some((0, 0)));
        assert!(view.marks.contains(&(0, 0)) && view.source_locked);
        assert_eq!(view.source(0).unwrap().raw(0).unwrap(), b"first");
        assert_eq!(view.source(2).unwrap().raw(0).unwrap(), b"replacement");
        assert!(view.source_name(0).contains("before rotation"));
        assert!(panel.message.contains("unobserved gaps"));
    });
    panel.update(visual, |panel, cx| {
        panel.receive(
            1,
            worker::Frame::Failed("watch coverage lost; snapshot retained".into()),
            cx,
        )
    });
    visual.run_until_parked();
    panel.read_with(visual, |panel, cx| {
        assert!(panel.error && !panel.active);
        assert_eq!(panel.view.as_ref().unwrap().entity_id(), id);
        assert_eq!(panel.view.as_ref().unwrap().read(cx).total(), 3);
    });
}
