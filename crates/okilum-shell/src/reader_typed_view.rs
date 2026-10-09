//! Native view selection uses the accepted canonical snapshot, never UI-thread IO.
use super::*;
use gpui_component::button::ButtonGroup;
use okilum_core::typed_view::{self, layout, Preferences, Selection};

#[derive(Default)]
pub(super) struct Navigation {
    pub active: std::cell::Cell<bool>,
    pub scroll: gpui::ScrollHandle,
    sections: std::cell::RefCell<Vec<(usize, String)>>,
}

/// Resolve only headings actually represented by the native surface. Duplicate
/// labels remain ambiguous rather than landing on an arbitrary query.
fn section_target(sections: &[(usize, String)], title: &str) -> Option<usize> {
    let mut matches = sections
        .iter()
        .enumerate()
        .filter(|(_, (_, label))| label == title);
    let (index, _) = matches.next()?;
    matches.next().is_none().then_some(index + 1)
}

pub(super) fn land(reader: &Reader, block: usize) -> Result<(), &'static str> {
    let heading = reader
        .outline
        .iter()
        .find(|heading| heading.target.block == block)
        .ok_or("This position is not represented in the Tasks view. Open Source to inspect it.")?;
    let index = section_target(&reader.typed_navigation.sections.borrow(), &heading.text)
        .ok_or("This heading has no unique Tasks section. Open Source to inspect it.")?;
    reader.typed_navigation.scroll.scroll_to_top_of_item(index);
    Ok(())
}

pub(super) fn outline(reader: &Reader, cx: &mut Context<Reader>) -> Option<AnyElement> {
    if !reader.typed_navigation.active.get() {
        return None;
    }
    Some(
        v_flex()
            .children(
                reader
                    .typed_navigation
                    .sections
                    .borrow()
                    .iter()
                    .map(|(line, title)| {
                        let line = *line;
                        div()
                            .id(("dashboard-outline", line))
                            .debug_selector(move || format!("dashboard-outline-{line}"))
                            .px_4()
                            .py_1()
                            .text_sm()
                            .cursor_pointer()
                            .child(title.clone())
                            .on_click(cx.listener(move |reader, _, window, cx| {
                                if let Some(index) = reader
                                    .typed_navigation
                                    .sections
                                    .borrow()
                                    .iter()
                                    .position(|(candidate, _)| *candidate == line)
                                {
                                    reader
                                        .typed_navigation
                                        .scroll
                                        .scroll_to_top_of_item(index + 1);
                                }
                                reader.focus_handle.focus(window, cx);
                                cx.notify();
                            }))
                    }),
            )
            .into_any_element(),
    )
}

#[derive(Clone)]
enum View {
    Markdown,
    Tasks(Arc<layout::Dashboard>),
    Fallback(String),
}

struct Cached {
    source: Arc<str>,
    preferences: Preferences,
    today: time::Date,
    view: View,
    filter: usize,
    grouping: Option<layout::Grouping>,
}
impl Cached {
    fn new(source: Arc<str>, preferences: Preferences, today: time::Date) -> Self {
        let view = match typed_view::select(&source, &preferences.mappings) {
            Selection::Markdown => View::Markdown,
            Selection::Fallback(reason) => View::Fallback(reason),
            Selection::Native(_) => match layout::parse(&source, today, preferences.tasks) {
                Ok(dashboard) => View::Tasks(Arc::new(dashboard)),
                Err(reason) => View::Fallback(reason),
            },
        };
        Self {
            source,
            preferences,
            today,
            view,
            filter: 0,
            grouping: None,
        }
    }
    fn refresh(&mut self, source: Arc<str>, preferences: Preferences, today: time::Date) -> bool {
        if self.source == source && self.preferences == preferences && self.today == today {
            return false;
        }
        *self = Self::new(source, preferences, today);
        true
    }
}

pub(super) fn render(
    reader: &Reader,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> Option<AnyElement> {
    // The reminders note can be shown through this same view (#919): its
    // generated Tasks source replaces the note text, opened on Overdue.
    let lens = reader.reminders_lens();
    let on_overdue = lens.is_some();
    let source = match lens {
        Some(source) => source,
        None => reader.note_canonical_source.clone()?,
    };
    let preferences = reader_ui_state::typed_views(cx);
    let today = reader_tasks::today();
    let cache = window.use_keyed_state(
        SharedString::from(format!(
            "typed-view:{}",
            reader.vault_root.join(reader.selected_file()).display()
        )),
        cx,
        |_, _| Cached::new(Arc::from(""), Preferences::default(), today),
    );
    let (view, changed) = cache.update(cx, |cached, _| {
        let changed = cached.refresh(source, preferences, today);
        (cached.view.clone(), changed)
    });
    if changed && on_overdue {
        cache.update(cx, |cached, _| cached.filter = 2);
    }
    let dashboard = match view {
        View::Markdown => return None,
        View::Fallback(reason) => {
            if changed {
                reader_toast::transient(reason, window, cx);
            }
            return None;
        }
        View::Tasks(dashboard) => dashboard,
    };
    // Navigation transfers Markdown focus to the replacement TextView. This
    // surface hides that view, so keep its keyboard dispatch on the visible
    // Reader instead. Find/sidebar focus must remain untouched.
    if reader.content.read(cx).focus_handle().is_focused(window) {
        reader.focus_handle.focus(window, cx);
    }
    reader.typed_navigation.active.set(true);
    *reader.typed_navigation.sections.borrow_mut() = dashboard
        .layout
        .order
        .iter()
        .map(|&i| {
            let s = &dashboard.sections[i];
            (
                s.source_line,
                s.title.clone().unwrap_or_else(|| "Tasks".into()),
            )
        })
        .collect();
    let search = if reader.find_open {
        reader.find_input.read(cx).value().to_string()
    } else {
        String::new()
    };
    let weak = cx.entity().downgrade();
    let filter = cache.read(cx).filter;
    let grouping = cache.read(cx).grouping.unwrap_or(dashboard.layout.grouping);
    let layout = layout::Defaults {
        density: dashboard.layout.density,
        grouping,
    };
    let filters = ButtonGroup::new("dashboard-filter").children(
        [(0, "All"), (1, "Open"), (2, "Overdue")].map(|(value, label)| {
            let cache = cache.clone();
            let reader = weak.clone();
            Button::new(("dashboard-filter", value))
                .small()
                .ghost()
                .label(label)
                .selected(filter == value)
                .on_click(move |_, _, cx| {
                    cache.update(cx, |s, _| s.filter = value);
                    let _ = reader.update(cx, |_, cx| cx.notify());
                })
        }),
    );
    let groups = ButtonGroup::new("dashboard-grouping").children(
        [
            (layout::Grouping::Query, "Query"),
            (layout::Grouping::Note, "Note"),
            (layout::Grouping::None, "None"),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (value, label))| {
            let cache = cache.clone();
            let reader = weak.clone();
            Button::new(("dashboard-group", i))
                .small()
                .ghost()
                .label(label)
                .tooltip(format!("Group by {}", label.to_lowercase()))
                .selected(grouping == value)
                .on_click(move |_, _, cx| {
                    cache.update(cx, |s, _| s.grouping = Some(value));
                    let _ = reader.update(cx, |_, cx| cx.notify());
                })
        }),
    );
    Some(
        div()
            .id("native-tasks-dashboard")
            .debug_selector(|| "native-tasks-dashboard".into())
            .size_full()
            .relative()
            .track_focus(&reader.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|reader, _, window, cx| reader.focus_handle.focus(window, cx)),
            )
            .track_scroll(&reader.typed_navigation.scroll)
            .overflow_y_scroll()
            .px_6()
            .pt_4()
            .pb_8()
            .flex()
            .flex_col()
            .gap_6()
            .child(h_flex().gap_3().flex_wrap().child(filters).child(groups))
            .children(dashboard.layout.order.iter().map(|&index| {
                let section = &dashboard.sections[index];
                let line = section.source_line;
                div().id(("dashboard-section", line)).flex_none().child(
                    reader_tasks::dashboard_section(
                        section.title.clone(),
                        match filter {
                            1 => format!("{}\nnot done", section.query_source),
                            2 => {
                                format!("{}\nnot done\ndue before today", section.query_source)
                            }
                            _ => section.query_source.clone(),
                        },
                        section.source_line,
                        layout,
                        search.clone(),
                        reader_ui_state::find_case_sensitive(cx),
                        weak.clone(),
                    ),
                )
            }))
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn accepted_frontmatter_and_preferences_select_views_without_stale_cache() {
        let day = time::macros::date!(2026 - 10 - 08);
        let source: Arc<str> = Arc::from(
            "---\ntype: dashboard\nview: tasks\n---\n## Today\n```tasks\nnot done\n```\n",
        );
        let mut cache = Cached::new(source.clone(), Preferences::default(), day);
        assert!(matches!(cache.view, View::Tasks(_)));
        assert!(!cache.refresh(source.clone(), Preferences::default(), day));
        assert!(cache.refresh(
            source.clone(),
            Preferences::default(),
            day.next_day().unwrap()
        ));
        assert!(cache.refresh(
            Arc::from(source.replace("view: tasks", "view: unavailable")),
            Preferences::default(),
            day
        ));
        assert!(matches!(cache.view, View::Fallback(_)));
        let mapped: Arc<str> = Arc::from(source.replace("view: tasks\n", ""));
        cache.refresh(mapped.clone(), Preferences::default(), day);
        assert!(matches!(cache.view, View::Markdown));
        let mut preferences = Preferences::default();
        preferences
            .mappings
            .insert("dashboard".into(), "tasks".into());
        cache.refresh(mapped, preferences, day);
        assert!(matches!(cache.view, View::Tasks(_)));
        cache.refresh(
            Arc::from(source.replace("not done", "unsupported query")),
            Preferences::default(),
            day,
        );
        assert!(matches!(cache.view, View::Fallback(_)));
    }
    #[test]
    fn heading_landing_uses_display_order_and_rejects_missing_or_duplicate_titles() {
        let sections = vec![(20, "Later".into()), (4, "Focus".into())];
        assert_eq!(section_target(&sections, "Focus"), Some(2));
        assert_eq!(section_target(&sections, "Later"), Some(1));
        assert_eq!(section_target(&sections, "Missing"), None);
        let duplicate = vec![(4, "Focus".into()), (20, "Focus".into())];
        assert_eq!(section_target(&duplicate, "Focus"), None);
    }

    #[gpui::test]
    fn heading_navigation_keeps_tasks_keyboard_focus(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let fixture = tempfile::tempdir().unwrap();
        let vault = fixture.path().join("vault");
        std::fs::create_dir(&vault).unwrap();
        std::fs::write(vault.join("Start.md"), "# Start\n\n[[Dashboard#Focus]]\n").unwrap();
        std::fs::write(
            vault.join("Dashboard.md"),
            "---\ntype: dashboard\nview: tasks\n---\n## Focus\n```tasks\nnot done\n```\n",
        )
        .unwrap();
        std::fs::write(
            vault.join("Work.md"),
            "- [ ] Ship dashboard\n- [ ] Review checklist\n",
        )
        .unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("Start.md".into()),
                        index_dir: Some(fixture.path().join("index")),
                        session_directory: Some(fixture.path().join("state")),
                        panel_settings_override: Some(fixture.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.content.read(cx).focus_handle().clone().focus(window, cx)
        });
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-f"
        } else {
            "ctrl-f"
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(r.find_open, "ordinary note positive control")
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        // A rendered-link click focuses the Markdown TextView before dispatch.
        reader.update_in(visual, |r, window, cx| {
            r.content.read(cx).focus_handle().clone().focus(window, cx);
        });
        let url = format!("{WIKI_SCHEME}Dashboard.md#Focus");
        visual.update(|window, cx| handle_link(&reader.downgrade(), &url, window, cx));
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(100));
        visual.run_until_parked();
        assert!(visual.debug_bounds("native-tasks-dashboard").is_some());
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "Dashboard.md");
            assert!(r.navigation.pending_landing.is_none());
        });
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-f"
        } else {
            "ctrl-f"
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(r.find_open, "Find after heading navigation")
        });
        visual.simulate_input("Review");
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("tasks-count-1").is_some(),
            "Find remains focused and filters native tasks"
        );
        visual.simulate_keystrokes("escape escape");
        visual.run_until_parked();
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-k"
        } else {
            "ctrl-k"
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(r.quick_open.open, "other reader shortcuts remain available")
        });
    }

    #[gpui::test]
    fn native_dashboard_keeps_source_escape_and_markdown_fallback(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let fixture = tempfile::tempdir().unwrap();
        let vault = fixture.path().join("vault");
        std::fs::create_dir(&vault).unwrap();
        let source = "---\nview: tasks\n---\n## Focus\n```tasks\nnot done\n```\n";
        std::fs::write(vault.join("Dashboard.md"), source).unwrap();
        std::fs::write(vault.join("Work.md"), "- [ ] Ship dashboard\n").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("Dashboard.md".into()),
                        index_dir: Some(fixture.path().join("cache")),
                        session_directory: Some(fixture.path().join("state")),
                        panel_settings_override: Some(fixture.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = entity.unwrap();
        visual.simulate_resize(size(px(1400.), px(960.)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("native-tasks-dashboard").is_some());
        #[cfg(unix)]
        {
            let (index, task) = view.read_with(visual, |v, _| {
                let index = v.tasks_index.clone().unwrap();
                let task = index
                    .query(&okilum_core::tasks::Query::parse(
                        "not done",
                        reader_tasks::today(),
                    ))
                    .remove(0);
                (index, task)
            });
            view.update_in(visual, |v, window, cx| {
                v.apply_task_change(
                    index.clone(),
                    task.clone(),
                    okilum_core::task_edit::Change::Checked(true),
                    window,
                    cx,
                )
            });
            visual.run_until_parked();
            assert_eq!(
                std::fs::read_to_string(vault.join("Work.md")).unwrap(),
                "- [x] Ship dashboard\n"
            );
            // GPUI's one-shot paint animation uses wall time, unlike the test
            // executor's notification lifetime clock. Click only its final bounds.
            std::thread::sleep(Duration::from_millis(450));
            visual.update(|window, _| window.refresh());
            visual.executor().advance_clock(Duration::from_millis(400));
            visual.run_until_parked();
            let undo = visual
                .debug_bounds("task-undo")
                .expect("visible Undo after actual save");
            visual.update(|window, _| {
                assert!(
                    Bounds::new(point(px(0.), px(0.)), window.viewport_size())
                        .contains(&undo.center()),
                    "Undo outside viewport: {undo:?}"
                )
            });
            visual.simulate_mouse_move(undo.center(), None, Modifiers::default());
            visual.simulate_click(undo.center(), Modifiers::default());
            visual.run_until_parked();
            assert_eq!(
                std::fs::read_to_string(vault.join("Work.md")).unwrap(),
                "- [ ] Ship dashboard\n"
            );
            view.update_in(visual, |v, window, cx| {
                v.apply_task_change(
                    index.clone(),
                    task.clone(),
                    okilum_core::task_edit::Change::Due(time::macros::date!(2027 - 02 - 17)),
                    window,
                    cx,
                );
            });
            visual.run_until_parked();
            assert_eq!(
                std::fs::read_to_string(vault.join("Work.md")).unwrap(),
                "- [ ] Ship dashboard 📅 2027-02-17\n"
            );
            std::fs::write(vault.join("Work.md"), "New prose\n\n- [ ] Ship dashboard\n").unwrap();
            view.update_in(visual, |v, window, cx| {
                v.apply_task_change(
                    index,
                    task,
                    okilum_core::task_edit::Change::Checked(true),
                    window,
                    cx,
                )
            });
            visual.run_until_parked();
            assert_eq!(
                std::fs::read_to_string(vault.join("Work.md")).unwrap(),
                "New prose\n\n- [ ] Ship dashboard\n"
            );
        }
        assert!(
            visual.debug_bounds("reader-edit").is_some(),
            "Source escape is visible"
        );
        view.update(visual, |v, cx| {
            let mut index = okilum_core::tasks::Index::default();
            index.replace(
                "Many.md",
                &(0..60)
                    .map(|i| format!("- [ ] Ship {i:03}\n"))
                    .collect::<String>(),
            );
            v.tasks_index = Some(Arc::new(index));
            cx.notify();
        });
        visual.simulate_resize(size(px(1400.), px(400.)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("tasks-count-60").is_some());
        view.update_in(visual, |v, window, cx| {
            v.focus_handle.focus(window, cx);
            v.scroll_reader_key(1., true, window, cx);
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert!(v.typed_navigation.scroll.offset().y < px(0.))
        });
        let scrolled = view.read_with(visual, |v, _| v.typed_navigation.scroll.offset().y);
        view.update(visual, |v, cx| {
            let block = v
                .outline
                .iter()
                .find(|heading| heading.text == "Focus")
                .unwrap()
                .target
                .block;
            v.scroll_to_block(block, cx);
        });
        visual.executor().advance_clock(Duration::from_millis(100));
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert!(v.navigation.pending_landing.is_none());
            assert!(
                v.typed_navigation.scroll.offset().y > scrolled,
                "heading link moves the visible surface"
            );
        });
        view.update_in(visual, |v, window, cx| {
            v.open_find(window, cx);
            v.find_input
                .update(cx, |input, cx| input.set_value("Ship 059", window, cx));
            v.run_find(window, cx);
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("tasks-count-1").is_some(),
            "Find filters native results"
        );
        view.update_in(visual, |v, window, cx| v.close_find(window, cx));
        visual.simulate_resize(size(px(1400.), px(960.)));
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| v.toggle_source(window, cx));
        visual.run_until_parked();
        view.read_with(visual, |v, _| assert!(v.editing.is_some()));
        assert!(visual.debug_bounds("native-tasks-dashboard").is_none());
        view.update_in(visual, |v, window, cx| v.toggle_source(window, cx));
        visual.run_until_parked();
        assert!(visual.debug_bounds("native-tasks-dashboard").is_some());
        view.update(visual, |v, cx| {
            v.note_canonical_source =
                Some(Arc::from(source.replace("view: tasks", "view: unknown")));
            cx.notify();
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("native-tasks-dashboard").is_none());
        assert!(visual.debug_bounds("reader-edit").is_some());
    }
}
