use super::*;
use ::core::prelude::v1::test;
use gpui_component::{input::Undo, WindowExt as _};
use std::sync::atomic::Ordering;

type Open<'a> = (
    Entity<Reader>,
    Entity<FileChrome>,
    Entity<EditorState>,
    &'a mut VisualTestContext,
    tempfile::TempDir,
);

fn open<'a>(cx: &'a mut TestAppContext, name: &str, source: &str) -> Open<'a> {
    cx.executor().allow_parking();
    let (reader, visual, temp) =
        crate::reader_viewer_chrome::tests::fixture_with_files(cx, name, &[(name, source)]);
    visual.simulate_resize(size(px(1440.), px(900.)));
    reader.update_in(visual, |r, w, cx| {
        w.activate_window();
        r.toggle_source(w, cx);
    });
    tick(visual, 130);
    let (file, input) = reader.read_with(visual, |r, _| {
        let e = r.editing.as_ref().expect("real file Edit entry");
        (e.file_chrome.clone().unwrap(), e.input.clone())
    });
    frame(visual);
    measured(visual, "file-edit-status-line");
    (reader, file, input, visual, temp)
}

fn frame(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.update(|w, cx| w.draw(cx).clear(cx));
}
fn tick(visual: &mut VisualTestContext, ms: u64) {
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(ms));
    frame(visual);
}
fn measured(visual: &mut VisualTestContext, name: &'static str) -> Bounds<Pixels> {
    crate::reader_viewer_chrome::tests::measured_bounds(visual, name)
        .unwrap_or_else(|| panic!("required mounted control: {name}"))
}
fn click(visual: &mut VisualTestContext, name: &'static str) {
    let at = measured(visual, name).center();
    visual.simulate_click(at, Modifiers::default());
    frame(visual);
}
fn drawn(input: &Entity<EditorState>) -> (usize, String, Vec<&'static str>) {
    DRAWN_STATUS.with(|all| {
        all.borrow()
            .get(&input.entity_id())
            .cloned()
            .expect("real bottom line rendered")
    })
}
fn text(input: &Entity<EditorState>, visual: &VisualTestContext) -> String {
    input.read_with(visual, |s, _| s.value().to_string())
}

// Initial inventory can be followed by a watcher batch and another Workbench
// publication. `workbench_ready.is_some()` can still name the previous batch.
fn startup_publication(reader: &Reader) -> Option<(u64, u64, u64)> {
    let current_rules = reader
        .tree_rules_source
        .as_ref()
        .is_some_and(|(vault, epoch)| {
            vault.ptr_eq(&Arc::downgrade(&reader.vault))
                && *epoch == reader.source_publication_epoch
        });
    let ready = !reader.loading.as_ref().is_some_and(|load| load.active)
        && !reader.incremental_initializing
        && !reader.incremental_active
        && reader.incremental_state.is_some()
        && reader.deferred_vault_changes.is_empty()
        && reader.watcher.is_some()
        && !reader.watcher_poll_active
        && reader
            .workbench_ready
            .as_ref()
            .is_some_and(|ready| Arc::ptr_eq(ready, &reader.workbench))
        && !reader.tree_rules_running
        && current_rules
        && Arc::ptr_eq(&reader.prepared_links, &reader.link_presentations);
    ready.then_some((
        reader.source_publication_epoch,
        reader.workbench_pending,
        reader.link_preparation_generation,
    ))
}

fn settle_startup(reader: &Entity<Reader>, visual: &mut VisualTestContext) -> (u64, u64, u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut stable = None;
    loop {
        // Keep the real watcher and its ordinary debounce/publication path.
        // The debounce uses wall time, while Workbench uses the test clock.
        reader.update_in(visual, |r, w, cx| r.poll_vault(w, cx));
        tick(visual, 50);
        let current = reader.read_with(visual, |r, _| startup_publication(r));
        if let Some(current) = current {
            let (previous, since) = stable.get_or_insert((current, std::time::Instant::now()));
            if *previous != current {
                *previous = current;
                *since = std::time::Instant::now();
            }
            if since.elapsed() >= okilum_core::watch::QUIET_WINDOW {
                diagnostic_error!(
                    "FILE2811_STARTUP_SETTLED epoch={} workbench={} links={} current_rules=true current_links=true watcher=true incremental_idle=true",
                    current.0, current.1, current.2
                );
                return current;
            }
        } else {
            stable = None;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "startup did not settle"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[gpui::test]
fn file2811_note_host_has_a_positive_control_and_file_keeps_one_header(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let note = "```json\n{\"needle\":1}\n```\n";
    let (reader, visual, _temp) =
        crate::reader_viewer_chrome::tests::fixture_with_files(cx, "note.md", &[("note.md", note)]);
    reader.update_in(visual, |r, w, cx| r.toggle_source(w, cx));
    reader
        .read_with(visual, |r, _| r.source_input().unwrap())
        .update(visual, |i, cx| i.set_selected_range(12..12, cx));
    tick(visual, 150);
    measured(visual, "source-code-format");
    measured(visual, "source-code-validate");
    measured(visual, "source-code-repair");
    measured(visual, "note-find");
    reader.update_in(visual, |r, w, cx| {
        r.open_source_find(cx);
        let _ = w;
    });
    tick(visual, 70);
    measured(visual, "search-panel");
    reader.update_in(visual, |r, w, cx| {
        r.toggle_source(w, cx);
        r.preview_file("data.json", w, cx);
    });
    tick(visual, 150);
    let header = measured(visual, "document-header-viewport");
    reader.update_in(visual, |r, w, cx| r.toggle_source(w, cx));
    tick(visual, 130);
    assert_eq!(measured(visual, "document-header-viewport"), header);
    measured(visual, "file-format");
    assert!(
        visual.debug_bounds("file-language").is_none(),
        "known extension keeps Language in the Format menu"
    );
    measured(visual, "viewer-find-field");
    measured(visual, "reader-read");
    for old in [
        "source-code-format",
        "source-code-validate",
        "source-code-repair",
        "source-code-validation",
        "note-find",
        "search-panel",
    ] {
        assert!(
            crate::reader_viewer_chrome::tests::measured_bounds(visual, old).is_none(),
            "file never mounts legacy {old}"
        );
    }
    reader.read_with(visual, |r, cx| {
        let chrome = r.viewer_chrome(cx);
        assert!(chrome
            .views
            .iter()
            .any(|v| v.active && v.action.id == "code-view-raw"));
        for v in chrome
            .views
            .iter()
            .filter(|v| v.action.id != "code-view-raw")
        {
            assert!(!v.action.enabled);
            assert!(v
                .action
                .disabled_reason
                .as_ref()
                .unwrap()
                .contains("while editing"));
        }
        for a in chrome
            .actions
            .iter()
            .filter(|a| a.id == "code-fold-all" || a.id == "code-unfold-all")
        {
            assert!(!a.enabled);
            assert!(a
                .disabled_reason
                .as_ref()
                .unwrap()
                .contains("Folding is not available"));
        }
    });
}

#[gpui::test]
fn file2811_live_json_badge_and_bottom_follow_one_revision_without_refresh(
    cx: &mut TestAppContext,
) {
    let (_reader, file, input, visual, _temp) = open(cx, "test1.json", "{\"a\":1}");
    assert_eq!(drawn(&input).1, "Valid JSON.");
    let before = file.read_with(visual, |s, _| (s.installs, s.scans.load(Ordering::SeqCst)));
    input.update_in(visual, |i, w, cx| i.replace_source_range(6..6, ",", w, cx));
    tick(visual, 119);
    assert_eq!(
        file.read_with(visual, |s, _| s.installs),
        before.0,
        "120 ms debounce is real"
    );
    tick(visual, 2);
    let status = drawn(&input);
    assert!(status.1.starts_with("Not valid JSON. Line 1, column"));
    assert_eq!(status.2, ["file-go-to-error", "file-status-repair"]);
    assert!(
        visual.debug_bounds("file-json-warning").is_some(),
        "live warning actually mounted without refresh"
    );
    file.read_with(visual, |s, cx| {
        assert_eq!(s.installs, before.0 + 1);
        assert_eq!(
            s.current.as_ref().unwrap().source.stamp,
            input.read(cx).source_stamp()
        );
        assert_eq!(
            s.last_valid
                .as_ref()
                .unwrap()
                .source
                .text
                .rope()
                .to_string(),
            "{\"a\":1}"
        );
    });
    input.update_in(visual, |i, w, cx| i.replace_source_range(6..7, "", w, cx));
    tick(visual, 121);
    assert_eq!(drawn(&input).1, "Valid JSON.");
    assert!(visual.debug_bounds("file-json-warning").is_none());
    assert!(!drawn(&input).1.contains("Not validated"));
}

#[gpui::test]
fn file2811_live_json_coalesces_keys_and_cancels_an_inflight_revision(cx: &mut TestAppContext) {
    let (_reader, file, input, visual, _temp) = open(cx, "test1.json", "{\"value\":\"\"}");
    let before = file.read_with(visual, |s, _| {
        (
            s.scans.load(Ordering::SeqCst),
            s.parses.load(Ordering::SeqCst),
            s.installs,
        )
    });
    for _ in 0..8 {
        input.update_in(visual, |i, w, cx| {
            i.replace_source_range(10..10, "n", w, cx)
        });
        tick(visual, 5);
    }
    assert_eq!(
        file.read_with(visual, |s, _| s.scans.load(Ordering::SeqCst)),
        before.0
    );
    tick(visual, 121);
    assert_eq!(
        file.read_with(visual, |s, _| s.parses.load(Ordering::SeqCst)),
        before.1 + 1
    );
    let (send, receive) = futures::channel::oneshot::channel();
    file.update(visual, |s, _| s.worker_gate = Some(receive));
    input.update_in(visual, |i, w, cx| i.replace_source_range(0..1, "[", w, cx));
    tick(visual, 121);
    assert!(
        file.read_with(visual, |s, _| s.parsing),
        "old worker is genuinely in flight"
    );
    input.update_in(visual, |i, w, cx| i.replace_source_range(0..1, "{", w, cx));
    send.send(()).unwrap();
    frame(visual);
    tick(visual, 121);
    file.read_with(visual, |s, cx| {
        assert_eq!(
            s.parses.load(Ordering::SeqCst),
            before.1 + 2,
            "cancelled worker never invokes the strict parser"
        );
        assert_eq!(
            s.installs,
            before.2 + 2,
            "only coalesced and current revisions install"
        );
        assert_eq!(
            s.current.as_ref().unwrap().source.stamp,
            input.read(cx).source_stamp()
        );
        assert!(matches!(s.current.as_ref().unwrap().verdict, Ok(Ok(()))));
    });
}

#[gpui::test]
fn file2811_header_find_and_typing_do_not_render_slots_or_notify_panels(cx: &mut TestAppContext) {
    struct Observer {
        _subscriptions: Vec<Subscription>,
    }
    let _cached = crate::reader_side_panel::tests::CachedPanels::on();
    let (reader, file, input, visual, temp) = open(
        cx,
        "test1.json",
        r#"{"a":"needle","b":"needle","c":"needle"}"#,
    );
    reader.update_in(visual, |r, w, cx| r.open_find(w, cx));
    visual.simulate_keystrokes("home");
    let startup = settle_startup(&reader, visual);
    reader.read_with(visual, |r, _| {
        let editing = r.editing.as_ref().unwrap();
        assert!(editing.autosave_schedule.deadline.is_none());
        assert!(editing.autosaving.is_none());
        assert!(!editing.store.has_pending_revision());
    });
    let counts = Rc::new(std::cell::Cell::new([0; 2]));
    let panels = reader.read_with(visual, |r, _| r.side_panels.clone());
    let _watch = visual.update(|_, cx| {
        cx.new(|cx: &mut Context<Observer>| Observer {
            _subscriptions: panels
                .into_iter()
                .enumerate()
                .map(|(n, p)| {
                    let counts = counts.clone();
                    cx.observe(&p.unwrap(), move |_, _, _| {
                        let mut c = counts.get();
                        c[n] += 1;
                        counts.set(c);
                    })
                })
                .collect(),
        })
    });
    let initial = crate::reader_viewer_chrome::header_renders(reader.entity_id());
    assert!(initial > 0, "positive control: real cached header rendered");
    for c in "needle".chars() {
        visual.simulate_input(&c.to_string());
        frame(visual);
        assert_eq!(
            crate::reader_viewer_chrome::header_renders(reader.entity_id()),
            initial
        );
        assert_eq!(counts.get(), [0, 0]);
    }
    tick(visual, 70);
    let target = file.read_with(visual, |s, _| s.find.clone());
    assert_eq!(target.read_with(visual, |s, cx| s.count(cx)), (0, 3));
    visual.simulate_keystrokes("enter");
    tick(visual, 70);
    let second =
        r#"{"a":"needle","b":"needle","c":"needle"}"#.match_indices("needle").nth(1).unwrap().0;
    assert_eq!(
        input.read_with(visual, |s, _| s.selected_range()),
        second..second + 6
    );
    visual.simulate_keystrokes("shift-enter");
    tick(visual, 70);
    assert_eq!(target.read_with(visual, |s, cx| s.count(cx)), (0, 3));
    assert!(input.read_with(visual, |s, _| s.search_session().external_presentation));
    assert!(visual.debug_bounds("search-panel").is_none());
    // Source mutation uses the same worker adapter; no matcher is rebuilt on a key.
    input.update_in(visual, |i, w, cx| {
        i.set_selected_range(6..6, cx);
        i.focus(w, cx);
    });
    visual.simulate_keystrokes("home");
    frame(visual);
    visual.update(|w, cx| {
        assert!(
            w.is_window_active(),
            "guard models typing in an active window, independently of focus-loss Save"
        );
        assert!(input.focus_handle(cx).is_focused(w));
    });
    let initial = crate::reader_viewer_chrome::header_renders(reader.entity_id());
    let typing_started = visual.update(|_, cx| cx.background_executor().now());
    let saves = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().saves);
    let disk_path = temp.path().join("vault/test1.json");
    let disk_before = std::fs::read(&disk_path).unwrap();
    counts.set([0, 0]);
    for _ in 0..5 {
        visual.simulate_input(" ");
        frame(visual);
        tick(visual, 121);
        assert_eq!(
            crate::reader_viewer_chrome::header_renders(reader.entity_id()),
            initial,
            "typing and live parse render zero cached slots"
        );
        assert_eq!(
            counts.get(),
            [0, 0],
            "typing and live parse notify zero panels"
        );
        let elapsed = visual.update(|_, cx| cx.background_executor().now()) - typing_started;
        assert!(elapsed < crate::reader_editor::AUTOSAVE_PAUSE);
        reader.read_with(visual, |r, _| {
            assert_eq!(startup_publication(r), Some(startup));
            let editing = r.editing.as_ref().unwrap();
            assert_eq!(editing.saves, saves);
            assert!(editing.autosaving.is_none());
        });
        assert_eq!(std::fs::read(&disk_path).unwrap(), disk_before);
    }
    reader.update(visual, |_, cx| cx.notify());
    frame(visual);
    assert_eq!(
        counts.get(),
        [1, 1],
        "positive control: both cached panel observers are live"
    );
}

fn open_menu(visual: &mut VisualTestContext) -> (Entity<PopupMenu>, Vec<(String, bool)>) {
    FORMAT_POPUP.with(|m| *m.borrow_mut() = None);
    let b = measured(visual, "file-format-split");
    visual.simulate_click(
        point(b.right() - px(10.), b.center().y),
        Modifiers::default(),
    );
    frame(visual);
    let (popup, items) =
        FORMAT_POPUP.with(|m| m.borrow().clone().expect("caret built its real menu"));
    let popup = popup.upgrade().expect("popup remains mounted");
    assert!(
        visual.update(|w, cx| popup.focus_handle(cx).is_focused(w)),
        "popup received focus"
    );
    (popup, items)
}

#[gpui::test]
fn file2811_format_menu_has_live_commands_language_and_busy_explanations(cx: &mut TestAppContext) {
    let (reader, _file, input, visual, _temp) = open(cx, "test1.json", "{\"a\":1}");
    let (popup, items) = open_menu(visual);
    assert_eq!(
        items,
        [
            ("Format code".into(), false),
            ("Validate code".into(), false),
            ("Repair code…".into(), false),
            ("Language: JSON".into(), false)
        ]
    );
    visual.simulate_keystrokes("down down enter");
    frame(visual);
    reader.read_with(visual, |r, _| {
        assert_eq!(
            r.editing.as_ref().unwrap().operations.last_action,
            Some(Action::Validate),
            "menu actually dispatches Validate"
        )
    });
    assert!(!visual.update(|w, cx| popup.focus_handle(cx).is_focused(w)));
    let (send, receive) = futures::channel::oneshot::channel();
    reader.update_in(visual, |r, w, cx| {
        r.editing.as_mut().unwrap().operations.worker_gate = Some(receive);
        r.code_operation(Action::Format, w, cx);
    });
    frame(visual);
    assert!(DRAWN_FORMAT.with(|d| d
        .borrow()
        .get(&input.entity_id())
        .unwrap()
        .as_ref()
        .unwrap()
        .contains("Formatting")));
    let (_, items) = open_menu(visual);
    for (label, disabled) in items.iter().take(3) {
        assert!(*disabled);
        assert!(label.contains("Formatting"));
    }
    visual.simulate_keystrokes("escape");
    send.send(()).unwrap();
    frame(visual);
    assert!(DRAWN_FORMAT.with(|d| d.borrow().get(&input.entity_id()).unwrap().is_none()));
}

#[gpui::test]
fn file2811_repair_sheet_keeps_top_cancel_bytes_apply_and_undo(cx: &mut TestAppContext) {
    let original = "{ a: 1 }";
    let (reader, _file, input, visual, temp) = open(cx, "broken.json", original);
    let path = temp.path().join("vault/broken.json");
    let body = measured(visual, "file-edit-body");
    let invalid = drawn(&input);
    assert!(invalid.1.starts_with("Not valid JSON. Line 1, column"));
    let original_stamp = input.read_with(visual, |s, _| s.source_stamp());
    reader.update_in(visual, |r, w, cx| r.code_operation(Action::Repair, w, cx));
    frame(visual);
    measured(visual, "file-repair-sheet");
    assert_eq!(
        measured(visual, "file-edit-body"),
        body,
        "sheet is an overlay; editor viewport stays unchanged"
    );
    assert_eq!(text(&input, visual), original);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    click(visual, "file-repair-cancel");
    assert!(visual.debug_bounds("file-repair-sheet").is_none());
    assert_eq!(
        input.read_with(visual, |s, _| s.source_stamp()),
        original_stamp
    );
    assert_eq!(
        drawn(&input).1,
        invalid.1,
        "Cancel restores the live invalid JSON bottom line"
    );
    assert_eq!(drawn(&input).2, invalid.2);
    reader.update_in(visual, |r, w, cx| r.code_operation(Action::Repair, w, cx));
    frame(visual);
    click(visual, "file-repair-apply");
    assert_eq!(text(&input, visual), "{ \"a\": 1 }");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        original,
        "Apply does not explicitly save"
    );
    assert_eq!(
        measured(visual, "file-edit-body"),
        body,
        "sheet is an overlay; editor viewport stays unchanged"
    );
    visual.dispatch_action(Undo);
    frame(visual);
    assert_eq!(
        text(&input, visual),
        original,
        "one Undo restores all of the repair"
    );
    reader.update_in(visual, |r, w, cx| r.code_operation(Action::Repair, w, cx));
    frame(visual);
    click(visual, "file-repair-apply");
    reader.update(visual, |r, cx| assert!(r.save_source(cx)));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        text(&input, visual)
    );
}

#[gpui::test]
fn file2811_repair_from_read_enters_edit_and_stale_sheet_closes(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (reader, visual, _temp) = crate::reader_viewer_chrome::tests::fixture_with_files(
        cx,
        "broken.json",
        &[("broken.json", "{ a: 1 }")],
    );
    tick(visual, 150);
    click(visual, "code-repair");
    assert!(reader.read_with(visual, |r, _| r.editing.is_some()));
    measured(visual, "file-repair-sheet");
    let input = reader.read_with(visual, |r, _| r.source_input().unwrap());
    input.update_in(visual, |s, w, cx| s.replace_source_range(5..6, "2", w, cx));
    frame(visual);
    assert!(
        visual.debug_bounds("file-repair-sheet").is_none(),
        "source mutation retires the actual sheet"
    );
    assert_eq!(text(&input, visual), "{ a: 2 }");
    reader.update_in(visual, |r, w, cx| r.apply_code_repair(w, cx));
    assert_eq!(text(&input, visual), "{ a: 2 }", "stale Apply is harmless");
    assert!(
        visual.update(|w, cx| !w.notifications(cx).is_empty()),
        "staleness has a real toast"
    );
}

/// #2884: the Format split is one control. Its action half and its menu half
/// share top, height and the header button height, meet at a 1 px seam and
/// fill the split's bounds exactly, so nothing steps or sticks out. The menu
/// half is measured on its own (`file-format-split-menu`), and the peer header
/// button is the control: it proves the measured height is the toolbar's.
#[gpui::test]
fn file2884_format_split_halves_form_one_control(cx: &mut TestAppContext) {
    let (_reader, _file, _input, visual, _temp) = open(cx, "test1.json", "{\"a\":1}");
    for mode in [
        gpui_component::ThemeMode::Light,
        gpui_component::ThemeMode::Dark,
    ] {
        visual.update(|_, cx| {
            gpui_component::Theme::change(mode, None, cx);
            brand::apply_theme(cx);
        });
        frame(visual);
        let header = measured(visual, "document-header-viewport");
        let split = measured(visual, "file-format-split");
        let action = measured(visual, "file-format");
        let menu = measured(visual, "file-format-split-menu");
        let peer = measured(visual, "reader-read");
        let context = format!("{mode:?}: action {action:?}, menu {menu:?}, split {split:?}");
        assert_eq!(action.top(), menu.top(), "halves share a top: {context}");
        assert_eq!(
            action.size.height, menu.size.height,
            "halves share a height: {context}"
        );
        assert_eq!(
            action.size.height, peer.size.height,
            "the split is as tall as the other header buttons: {context}, peer {peer:?}"
        );
        assert_eq!(
            (action.top(), action.bottom()),
            (split.top(), split.bottom()),
            "no half sticks out of the split: {context}"
        );
        let seam = menu.left() - action.right();
        assert!(
            seam >= px(0.) && seam <= px(1.),
            "halves are adjacent, a 1 px seam at most: {context}"
        );
        assert_eq!(
            action.left(),
            split.left(),
            "outline starts at the split: {context}"
        );
        assert_eq!(
            menu.right(),
            split.right(),
            "outline ends at the split: {context}"
        );
        assert!(
            split.top() >= header.top()
                && split.bottom() <= header.bottom()
                && split.left() >= header.left()
                && split.right() <= header.right(),
            "the split sits inside the header: {context}, header {header:?}"
        );
    }
    // The measured menu half is the real trigger: clicking its centre opens
    // the Format menu.
    FORMAT_POPUP.with(|m| *m.borrow_mut() = None);
    click(visual, "file-format-split-menu");
    assert!(
        FORMAT_POPUP.with(|m| m.borrow().is_some()),
        "the measured menu half opens the Format menu"
    );
}

#[gpui::test]
fn file2811_narrow_header_protects_find_read_save_format_and_more(cx: &mut TestAppContext) {
    let (_reader, _file, input, visual, _temp) = open(cx, "broken.json", "{ a: 1 }");
    for mode in [
        gpui_component::ThemeMode::Light,
        gpui_component::ThemeMode::Dark,
    ] {
        input.update_in(visual, |i, w, cx| {
            i.focus(w, cx);
            i.replace_source_range(0..0, " ", w, cx)
        });
        visual.update(|_, cx| {
            gpui_component::Theme::change(mode, None, cx);
            brand::apply_theme(cx);
        });
        visual.simulate_resize(size(px(700.), px(800.)));
        frame(visual);
        let header = measured(visual, "document-header-viewport");
        assert!(
            header.left() >= px(0.) && header.right() <= px(700.),
            "the viewport itself fits the 700 px window: {header:?}"
        );
        for id in [
            "viewer-find-field",
            "code-view-menu",
            "file-format-split",
            "reader-read",
            "source-save",
            "document-more",
        ] {
            let bounds = measured(visual, id);
            assert!(
                bounds.left() >= header.left() && bounds.right() <= header.right(),
                "{id} must fit: {bounds:?}, header {header:?}"
            );
            assert!(bounds.size.height >= px(28.), "{id} has a full hit area");
        }
        assert!(
            crate::reader_viewer_chrome::tests::measured_bounds(visual, "file-language").is_none()
        );
        let (_, items) = open_menu(visual);
        assert!(items.iter().any(|i| i.0 == "Language: JSON"));
        visual.simulate_keystrokes("escape");
        frame(visual);
    }
}

#[gpui::test]
fn file2811_return_to_read_restores_view_folds_query_and_does_not_remember_raw(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let (reader, visual, _temp) = crate::reader_viewer_chrome::tests::fixture_with_files(
        cx,
        "test1.json",
        &[("test1.json", r#"{"a":{"needle":1},"b":{"needle":2}}"#)],
    );
    tick(visual, 150);
    let code = reader.read_with(visual, |r, _| {
        r.file_preview.as_ref().unwrap().code.clone().unwrap()
    });
    code.update_in(visual, |s, w, cx| s.set_view(View::Tree, w, cx));
    frame(visual);
    click(visual, "data-node-fold-1");
    let target = code.read_with(visual, |s, cx| s.find_target(code.clone(), cx));
    visual.update(|w, cx| target.set_query("needle", w, cx));
    tick(visual, 70);
    let saved = code.read_with(visual, |s, cx| s.save_presentation(cx));
    reader.update_in(visual, |r, w, cx| r.toggle_source(w, cx));
    tick(visual, 130);
    assert_eq!(
        reader.read_with(visual, |r, cx| r
            .file_editor_find_target(cx)
            .unwrap()
            .input
            .read(cx)
            .value()
            .to_string()),
        "needle"
    );
    reader.update_in(visual, |r, w, cx| r.toggle_source(w, cx));
    tick(visual, 150);
    let restored = reader.read_with(visual, |r, _| {
        r.file_preview.as_ref().unwrap().code.clone().unwrap()
    });
    assert_eq!(
        restored.read_with(visual, |s, cx| s.save_presentation(cx)),
        saved
    );
    reader.read_with(visual, |_, cx| {
        assert_eq!(
            reader_ui_state::code_view("json", cx),
            Some(View::Tree),
            "internal Raw editing never becomes a remembered read choice"
        )
    });
}

#[test]
fn file2811_every_legacy_outcome_has_a_plain_file_home() {
    for (old, language, action, expected) in [
        ("Valid strict JSON", "json", None, "Valid JSON."),
        (
            "Validation unavailable above 256 KiB",
            "yaml",
            None,
            "Too large to validate. 300 KB is over the 256 KB limit.",
        ),
        (
            "Syntax validation for yaml works with files up to 256 KiB; this file is 0.3 MiB.",
            "yaml",
            None,
            "Too large to validate. 300 KB is over the 256 KB limit.",
        ),
        (
            "Invalid JSON · 2:3 · expected a value",
            "json",
            None,
            "Not valid JSON. Line 2, column 3: expected a value",
        ),
        (
            "Validation not checked for this revision",
            "rust",
            None,
            "Not validated since your last change.",
        ),
        (
            "Validation unavailable for jsonc; no other dialect substituted.",
            "jsonc",
            None,
            "Validation is not available for JSONC. Nothing was checked.",
        ),
        (
            "Syntax error (yaml) · 2:4 · missing value",
            "yaml",
            None,
            "Syntax problem in YAML. Line 2, column 4.",
        ),
        (
            "Syntax checked (yaml); semantic validation is not available.",
            "yaml",
            None,
            "No syntax problems found. Full validation is not available for YAML.",
        ),
        (
            "Syntax check exceeded its budget; source unchanged.",
            "yaml",
            None,
            "Syntax check stopped: it took too long. Nothing was changed.",
        ),
        (
            "JSON is invalid at 12:3; source unchanged.",
            "json",
            Some(Action::Format),
            "Not formatted. The text has a syntax error on line 12. Nothing was changed.",
        ),
        (
            "No formatter installed.",
            "yaml",
            Some(Action::Format),
            "Not formatted. No formatter installed. Nothing was changed.",
        ),
        (
            "Unterminated string.",
            "json",
            Some(Action::Repair),
            "No safe repair. Unterminated string. Nothing was changed.",
        ),
    ] {
        assert_eq!(status_text(old, language, action, 300 * 1024), expected);
    }
}

#[gpui::test]
fn file2811_error_navigation_uses_utf8_offsets(cx: &mut TestAppContext) {
    let (_reader, file, input, visual, _temp) = open(cx, "broken.json", "{\"é\":1,}");
    let expected = file.read_with(visual, |s, _| {
        s.current
            .as_ref()
            .unwrap()
            .diagnostic
            .as_ref()
            .unwrap()
            .offset
    });
    click(visual, "file-json-warning");
    assert_eq!(
        input.read_with(visual, |s, _| s.selected_range()),
        expected..expected
    );
}

#[gpui::test]
fn file2811_actual_save_failure_and_conflict_disable_all_commands(cx: &mut TestAppContext) {
    let (reader, _file, input, visual, temp) = open(cx, "test1.json", "{\"a\":1}");
    let path = temp.path().join("vault/test1.json");
    input.update_in(visual, |i, w, cx| i.replace_source_range(0..0, " ", w, cx));
    // The file's identity/content is unchanged; publication itself is refused.
    #[cfg(unix)]
    let protected = temp.path().join("vault");
    #[cfg(windows)]
    let protected = path.clone();
    struct Restore(PathBuf, std::fs::Permissions);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, self.1.clone());
        }
    }
    let restore = Restore(
        protected.clone(),
        std::fs::metadata(&protected).unwrap().permissions(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&protected, std::fs::Permissions::from_mode(0o500)).unwrap();
    }
    #[cfg(windows)]
    {
        let mut permissions = restore.1.clone();
        permissions.set_readonly(true);
        std::fs::set_permissions(&protected, permissions).unwrap();
    }
    reader.update(visual, |r, cx| {
        assert!(!r.save_source(cx));
        assert!(r.editing.as_ref().unwrap().save_failed);
    });
    frame(visual);
    assert!(drawn(&input).1.starts_with("Saving failed."));
    let (_, items) = open_menu(visual);
    for (label, disabled) in items.iter().take(3) {
        assert!(*disabled);
        assert!(label.contains("Saving failed"));
    }
    visual.simulate_keystrokes("escape");
    frame(visual);
    drop(restore);
    reader.update(visual, |r, cx| assert!(r.save_source(cx)));
    input.update_in(visual, |i, w, cx| i.replace_source_range(0..0, " ", w, cx));
    std::fs::write(&path, "{\"disk\":2}").unwrap();
    reader.update(visual, |r, cx| {
        assert!(!r.save_source(cx));
        assert!(r.editing.as_ref().unwrap().conflict_detected);
    });
    frame(visual);
    assert!(drawn(&input).1.starts_with("This file changed on disk."));
    let (_, items) = open_menu(visual);
    for (label, disabled) in items.iter().take(3) {
        assert!(*disabled);
        assert!(label.contains("changed on disk"));
    }
    visual.simulate_keystrokes("escape");
    frame(visual);
    measured(visual, "source-park");
    measured(visual, "file-conflict-actions");
}

#[gpui::test]
fn file2811_explicit_non_json_validation_refusal_and_shortcuts_use_the_new_host(
    cx: &mut TestAppContext,
) {
    let (reader, _file, input, visual, temp) = open(cx, "source.rs", "fn main() {}\n");
    assert_eq!(drawn(&input).1, "Not validated since your last change.");
    let bytes = std::fs::read(temp.path().join("vault/source.rs")).unwrap();
    input.update_in(visual, |i, w, cx| i.focus(w, cx));
    frame(visual);
    visual.simulate_keystrokes("alt-shift-v");
    frame(visual);
    assert!(drawn(&input).1.contains("No syntax problems found"));
    assert_eq!(
        std::fs::read(temp.path().join("vault/source.rs")).unwrap(),
        bytes
    );
    visual.simulate_keystrokes("alt-shift-f");
    frame(visual);
    assert!(drawn(&input).1.starts_with("Not formatted."));
    visual.simulate_keystrokes("alt-shift-r");
    frame(visual);
    assert!(drawn(&input).1.starts_with("No safe repair."));
    assert_eq!(
        std::fs::read(temp.path().join("vault/source.rs")).unwrap(),
        bytes
    );
    reader.read_with(visual, |r, _| {
        assert!(!r.editing.as_ref().unwrap().operations.busy)
    });
}

#[gpui::test]
fn file2811_primary_format_preserves_tokens_bom_crlf_and_one_undo(cx: &mut TestAppContext) {
    let original = "\u{feff}{\"a\":1,\"a\":2e3,\"str\":\"\\u0041\"}\r\n";
    let (reader, _file, input, visual, temp) = open(cx, "tokens.json", original);
    click(visual, "file-format");
    let formatted = text(&input, visual);
    assert_eq!(
        formatted,
        "\u{feff}{\r\n  \"a\": 1,\r\n  \"a\": 2e3,\r\n  \"str\": \"\\u0041\"\r\n}\r\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("vault/tokens.json")).unwrap(),
        original
    );
    assert!(
        visual.update(|w, cx| !w.notifications(cx).is_empty()),
        "Format actually posted its toast"
    );
    reader.read_with(visual, |r, _| {
        assert!(
            r.editing.as_ref().unwrap().operations.notice.is_none(),
            "success no longer occupies the bottom line"
        )
    });
    visual.dispatch_action(Undo);
    frame(visual);
    assert_eq!(text(&input, visual), original);
}

#[gpui::test]
fn file2811_late_format_and_park_cannot_apply_to_a_newer_source(cx: &mut TestAppContext) {
    let original = "{\"a\":1}";
    let (reader, _file, input, visual, _temp) = open(cx, "test1.json", original);
    // A real successful formatter is the control for the two rejected jobs.
    click(visual, "file-format");
    assert_ne!(text(&input, visual), original);
    visual.dispatch_action(Undo);
    frame(visual);
    assert_eq!(text(&input, visual), original);
    let (send, receive) = futures::channel::oneshot::channel();
    reader.update_in(visual, |r, w, cx| {
        r.editing.as_mut().unwrap().operations.worker_gate = Some(receive);
        r.code_operation(Action::Format, w, cx);
    });
    frame(visual);
    input.update_in(visual, |i, w, cx| i.replace_source_range(5..6, "2", w, cx));
    send.send(()).unwrap();
    frame(visual);
    assert_eq!(text(&input, visual), "{\"a\":2}");
    reader.read_with(visual, |r, _| {
        assert!(!r.editing.as_ref().unwrap().operations.busy)
    });
    assert!(visual.update(|w, cx| !w.notifications(cx).is_empty()));
    let (send, receive) = futures::channel::oneshot::channel();
    reader.update_in(visual, |r, w, cx| {
        r.editing.as_mut().unwrap().operations.worker_gate = Some(receive);
        r.code_operation(Action::Format, w, cx);
        let editing = r.editing.take().unwrap();
        let (store, restore) = editing.park(cx);
        r.editing = Some(restore(store));
        input.update(cx, |i, cx| i.set_disabled(false, cx));
    });
    send.send(()).unwrap();
    frame(visual);
    assert_eq!(text(&input, visual), "{\"a\":2}");
    click(visual, "file-format");
    assert_eq!(text(&input, visual), "{\n  \"a\": 2\n}");
}

#[gpui::test]
fn file2811_real_refusals_and_syntax_verdicts_have_one_bottom_home(cx: &mut TestAppContext) {
    let deep = format!("{{\"deep\":{}0{}}}", "[".repeat(129), "]".repeat(129));
    for (name, source, action, prefix, reason) in [
        (
            "invalid.json",
            "{ a: 1 }",
            Action::Format,
            "Not formatted.",
            "syntax error on line 1",
        ),
        (
            "mixed.json",
            "{\n\"a\":1\r\n}",
            Action::Format,
            "Not formatted.",
            "Mixed line endings",
        ),
        (
            "deep.json",
            deep.as_str(),
            Action::Format,
            "Not formatted.",
            "128 levels",
        ),
        (
            "unclosed.json",
            "{\"a\":\"unfinished}",
            Action::Repair,
            "No safe repair.",
            "Unterminated",
        ),
        (
            "mismatch.json",
            "{\"a\":[}",
            Action::Repair,
            "No safe repair.",
            "Mismatched",
        ),
        (
            "missing.json",
            "{\"a\":,}",
            Action::Repair,
            "No safe repair.",
            "No safe strict-JSON correction",
        ),
        (
            "dialect.jsonc",
            "// comment\n{}",
            Action::Validate,
            "Validation is not available for JSONC.",
            "Nothing was checked",
        ),
        (
            "data.yml",
            "value: [",
            Action::Validate,
            "Syntax problem in YAML.",
            "Line 1",
        ),
    ] {
        let (reader, _file, input, visual, temp) = open(cx, name, source);
        reader.update_in(visual, |r, w, cx| r.code_operation(action, w, cx));
        frame(visual);
        let bottom = drawn(&input).1;
        assert!(
            bottom.starts_with(prefix) && bottom.contains(reason),
            "{name}: {bottom}"
        );
        measured(visual, "file-edit-status-line");
        assert!(visual.debug_bounds("source-code-validation").is_none());
        assert_eq!(text(&input, visual), source);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("vault").join(name)).unwrap(),
            source
        );
    }
}

#[gpui::test]
fn file2811_raw_parts_and_async_formatted_restore_after_edit(cx: &mut TestAppContext) {
    let source = format!(
        "{{\"payload\":\"{}\",\"tail\":42}}",
        "value ".repeat(20_000)
    );
    for view in [View::Raw, View::Formatted] {
        cx.executor().allow_parking();
        let (reader, visual, _temp) = crate::reader_viewer_chrome::tests::fixture_with_files(
            cx,
            "paged.json",
            &[("paged.json", &source)],
        );
        tick(visual, 150);
        let code = reader.read_with(visual, |r, _| {
            r.file_preview.as_ref().unwrap().code.clone().unwrap()
        });
        code.update_in(visual, |c, w, cx| c.set_view(view, w, cx));
        frame(visual);
        if view == View::Raw {
            click(visual, "code-next-part");
        }
        reader.update_in(visual, |r, _, cx| {
            r.code_view_input(cx).unwrap().update(cx, |i, cx| {
                i.set_selected_range(2..8, cx);
                i.set_scroll_offset(point(px(-20.), px(-4.)), cx);
            });
        });
        let saved = code.read_with(visual, |c, cx| c.save_presentation(cx));
        reader.update_in(visual, |r, w, cx| r.toggle_source(w, cx));
        tick(visual, 130);
        reader.update_in(visual, |r, w, cx| r.toggle_source(w, cx));
        tick(visual, 150);
        let restored = reader.read_with(visual, |r, _| {
            r.file_preview.as_ref().unwrap().code.clone().unwrap()
        });
        assert_eq!(
            restored.read_with(visual, |c, cx| c.save_presentation(cx)),
            saved,
            "{view:?} restores its page, selection and scroll after worker preparation"
        );
    }
}

#[gpui::test]
fn file2811_ambiguous_language_has_a_button_then_a_narrow_menu(cx: &mut TestAppContext) {
    let (_reader, _file, _input, visual, _temp) = open(cx, "scratch.txt", "plain text");
    measured(visual, "file-language");
    let (_, items) = open_menu(visual);
    assert!(
        !items.iter().any(|i| i.0.starts_with("Language:")),
        "wide standalone Language is not repeated in Format"
    );
    visual.simulate_keystrokes("escape");
    frame(visual);
    visual.simulate_resize(size(px(700.), px(800.)));
    frame(visual);
    assert!(crate::reader_viewer_chrome::tests::measured_bounds(visual, "file-language").is_none());
    let (_, items) = open_menu(visual);
    assert!(
        items.iter().any(|i| i.0 == "Language: TEXT"),
        "narrow Language remains reachable"
    );
}

#[gpui::test]
fn file2811_conflicting_repair_closes_the_sheet_without_applying_or_writing(
    cx: &mut TestAppContext,
) {
    let original = "{ a: 1 }";
    let (reader, _file, input, visual, temp) = open(cx, "broken.json", original);
    input.update_in(visual, |i, w, cx| i.replace_source_range(0..0, " ", w, cx));
    tick(visual, 121);
    reader.update_in(visual, |r, w, cx| r.code_operation(Action::Repair, w, cx));
    frame(visual);
    measured(visual, "file-repair-sheet");
    let path = temp.path().join("vault/broken.json");
    let remote = "{\"remote\":2}";
    std::fs::write(&path, remote).unwrap();
    reader.update(visual, |r, cx| {
        assert!(!r.save_source(cx));
        assert!(r.editing.as_ref().unwrap().conflict_detected);
    });
    frame(visual);
    visual.update(|w, cx| w.clear_notifications(cx));
    click(visual, "file-repair-apply");
    assert!(
        visual.debug_bounds("file-repair-sheet").is_none(),
        "stale conflict retires the real sheet"
    );
    assert_eq!(text(&input, visual), format!(" {original}"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), remote);
    assert!(
        visual.update(|w, cx| !w.notifications(cx).is_empty()),
        "stale Apply has a real toast"
    );
}

#[gpui::test]
fn file2811_app_teardown_releases_live_parts_and_pending_json_validation(cx: &mut TestAppContext) {
    let mut app = cx.new_app();
    let (reader, file, input, visual, temp) = open(&mut app, "test1.json", "{\"a\":1}");
    input.update_in(visual, |i, w, cx| i.replace_source_range(6..6, ",", w, cx));
    visual.run_until_parked();
    file.read_with(visual, |s, _| {
        assert!(
            s.task.is_some() && !s.parsing,
            "positive control: strict validation debounce is still pending"
        );
    });
    let weak_file = file.downgrade();
    let weak_input = input.downgrade();
    assert!(weak_file.upgrade().is_some() && weak_input.upgrade().is_some());
    visual.update(|w, cx| {
        // VisualTestContext retains its TestWindow after close. Retire the
        // platform input handler, as destroying a native window would do.
        w.blur(cx);
        w.draw(cx).clear(cx);
        w.remove_window();
    });
    drop(reader);
    drop(file);
    drop(input);
    // Entity drops enqueue releases; flush an App effect cycle after the last
    // external owner is gone, before checking for a retained live-part cycle.
    app.update(|_| {});
    app.run_until_parked();
    assert!(
        weak_file.upgrade().is_none(),
        "closing the window releases its live parts before App shutdown"
    );
    assert!(
        weak_input.upgrade().is_none(),
        "pending validation never retains the source editor"
    );
    app.quit();
    drop(app);
    cx.run_until_parked();
    drop(temp);
}

#[gpui::test]
fn file2811_conflict_bottom_actions_are_not_covered_by_a_duplicate_toast(cx: &mut TestAppContext) {
    let (reader, _file, input, visual, temp) = open(cx, "test1.json", "{\"a\":1}");
    settle_startup(&reader, visual);
    reader.update(visual, |r, cx| {
        r.link_notice = Some("Unrelated notice remains visible".into());
        cx.notify();
    });
    frame(visual);
    assert!(
        visual.update(|w, cx| !w.notifications(cx).is_empty()),
        "positive control: the real persistent notice host is live"
    );
    reader.update(visual, |r, cx| {
        r.link_notice = None;
        cx.notify();
    });
    frame(visual);
    // Retire only the unrelated positive-control fixture before the conflict.
    // The conflict below must keep the notification host empty on its own.
    visual.update(|w, cx| w.clear_notifications(cx));
    // NotificationList retains dismissed entities through its 200 ms exit
    // animation; drive that lifecycle as its own flush_dismiss control does.
    tick(visual, 250);
    assert!(visual.update(|w, cx| w.notifications(cx).is_empty()));
    input.update_in(visual, |i, w, cx| i.replace_source_range(0..0, " ", w, cx));
    let path = temp.path().join("vault/test1.json");
    let remote = "{\"remote\":2}";
    std::fs::write(&path, remote).unwrap();
    reader.update(visual, |r, cx| {
        assert!(!r.save_source(cx));
        assert!(r.editing.as_ref().unwrap().conflict_detected);
    });
    frame(visual);
    assert!(drawn(&input).1.contains("This file changed on disk"));
    assert!(
        visual.update(|w, cx| w.notifications(cx).is_empty()),
        "the bottom-line conflict never mounts a second persistent toast"
    );
    click(visual, "source-park");
    assert!(
        reader.read_with(visual, |r, _| r.editing.is_none()),
        "the actual Close, keeping draft action remains clickable"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), remote);
}

/// #2887: an XML file (the QA showcase Ant build) had no Edit button and a
/// «view-only» lock. It now edits through the same header as YAML and TOML:
/// the real Edit button opens it, typed text lands, Validate runs the XML
/// syntax check, Format refuses without touching the text, and Save writes.
#[gpui::test]
fn xml2887_edit_button_typing_validate_format_and_save(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let name = "apache-ant-build.xml";
    let source = "<?xml version=\"1.0\"?>\n<project name=\"demo\" default=\"jar\">\n  <target name=\"jar\">\n    <jar destfile=\"demo.jar\" basedir=\"build\"/>\n  </target>\n</project>\n";
    let (reader, visual, temp) =
        crate::reader_viewer_chrome::tests::fixture_with_files(cx, name, &[(name, source)]);
    let path = temp.path().join("vault").join(name);
    visual.simulate_resize(size(px(1440.), px(900.)));
    reader.update_in(visual, |_, w, _| w.activate_window());
    tick(visual, 130);
    reader.read_with(visual, |r, cx| {
        assert_eq!(r.selected_file(), name);
        assert!(r.code_file_editable(cx), "XML must be editable");
        assert!(r.editing.is_none());
    });
    // The user's path: the header Edit button, not a direct call.
    click(visual, "reader-edit");
    tick(visual, 130);
    let input = reader.read_with(visual, |r, _| {
        r.editing
            .as_ref()
            .expect("Edit button opens the XML editor")
            .input
            .clone()
    });
    measured(visual, "reader-read");
    measured(visual, "file-edit-status-line");
    assert_eq!(text(&input, visual), source);
    assert!(input.read_with(visual, |i, _| i.is_editable()));

    // Typed input reaches the buffer (a comment after the root element).
    input.update_in(visual, |i, w, cx| {
        i.focus(w, cx);
        let position = gpui_component::input::Position {
            line: 6,
            character: 0,
        };
        i.set_cursor_position(position, w, cx);
    });
    frame(visual);
    visual.simulate_input("<!-- edited -->");
    frame(visual);
    let edited = text(&input, visual);
    assert_eq!(edited, format!("{source}<!-- edited -->"));

    // Validate runs the XML grammar, Format refuses and changes nothing.
    reader.update_in(visual, |r, w, cx| r.code_operation(Action::Validate, w, cx));
    frame(visual);
    let bottom = drawn(&input).1;
    assert!(bottom.contains("No syntax problems found"), "{bottom}");
    reader.update_in(visual, |r, w, cx| r.code_operation(Action::Format, w, cx));
    frame(visual);
    assert!(drawn(&input).1.starts_with("Not formatted."));
    assert_eq!(text(&input, visual), edited);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);

    // Save writes the edited text.
    reader.update(visual, |r, cx| assert!(r.save_source(cx)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);

    // A broken document gets the XML syntax verdict, not a silent pass.
    let end = edited.len();
    input.update_in(visual, |i, w, cx| {
        i.replace_source_range(end..end, "<broken", w, cx)
    });
    frame(visual);
    reader.update_in(visual, |r, w, cx| r.code_operation(Action::Validate, w, cx));
    frame(visual);
    let bottom = drawn(&input).1;
    assert!(bottom.starts_with("Syntax problem in XML."), "{bottom}");
}

/// #2887 review: Edit is not offered for an XML file declaring a non-UTF-8
/// encoding; Ctrl/Cmd+E does nothing and the bytes stay as they were.
#[gpui::test]
fn xml2887_non_utf8_declaration_offers_no_edit_and_never_writes(cx: &mut TestAppContext) {
    for (name, encoding) in [
        ("latin.xml", "ISO-8859-1"),
        ("cyrillic.xml", "Windows-1251"),
    ] {
        cx.executor().allow_parking();
        let source = format!("<?xml version=\"1.0\" encoding=\"{encoding}\"?>\n<a>cafe</a>\n");
        let (reader, visual, temp) =
            crate::reader_viewer_chrome::tests::fixture_with_files(cx, name, &[(name, &source)]);
        visual.simulate_resize(size(px(1440.), px(900.)));
        tick(visual, 130);
        reader.update_in(visual, |r, w, cx| {
            assert_eq!(r.selected_file(), name);
            assert!(!r.code_file_editable(cx), "{name}");
            r.toggle_source(w, cx);
            assert!(r.editing.is_none(), "{name}: Edit must not open");
        });
        frame(visual);
        assert!(visual.debug_bounds("reader-edit").is_none(), "{name}");
        assert_eq!(
            std::fs::read(temp.path().join("vault").join(name)).unwrap(),
            source.as_bytes()
        );
    }
}

/// #2887 review: a UTF-8 XML file round-trips non-ASCII text byte for byte.
#[gpui::test]
fn xml2887_utf8_non_ascii_round_trip(cx: &mut TestAppContext) {
    let source = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<a>Жcafé</a>\r\n";
    let (reader, _file, input, visual, temp) = open(cx, "utf8.xml", source);
    input.update_in(visual, |i, w, cx| {
        i.focus(w, cx);
        let position = gpui_component::input::Position {
            line: 2,
            character: 0,
        };
        i.set_cursor_position(position, w, cx);
    });
    frame(visual);
    visual.simulate_input("<!-- café Ж -->");
    frame(visual);
    let expected = format!("{source}<!-- café Ж -->");
    assert_eq!(text(&input, visual), expected);
    reader.update(visual, |r, cx| assert!(r.save_source(cx)));
    assert_eq!(
        std::fs::read(temp.path().join("vault/utf8.xml")).unwrap(),
        expected.as_bytes()
    );
}
