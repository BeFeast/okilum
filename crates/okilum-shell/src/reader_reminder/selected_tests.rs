use super::*;
use ::core::prelude::v1::test;
use gpui::{TestAppContext, VisualTestContext};
use selection::test_hooks::{Harness, Hooks};
use std::cell::RefCell;

thread_local! {
    static EDITOR_REQUEST: RefCell<Option<selection::Request>> = const { RefCell::new(None) };
}
pub(super) fn record_editor_request(request: Option<selection::Request>) {
    EDITOR_REQUEST.with(|slot| *slot.borrow_mut() = request);
}
const SOURCE: &str = "# Winter\r\n\r\n1 November 2027\r\n";
const LINE: &str = "- [ ] 1 November 2027 [[Source.md#Winter]] 📅 2027-11-01\n";
const DASHBOARD: &str =
    "---\nview: tasks\n---\n## Reminders\n```tasks\nnot done\nsort by path\n```\n";
fn date() -> time::Date {
    time::macros::date!(2027 - 11 - 01)
}

fn mount<'a>(
    cx: &'a mut TestAppContext,
    temp: &tempfile::TempDir,
) -> (Entity<Reader>, &'a mut VisualTestContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        cx.set_global(Harness::default());
    });
    let root = temp.path().join("vault");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("Source.md"), SOURCE).unwrap();
    std::fs::write(root.join("Dashboard.md"), DASHBOARD).unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Source.md".into()),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(960.)));
    (reader.unwrap(), visual)
}
async fn ready(reader: &Entity<Reader>, visual: &mut VisualTestContext) {
    visual
        .condition(reader, |reader, _| {
            reader.current_rel == "Source.md"
                && reader.document_ready()
                && reader.tasks_index.is_some()
                && !reader.incremental_initializing
        })
        .await;
    reader.update(visual, |reader, _| {
        reader.watcher.take();
    });
    visual.run_until_parked();
}
fn select_date(reader: &Entity<Reader>, visual: &mut VisualTestContext) -> Point<Pixels> {
    visual.run_until_parked();
    let origin = reader.read_with(visual, |reader, cx| {
        crate::reader_scroll_geometry_tests::paragraph_origin(reader, cx, 1)
    });
    let start = origin + point(px(2.), px(10.));
    let end = origin + point(px(400.), px(10.));
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    visual.run_until_parked();
    reader.read_with(visual, |reader, cx| {
        assert_eq!(
            reader.content.read(cx).selected_text().trim(),
            "1 November 2027",
            "painted date drag positive control"
        )
    });
    origin + point(px(20.), px(10.))
}
fn open_menu(reader: &Entity<Reader>, visual: &mut VisualTestContext) {
    let at = select_date(reader, visual);
    visual.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("remind-me-item").is_some(),
        "real selection context menu offers the resolved reminder"
    );
    reader.read_with(visual, |reader, cx| {
        assert_eq!(reader.reminder_for_selection(cx).unwrap().1, date());
        assert_eq!(date_label(date()), "Mon, 1 Nov 2027");
    });
}
fn click_item(visual: &mut VisualTestContext) {
    let bounds = visual.debug_bounds("remind-me-item").unwrap();
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("remind-me-item").is_none(),
        "the actual menu dismisses after its action"
    );
}
fn completed(cx: &App, root: &Path, count: usize) -> bool {
    cx.global::<Harness>()
        .completed
        .get(root)
        .is_some_and(|results| results.len() >= count)
}
async fn added(
    reader: &Entity<Reader>,
    visual: &mut VisualTestContext,
    root: &Path,
    note: &str,
    count: usize,
    tasks: usize,
) {
    visual.update(|_, cx| {
        assert_eq!(
            cx.global::<Harness>().attempts.last(),
            Some(&true),
            "actual menu/action reached the writer with current retained source authority"
        );
    });
    diagnostic_error!("Reminder test waiting for {note}: completions={count}, tasks={tasks}");
    visual
        .condition(reader, |reader, cx| {
            completed(cx, root, count)
                && reader
                    .tasks_index
                    .as_ref()
                    .is_some_and(|index| index.note_tasks(note).len() == tasks)
        })
        .await;
}
fn click_undo(visual: &mut VisualTestContext) {
    std::thread::sleep(Duration::from_millis(450));
    visual.update(|window, _| window.refresh());
    visual.executor().advance_clock(Duration::from_millis(400));
    visual.run_until_parked();
    let undo = visual
        .debug_bounds("undo-reminder")
        .expect("current actual reminder receipt Undo");
    visual.update(|window, _| {
        assert!(Bounds::new(point(px(0.), px(0.)), window.viewport_size()).contains(&undo.center()))
    });
    visual.simulate_mouse_move(undo.center(), None, Modifiers::default());
    visual.simulate_click(undo.center(), Modifiers::default());
    visual.run_until_parked();
}

#[gpui::test]
async fn reminder_724_read_menu_create_append_undo_updates_tasks_and_preserves_source(
    cx: &mut TestAppContext,
) {
    let temp = tempfile::tempdir().unwrap();
    let (reader, visual) = mount(cx, &temp);
    let root = temp.path().join("vault");
    ready(&reader, visual).await;
    // A non-date selection offers nothing; the same mounted menu below is its positive control.
    reader.update(visual, |reader, cx| {
        reader
            .content
            .update(cx, |content, cx| content.select_all(cx))
    });
    visual.run_until_parked();
    let at = reader.read_with(visual, |reader, cx| {
        crate::reader_scroll_geometry_tests::paragraph_origin(reader, cx, 1)
    }) + point(px(20.), px(10.));
    visual.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("remind-me-item").is_none());
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    open_menu(&reader, visual);
    click_item(visual);
    added(&reader, visual, &root, NOTE, 1, 1).await;
    assert_eq!(std::fs::read_to_string(root.join(NOTE)).unwrap(), LINE);
    click_undo(visual);
    visual
        .condition(&reader, |reader, cx| {
            cx.global::<Harness>().undone == [true]
                && reader
                    .tasks_index
                    .as_ref()
                    .is_some_and(|index| index.note_tasks(NOTE).is_empty())
        })
        .await;
    assert_eq!(
        std::fs::read_to_string(root.join(NOTE)).unwrap(),
        "",
        "exclusive-create Undo keeps the safe empty note"
    );
    let prelude = "\u{feff}# Existing reminders שלום\r\n";
    let crlf_line = LINE.replace('\n', "\r\n");
    std::fs::write(root.join(NOTE), prelude).unwrap();
    for count in [2, 3] {
        open_menu(&reader, visual);
        click_item(visual);
        added(&reader, visual, &root, NOTE, count, count - 1).await;
    }
    assert_eq!(
        std::fs::read_to_string(root.join(NOTE)).unwrap(),
        format!("{prelude}\r\n{crlf_line}\r\n{crlf_line}")
    );
    click_undo(visual);
    visual
        .condition(&reader, |reader, cx| {
            cx.global::<Harness>().undone == [true, true]
                && reader
                    .tasks_index
                    .as_ref()
                    .is_some_and(|index| index.note_tasks(NOTE).len() == 1)
        })
        .await;
    assert_eq!(
        std::fs::read_to_string(root.join(NOTE)).unwrap(),
        format!("{prelude}\r\n{crlf_line}")
    );
    reader.update_in(visual, |reader, window, cx| {
        reader.open_note("Dashboard.md", None, window, cx)
    });
    visual
        .condition(&reader, |reader, _| {
            reader.current_rel == "Dashboard.md" && reader.document_ready()
        })
        .await;
    visual.run_until_parked();
    assert!(visual.debug_bounds("native-tasks-dashboard").is_some());
    assert!(
        visual.debug_bounds("task-text-5-0").is_some(),
        "real dashboard paints the reminder row"
    );
    reader.read_with(visual, |reader, _| {
        assert_eq!(
            reader.tasks_index.as_ref().unwrap().note_tasks(NOTE)[0].due,
            Some(date())
        )
    });
    assert_eq!(
        std::fs::read_to_string(root.join("Source.md")).unwrap(),
        SOURCE
    );
}

fn editor_action(reader: &Entity<Reader>, visual: &mut VisualTestContext) -> RemindOnSelection {
    EDITOR_REQUEST.with(|slot| *slot.borrow_mut() = None);
    let input = reader.read_with(visual, |reader, _| {
        reader.editing.as_ref().unwrap().input().clone()
    });
    visual.run_until_parked();
    let at = input.read_with(visual, |state, _| {
        assert_eq!(
            state.selected_text().to_string(),
            "1 November 2027",
            "actual editor selection before right-click"
        );
        state
            .range_to_bounds(&state.selected_range())
            .expect("selected date has painted editor geometry")
            .center()
    });
    reader.read_with(visual, |reader, _| {
        let facts = reader.editing.as_ref().unwrap().menu_facts();
        assert_eq!(
            facts.date(),
            Some(date()),
            "the observer mirrored the actual selected date"
        );
        assert!(
            facts.3.borrow().is_some(),
            "captured request exists before menu construction"
        );
    });
    let before = MENU_BUILDS.with(|count| count.get());
    visual.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        MENU_BUILDS.with(|count| count.get()),
        before + 1,
        "actual editor context builder ran"
    );
    let request = EDITOR_REQUEST
        .with(|slot| slot.borrow().clone())
        .expect("actual menu retained the date request outside the input update");
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    input.update_in(visual, |state, window, cx| state.focus(window, cx));
    RemindOnSelection {
        request: Some(request),
    }
}

#[gpui::test]
async fn reminder_724_editor_menu_captures_original_selection_and_unsaved_source(
    cx: &mut TestAppContext,
) {
    let temp = tempfile::tempdir().unwrap();
    let (reader, visual) = mount(cx, &temp);
    let root = temp.path().join("vault");
    ready(&reader, visual).await;
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    visual.run_until_parked();
    let input = reader.read_with(visual, |reader, _| {
        reader.editing.as_ref().unwrap().input().clone()
    });
    let unsaved = "# Unsaved heading\r\n\r\nMeet 1 November 2027 in Jerusalem.\r\n";
    input.update_in(visual, |state, window, cx| {
        state.focus(window, cx);
        state.set_selected_range(0..SOURCE.len(), cx);
    });
    visual.simulate_input(unsaved);
    visual.run_until_parked();
    let authored_snapshot = input.read_with(visual, |state, _| state.value().to_string());
    input.update(visual, |state, cx| {
        let at = state.value().find("1 November 2027").unwrap();
        state.set_selected_range(at..at + "1 November 2027".len(), cx);
    });
    visual.run_until_parked();
    let action = editor_action(&reader, visual);
    visual.dispatch_action(action);
    added(&reader, visual, &root, NOTE, 1, 1).await;
    assert_eq!(
        std::fs::read_to_string(root.join(NOTE)).unwrap(),
        "- [ ] Meet 1 November 2027 in Jerusalem. [[Source.md#Unsaved heading]] 📅 2027-11-01\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("Source.md")).unwrap(),
        SOURCE,
        "reminder never saves the dirty source editor"
    );
    input.read_with(visual, |state, _| {
        assert_eq!(state.value().as_str(), authored_snapshot)
    });
    assert!(reader.read_with(visual, |reader, cx| reader.source_is_dirty(cx)));
}

#[gpui::test]
async fn reminder_724_stale_reader_and_editor_menu_refuse_without_retarget(
    cx: &mut TestAppContext,
) {
    let temp = tempfile::tempdir().unwrap();
    let (reader, visual) = mount(cx, &temp);
    let root = temp.path().join("vault");
    ready(&reader, visual).await;
    open_menu(&reader, visual);
    reader.update(visual, |reader, cx| {
        reader.navigation.generation += 1;
        cx.notify();
    });
    click_item(visual);
    assert!(!root.join(NOTE).exists());
    assert!(visual.debug_bounds("reader-error-toast").is_some());
    open_menu(&reader, visual);
    click_item(visual);
    added(&reader, visual, &root, NOTE, 1, 1).await;
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    visual.run_until_parked();
    let input = reader.read_with(visual, |reader, _| {
        reader.editing.as_ref().unwrap().input().clone()
    });
    let at = input.read_with(visual, |state, _| {
        state.value().find("1 November 2027").unwrap()
    });
    input.update(visual, |state, cx| {
        state.set_selected_range(at..at + "1 November 2027".len(), cx)
    });
    visual.run_until_parked();
    let stale = editor_action(&reader, visual);
    input.update(visual, |state, cx| state.set_selected_range(0..1, cx));
    visual.run_until_parked();
    visual.dispatch_action(stale);
    visual.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(root.join(NOTE)).unwrap(),
        LINE,
        "old label never retargets a new editor selection"
    );
    input.update(visual, |state, cx| {
        state.set_selected_range(at..at + "1 November 2027".len(), cx)
    });
    visual.run_until_parked();
    let fresh = editor_action(&reader, visual);
    visual.dispatch_action(fresh);
    added(&reader, visual, &root, NOTE, 2, 2).await;
    assert_eq!(
        std::fs::read_to_string(root.join("Source.md")).unwrap(),
        SOURCE
    );
}

#[gpui::test]
async fn reminder_724_held_worker_and_completion_refuse_rebound_owner(cx: &mut TestAppContext) {
    for phase in ["before", "after"] {
        let temp = tempfile::tempdir().unwrap();
        let (reader, visual) = mount(cx, &temp);
        let root = temp.path().join("vault");
        ready(&reader, visual).await;
        let (release, held) = async_channel::bounded(1);
        let (entered, boundary) = async_channel::bounded(1);
        visual.update(|_, cx| {
            cx.global_mut::<Harness>().queued.insert(
                root.clone(),
                Hooks {
                    before: (phase == "before").then_some(held.clone()),
                    after: (phase == "after").then_some(held),
                    entered: Some(entered),
                },
            );
        });
        open_menu(&reader, visual);
        click_item(visual);
        visual.update(|_, cx| {
            assert_eq!(
                cx.global::<Harness>().attempts.last(),
                Some(&true),
                "held-boundary action must have reached the current request"
            )
        });
        match futures::future::select(
            Box::pin(boundary.recv()),
            Box::pin(visual.executor().timer(Duration::from_secs(3))),
        )
        .await
        {
            futures::future::Either::Left((result, _)) => {
                result.expect("owned worker entered the held boundary")
            }
            futures::future::Either::Right(_) => panic!(
                "real {phase}-write boundary was not reached; refusing an unbounded fixture wait"
            ),
        }
        if phase == "after" {
            assert_eq!(std::fs::read_to_string(root.join(NOTE)).unwrap(), LINE);
        } else {
            assert!(!root.join(NOTE).exists());
        }
        reader.update(visual, |reader, cx| {
            reader.vault_root = temp.path().join("replacement");
            reader.navigation.generation += 1;
            cx.notify();
        });
        visual.run_until_parked();
        release.try_send(()).unwrap();
        visual
            .condition(&reader, |_, cx| completed(cx, &root, 1))
            .await;
        visual.update(|_, cx| assert!(!cx.global::<Harness>().completed[&root][0].1));
        assert!(
            visual.debug_bounds("undo-reminder").is_none(),
            "stale completion publishes no receipt in the new owner"
        );
        reader.update(visual, |reader, cx| {
            reader.vault_root = root.clone();
            reader.navigation.generation += 1;
            cx.notify();
        });
        visual.run_until_parked();
        open_menu(&reader, visual);
        click_item(visual);
        added(
            &reader,
            visual,
            &root,
            NOTE,
            2,
            if phase == "after" { 2 } else { 1 },
        )
        .await;
        visual.update(|_, cx| assert_eq!(cx.global::<Harness>().completed[&root][1], (true, true)));
        assert_eq!(
            std::fs::read_to_string(root.join("Source.md")).unwrap(),
            SOURCE
        );
    }
}

#[gpui::test]
async fn reminder_724_readonly_missing_vault_and_storage_errors_keep_source(
    cx: &mut TestAppContext,
) {
    use std::os::unix::fs::PermissionsExt;
    for problem in ["readonly", "missing vault", "no storage"] {
        let temp = tempfile::tempdir().unwrap();
        let (reader, visual) = mount(cx, &temp);
        let root = temp.path().join("vault");
        ready(&reader, visual).await;
        let note = root.join(NOTE);
        let moved = temp.path().join("moved-vault");
        match problem {
            "readonly" => {
                std::fs::write(&note, "Existing prose\n").unwrap();
                std::fs::set_permissions(&note, std::fs::Permissions::from_mode(0o444)).unwrap();
            }
            "no storage" => reader.update(visual, |reader, _| reader.session_directory = None),
            _ => {}
        }
        open_menu(&reader, visual);
        if problem == "missing vault" {
            std::fs::rename(&root, &moved).unwrap();
        }
        click_item(visual);
        if problem != "no storage" {
            visual
                .condition(&reader, |_, cx| completed(cx, &root, 1))
                .await;
        }
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("reader-error-toast").is_some(),
            "{problem} refusal is visible"
        );
        assert!(visual.debug_bounds("undo-reminder").is_none());
        match problem {
            "readonly" => {
                assert_eq!(std::fs::read_to_string(&note).unwrap(), "Existing prose\n");
                std::fs::set_permissions(&note, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            "missing vault" => {
                assert!(!root.exists());
                assert!(!moved.join(NOTE).exists());
                std::fs::rename(&moved, &root).unwrap();
            }
            _ => {
                assert!(!note.exists());
                reader.update(visual, |reader, _| {
                    reader.session_directory = Some(temp.path().join("state"))
                });
            }
        }
        open_menu(&reader, visual);
        click_item(visual);
        added(
            &reader,
            visual,
            &root,
            NOTE,
            if problem == "no storage" { 1 } else { 2 },
            1,
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(root.join("Source.md")).unwrap(),
            SOURCE
        );
    }
}

#[gpui::test]
async fn reminder_724_configured_path_and_revision_checked_undo_preserve_recovery(
    cx: &mut TestAppContext,
) {
    for interference in ["dirty draft", "external revision"] {
        let temp = tempfile::tempdir().unwrap();
        let (reader, visual) = mount(cx, &temp);
        let root = temp.path().join("vault");
        ready(&reader, visual).await;
        std::fs::create_dir(root.join("Inbox")).unwrap();
        reader.update(visual, |reader, cx| {
            let mut prefs = reader.reminder_prefs.clone();
            prefs.note = "Inbox/Reminders.md".into();
            reader.set_reminder_prefs(prefs, cx);
        });
        open_menu(&reader, visual);
        click_item(visual);
        added(&reader, visual, &root, "Inbox/Reminders.md", 1, 1).await;
        assert!(!root.join(NOTE).exists());
        assert_eq!(
            reader_reminder_settings::load(&temp.path().join("state"), &root).note,
            "Inbox/Reminders.md",
            "existing Settings persistence selects the central note"
        );
        let note = root.join("Inbox/Reminders.md");
        let drafts = temp.path().join("state/editor-drafts");
        let changed = format!("{LINE}Retained text\n");
        if interference == "dirty draft" {
            let mut editor = okilum_core::file_editor::FileEditor::open(&note, &drafts).unwrap();
            editor.set_text(changed.clone()).unwrap();
            drop(editor);
        } else {
            std::fs::write(&note, &changed).unwrap();
        }
        click_undo(visual);
        visual
            .condition(&reader, |_, cx| cx.global::<Harness>().undone == [false])
            .await;
        assert!(visual.debug_bounds("reader-error-toast").is_some());
        if interference == "dirty draft" {
            assert_eq!(std::fs::read_to_string(&note).unwrap(), LINE);
            let editor = okilum_core::file_editor::FileEditor::open(&note, &drafts).unwrap();
            assert!(editor.dirty());
            assert_eq!(editor.text(), changed);
        } else {
            assert_eq!(std::fs::read_to_string(&note).unwrap(), changed);
        }
        assert_eq!(
            std::fs::read_to_string(root.join("Source.md")).unwrap(),
            SOURCE
        );
    }
}
