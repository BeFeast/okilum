//! Focused offline acceptance for arbitrary Snooze. Product writes still use
//! the real calendar event, indexed occurrence, FileEditor and receipt Undo.
use super::*;
use crate::reader_task_actions::test_hooks::{Harness, Hooks};
use ::core::prelude::v1::test;
use gpui::{Global, TestAppContext, VisualTestContext};
use gpui_component::calendar::CalendarState;

#[derive(Default)]
struct Calendars(Vec<WeakEntity<CalendarState>>);
impl Global for Calendars {}
pub(super) fn record_calendar(cx: &mut App, calendar: &Entity<CalendarState>) {
    if cx.try_global::<Calendars>().is_none() {
        cx.set_global(Calendars::default());
    }
    cx.global_mut::<Calendars>().0.push(calendar.downgrade());
}
const DASHBOARD: &str = "---\nview: tasks\n---\n## Focus\n```tasks\nnot done\nsort by path\n```\n";
fn due() -> time::Date {
    today() + time::Duration::days(40)
}
fn snoozed() -> time::Date {
    today() + time::Duration::days(12)
}
fn source() -> String {
    let line = format!("- [ ] Same authored שלום task 📅 {} ^block\r\n", due());
    format!("\u{feff}# Work\r\n{line}{line}")
}
fn changed() -> String {
    let before = source();
    let first = before.find(" ^block").unwrap();
    let second = before[first + 1..].find(" ^block").unwrap() + first + 1;
    let mut after = before;
    after.insert_str(second, &format!(" ⏳ {}", snoozed()));
    after
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
    std::fs::write(root.join("Dashboard.md"), DASHBOARD).unwrap();
    std::fs::write(root.join("Work.md"), source()).unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Dashboard.md".into()),
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
fn completed(cx: &App, root: &Path, count: usize) -> bool {
    cx.global::<Harness>()
        .results
        .get(root)
        .is_some_and(|results| results.len() >= count)
}
fn open_snooze(reader: &Entity<Reader>, visual: &mut VisualTestContext) -> Entity<CalendarState> {
    visual.run_until_parked();
    let button = visual
        .debug_bounds("task-date-5-1")
        .expect("real second-occurrence date menu");
    visual.update(|_, cx| cx.set_global(Calendars::default()));
    visual.simulate_click(button.center(), Modifiers::default());
    visual.run_until_parked();
    visual.simulate_keystrokes("down down down down right");
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("task-snooze-calendar").is_some(),
        "positive control: actual Snooze calendar is painted in the menu"
    );
    let calendar = visual.update(|_, cx| {
        cx.global::<Calendars>()
            .0
            .last()
            .unwrap()
            .upgrade()
            .unwrap()
    });
    reader.read_with(visual, |reader, _| {
        assert_eq!(reader.current_rel, "Dashboard.md")
    });
    calendar
}
fn choose(calendar: &Entity<CalendarState>, date: time::Date, visual: &mut VisualTestContext) {
    // The same activation API invoked by the calendar's wired day item emits
    // the real Selected event to its retained production menu subscription.
    let date = chrono::NaiveDate::from_ymd_opt(date.year(), date.month() as u32, date.day() as u32)
        .unwrap();
    calendar.update(visual, |calendar, cx| {
        assert!(calendar.activate_date(date, cx))
    });
}
fn result_task(reader: &Reader) -> Option<Task> {
    reader
        .tasks_index
        .as_ref()?
        .note_tasks("Work.md")
        .into_iter()
        .find(|task| task.scheduled == Some(snoozed()))
}
fn click_undo(visual: &mut VisualTestContext) {
    // Existing notification animation uses wall time independently of GPUI's
    // executor clock. Match the existing dashboard Undo fixture's final bounds.
    std::thread::sleep(Duration::from_millis(450));
    visual.update(|window, _| window.refresh());
    visual.run_until_parked();
    let undo = visual
        .debug_bounds("task-undo")
        .expect("successful save exposes actual receipt Undo");
    visual.simulate_mouse_move(undo.center(), None, Modifiers::default());
    visual.simulate_click(undo.center(), Modifiers::default());
}

#[gpui::test]
async fn snooze_calendar_exact_occurrence_preserves_due_and_undo_then_reopen_filter(
    cx: &mut TestAppContext,
) {
    let temp = tempfile::tempdir().unwrap();
    let (reader, visual) = mount(cx, &temp);
    let root = temp.path().join("vault");
    visual
        .condition(&reader, |reader, _| {
            reader
                .tasks_index
                .as_ref()
                .is_some_and(|i| i.note_tasks("Work.md").len() == 2)
                && reader.current_rel == "Dashboard.md"
        })
        .await;
    reader.update_in(visual, |reader, _, _| {
        reader.watcher.take();
    });
    let calendar = open_snooze(&reader, visual);
    choose(&calendar, snoozed(), visual);
    visual
        .condition(&reader, |reader, cx| {
            completed(cx, &root, 1) && result_task(reader).is_some()
        })
        .await;
    assert_eq!(
        std::fs::read_to_string(root.join("Work.md")).unwrap(),
        changed()
    );
    reader.read_with(visual, |reader, _| {
        let task = result_task(reader).unwrap();
        assert_eq!(task.line, 3);
        assert_eq!(task.due, Some(due()));
        assert!(
            reader.tasks_index.as_ref().unwrap().note_tasks("Work.md")[0]
                .scheduled
                .is_none()
        );
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("task-scheduled-5-1").is_some());
    let upcoming = visual.debug_bounds("dashboard-filter-3").unwrap();
    visual.simulate_click(upcoming.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("task-scheduled-5-1").is_some(),
        "existing Upcoming remains due-based and keeps the scheduled badge"
    );
    click_undo(visual);
    visual
        .condition(&reader, |reader, _| {
            reader.tasks_index.as_ref().is_some_and(|i| {
                i.note_tasks("Work.md")
                    .iter()
                    .all(|task| task.scheduled.is_none())
            })
        })
        .await;
    assert_eq!(
        std::fs::read_to_string(root.join("Work.md")).unwrap(),
        source()
    );
    let calendar = open_snooze(&reader, visual);
    choose(&calendar, snoozed(), visual);
    visual
        .condition(&reader, |reader, cx| {
            completed(cx, &root, 2) && result_task(reader).is_some()
        })
        .await;
    reader.update_in(visual, |reader, window, cx| {
        reader.open_note("Work.md", None, window, cx)
    });
    visual.run_until_parked();
    reader.update_in(visual, |reader, window, cx| {
        reader.open_note("Dashboard.md", None, window, cx)
    });
    visual
        .condition(&reader, |reader, _| {
            reader.current_rel == "Dashboard.md" && result_task(reader).is_some()
        })
        .await;
    visual.run_until_parked();
    assert!(visual.debug_bounds("task-scheduled-5-1").is_some());
    for filter in ["dashboard-filter-0", "dashboard-filter-3"] {
        let chip = visual.debug_bounds(filter).unwrap();
        visual.simulate_click(chip.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("task-scheduled-5-1").is_some());
    }
    assert_eq!(
        std::fs::read_to_string(root.join("Work.md")).unwrap(),
        changed()
    );
}

#[gpui::test]
async fn snooze_calendar_stale_menu_navigation_root_and_index_refuse_without_retarget(
    cx: &mut TestAppContext,
) {
    for scope in ["navigation", "root", "index"] {
        let temp = tempfile::tempdir().unwrap();
        let (reader, visual) = mount(cx, &temp);
        let root = temp.path().join("vault");
        visual
            .condition(&reader, |reader, _| {
                reader
                    .tasks_index
                    .as_ref()
                    .is_some_and(|index| index.note_tasks("Work.md").len() == 2)
                    && reader.current_rel == "Dashboard.md"
            })
            .await;
        reader.update_in(visual, |reader, _, _| {
            reader.watcher.take();
        });
        let calendar = open_snooze(&reader, visual);
        let old = reader.read_with(visual, |reader, _| {
            (
                reader.vault_root.clone(),
                reader.tasks_index.clone().unwrap(),
            )
        });
        reader.update_in(visual, |reader, _, cx| {
            match scope {
                "navigation" => reader.navigation.generation += 1,
                "root" => reader.vault_root = temp.path().join("other-vault"),
                _ => reader.tasks_index = Some(Arc::new((*old.1).clone())),
            }
            cx.notify();
        });
        choose(&calendar, snoozed(), visual);
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(root.join("Work.md")).unwrap(),
            source()
        );
        assert!(visual.debug_bounds("reader-error-toast").is_some());
        reader.update_in(visual, |reader, _, cx| {
            reader.vault_root = old.0;
            reader.tasks_index = Some(old.1);
            reader.navigation.generation += 1;
            cx.notify();
        });
        visual.simulate_keystrokes("escape escape");
        visual.run_until_parked();
        let fresh = open_snooze(&reader, visual);
        choose(&fresh, snoozed(), visual);
        visual
            .condition(&reader, |reader, cx| {
                completed(cx, &root, 1) && result_task(reader).is_some()
            })
            .await;
        assert_eq!(
            std::fs::read_to_string(root.join("Work.md")).unwrap(),
            changed(),
            "positive control: fresh menu writes only its exact occurrence"
        );
    }
}

#[gpui::test]
async fn snooze_calendar_storage_failure_keeps_canonical_source_and_fresh_retry(
    cx: &mut TestAppContext,
) {
    let temp = tempfile::tempdir().unwrap();
    let (reader, visual) = mount(cx, &temp);
    let root = temp.path().join("vault");
    visual
        .condition(&reader, |reader, _| {
            reader
                .tasks_index
                .as_ref()
                .is_some_and(|index| index.note_tasks("Work.md").len() == 2)
                && reader.current_rel == "Dashboard.md"
        })
        .await;
    reader.update_in(visual, |reader, _, _| {
        reader.watcher.take();
    });
    let poison = temp.path().join("unusable-state");
    std::fs::write(&poison, b"blocked recovery storage").unwrap();
    reader.update_in(visual, |reader, _, _| {
        reader.session_directory = Some(poison.clone())
    });
    let calendar = open_snooze(&reader, visual);
    choose(&calendar, snoozed(), visual);
    visual
        .condition(&reader, |_, cx| completed(cx, &root, 1))
        .await;
    visual.run_until_parked();
    assert!(visual.debug_bounds("reader-error-toast").is_some());
    assert!(visual.debug_bounds("task-undo").is_none());
    assert_eq!(
        std::fs::read_to_string(root.join("Work.md")).unwrap(),
        source()
    );
    reader.update_in(visual, |reader, _, cx| {
        reader.session_directory = Some(temp.path().join("state"));
        cx.notify();
    });
    let fresh = open_snooze(&reader, visual);
    choose(&fresh, snoozed(), visual);
    visual
        .condition(&reader, |reader, cx| {
            completed(cx, &root, 2) && result_task(reader).is_some()
        })
        .await;
    assert_eq!(
        std::fs::read_to_string(root.join("Work.md")).unwrap(),
        changed()
    );
}

#[gpui::test]
async fn snooze_calendar_held_worker_and_completion_reject_rebound_reader(cx: &mut TestAppContext) {
    for phase in ["before", "after"] {
        let temp = tempfile::tempdir().unwrap();
        let (reader, visual) = mount(cx, &temp);
        let root = temp.path().join("vault");
        visual
            .condition(&reader, |reader, _| {
                reader
                    .tasks_index
                    .as_ref()
                    .is_some_and(|index| index.note_tasks("Work.md").len() == 2)
                    && reader.current_rel == "Dashboard.md"
            })
            .await;
        reader.update_in(visual, |reader, _, _| {
            reader.watcher.take();
        });
        let (release, held) = async_channel::bounded(1);
        let (entered, actual_boundary) = async_channel::bounded(1);
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
        let calendar = open_snooze(&reader, visual);
        choose(&calendar, snoozed(), visual);
        visual.run_until_parked();
        // After-save hold has real canonical positive control; before-save hold
        // has the opposite positive control and is released only after revoke.
        actual_boundary
            .recv()
            .await
            .expect("real worker reached the held boundary");
        if phase == "after" {
            assert_eq!(
                std::fs::read_to_string(root.join("Work.md")).unwrap(),
                changed()
            );
        } else {
            assert_eq!(
                std::fs::read_to_string(root.join("Work.md")).unwrap(),
                source()
            );
        }
        reader.update_in(visual, |reader, _, cx| {
            reader.vault_root = temp.path().join("replacement");
            reader.navigation.generation += 1;
            reader.tasks_index = Some(Arc::default());
            cx.notify();
        });
        visual.run_until_parked();
        release.try_send(()).unwrap();
        visual
            .condition(&reader, |_, cx| completed(cx, &root, 1))
            .await;
        reader.read_with(visual, |reader, cx| {
            assert!(reader
                .tasks_index
                .as_ref()
                .unwrap()
                .note_tasks("Work.md")
                .is_empty());
            assert!(
                !cx.global::<Harness>().results[&root][0].1,
                "stale completion never publishes to rebound Reader"
            );
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("task-undo").is_none());
        assert_eq!(
            std::fs::read_to_string(root.join("Work.md")).unwrap(),
            if phase == "before" {
                source()
            } else {
                changed()
            }
        );
        reader.update_in(visual, |reader, window, cx| {
            reader.vault_root = root.clone();
            reader.navigation.generation += 1;
            reader.start_incremental(
                okilum_core::Changes {
                    changed: ["Work.md".into()].into_iter().collect(),
                    ..Default::default()
                },
                window,
                cx,
            );
        });
        visual
            .condition(&reader, |reader, _| {
                reader
                    .tasks_index
                    .as_ref()
                    .is_some_and(|index| index.note_tasks("Work.md").len() == 2)
                    && !reader.incremental_active
            })
            .await;
        visual.simulate_keystrokes("escape escape");
        visual.run_until_parked();
        let fresh = open_snooze(&reader, visual);
        let next = snoozed() + time::Duration::days(3);
        choose(&fresh, next, visual);
        visual
            .condition(&reader, |reader, cx| {
                completed(cx, &root, 2)
                    && reader
                        .tasks_index
                        .as_ref()
                        .unwrap()
                        .note_tasks("Work.md")
                        .iter()
                        .any(|task| task.scheduled == Some(next))
            })
            .await;
        reader.read_with(visual, |_, cx| {
            assert_eq!(
                cx.global::<Harness>().results[&root][1],
                (true, true),
                "positive control: fresh completion applies to its current Reader"
            )
        });
    }
}

#[gpui::test]
async fn snooze_calendar_undo_refuses_intervening_source_and_preserves_recovery(
    cx: &mut TestAppContext,
) {
    let temp = tempfile::tempdir().unwrap();
    let (reader, visual) = mount(cx, &temp);
    let root = temp.path().join("vault");
    visual
        .condition(&reader, |reader, _| {
            reader
                .tasks_index
                .as_ref()
                .is_some_and(|index| index.note_tasks("Work.md").len() == 2)
                && reader.current_rel == "Dashboard.md"
        })
        .await;
    reader.update_in(visual, |reader, _, _| {
        reader.watcher.take();
    });
    let calendar = open_snooze(&reader, visual);
    choose(&calendar, snoozed(), visual);
    visual
        .condition(&reader, |reader, cx| {
            completed(cx, &root, 1) && result_task(reader).is_some()
        })
        .await;
    let mut editor = okilum_core::file_editor::FileEditor::open(
        &root.join("Work.md"),
        &temp.path().join("state/editor-drafts"),
    )
    .unwrap();
    let unsaved = format!("{}Retained unsaved draft\r\n", changed());
    editor.set_text(unsaved.clone()).unwrap();
    drop(editor);
    click_undo(visual);
    visual.run_until_parked();
    visual
        .condition(&reader, |_, cx| {
            cx.global::<Harness>()
                .undo_results
                .get(&root)
                .is_some_and(|results| results == &[false])
        })
        .await;
    assert!(
        visual.debug_bounds("reader-error-toast").is_some(),
        "actual revision/dirty-draft refusal is visible"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("Work.md")).unwrap(),
        changed()
    );
    let mut recovered = okilum_core::file_editor::FileEditor::open(
        &root.join("Work.md"),
        &temp.path().join("state/editor-drafts"),
    )
    .unwrap();
    assert_eq!(recovered.text(), unsaved);
    assert!(recovered.dirty());
    // Only the fixture's explicit Reload discards its own recovery edit, then
    // an independent canonical change exercises the receipt's revision guard.
    recovered.reload().unwrap();
    drop(recovered);
    let external = format!("{}Independent canonical prose\r\n", changed());
    std::fs::write(root.join("Work.md"), &external).unwrap();
    click_undo(visual);
    visual
        .condition(&reader, |_, cx| {
            cx.global::<Harness>()
                .undo_results
                .get(&root)
                .is_some_and(|results| results == &[false, false])
        })
        .await;
    assert_eq!(
        std::fs::read_to_string(root.join("Work.md")).unwrap(),
        external
    );
}
