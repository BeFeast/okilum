//! Native, read-only Tasks blocks. Index ownership stays with Reader snapshots.
use super::*;
use gpui_component::{checkbox::Checkbox, Disableable};
use tessera_core::tasks::{Group, Index, Query, Task};

pub(super) fn from_snapshot(snapshot: &tessera_core::vault::warm::Snapshot) -> Arc<Index> {
    let mut index = Index::default();
    for path in snapshot.source_paths() {
        if let Some(source) = snapshot.source(path) {
            index.replace(path, &source);
        }
    }
    Arc::new(index)
}

#[derive(Clone)]
struct TasksBlock {
    query: String,
    offset: usize,
}

pub(super) fn plugins(view: TextView, reader: WeakEntity<Reader>) -> TextView {
    view.markdown_block_parser(|node, cx| {
        let markdown_ast::Node::Code(code) = node else {
            return None;
        };
        if code.lang.as_deref() != Some("tasks") {
            return None;
        }
        Some(
            MarkdownNode::new(
                "tasks-query",
                TasksBlock {
                    query: code.value.clone(),
                    offset: node.position().map_or(0, |p| p.start.offset) + cx.offset(),
                },
            )
            .text(cx.node_source(node).unwrap_or("").to_owned())
            .markdown(cx.node_source(node).unwrap_or("").to_owned()),
        )
    })
    .markdown_block_renderer("tasks-query", move |node, _, _cx| {
        let Some(block) = node.data::<TasksBlock>() else {
            return div().into_any_element();
        };
        div()
            .id(("tasks-block", block.offset))
            .child(TasksList {
                query: block.query.clone(),
                offset: block.offset,
                reader: reader.clone(),
            })
            .into_any_element()
    })
}

#[derive(IntoElement)]
struct TasksList {
    query: String,
    offset: usize,
    reader: WeakEntity<Reader>,
}
struct Results {
    index: Arc<Index>,
    today: time::Date,
    query: String,
    tasks: Vec<Task>,
    rows: Vec<Vec<usize>>,
    unsupported: Vec<String>,
    groups: Vec<Group>,
    explicit_groups: bool,
    expanded: std::collections::BTreeSet<(String, usize)>,
    shown: usize,
}
impl Results {
    fn refresh(&mut self, index: Arc<Index>, now: time::Date, source: &str) {
        if !Arc::ptr_eq(&self.index, &index) || self.today != now || self.query != source {
            let query = Query::parse(source, now);
            self.tasks = index.query(&query);

            self.explicit_groups = !query.groups.is_empty();
            self.groups = if query.groups.is_empty() {
                vec![Group::Filename]
            } else {
                query.groups
            };
            // Stable grouping preserves the query ordering inside each group.
            self.tasks.sort_by_key(|task| group_key(task, &self.groups));
            self.rows = carried_rows(&self.tasks, &self.groups, self.explicit_groups);
            self.unsupported = query.unsupported;
            self.index = index;
            self.today = now;
            if self.query != source {
                self.shown = 20;
                self.expanded.clear();
            }
            self.query = source.to_owned();
        }
    }
}
// Include source identity in filename groups: equal stems never merge unrelated notes.
fn group_key(task: &Task, groups: &[Group]) -> Vec<String> {
    groups
        .iter()
        .map(|g| match g {
            Group::Filename => format!("{}\0{}", g.label(task), task.path),
            Group::Priority => task.priority.to_string(),
            _ => g.label(task),
        })
        .collect()
}
// Collapse exact carried copies, never different schedules/statuses or two tasks
// in the same note. Expansion retains every source action and its own identity.
fn carried_rows(tasks: &[Task], groups: &[Group], explicit: bool) -> Vec<Vec<usize>> {
    let mut keys = std::collections::BTreeMap::new();
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for (ix, task) in tasks.iter().enumerate() {
        let key = (
            task.text.split_whitespace().collect::<Vec<_>>().join(" "),
            task.checked,
            task.due,
            task.scheduled,
            task.start,
            task.done,
            task.priority,
            if explicit {
                group_key(task, groups)
            } else {
                Vec::new()
            },
        );
        let candidates: &mut Vec<usize> = keys.entry(key).or_default();
        if let Some(row) = candidates
            .iter()
            .copied()
            .find(|&r| rows[r].iter().all(|&t| tasks[t].path != task.path))
        {
            rows[row].push(ix);
        } else {
            candidates.push(rows.len());
            rows.push(vec![ix]);
        }
    }
    rows
}
fn task_label(task: &Task) -> String {
    let mut text = task
        .display
        .clone()
        .unwrap_or_else(|| tessera_core::render::strip_inline_markdown(&task.text));
    for (marker, date) in [
        ('📅', task.due),
        ('⏳', task.scheduled),
        ('🛫', task.start),
        ('✅', task.done),
    ] {
        if let Some(date) = date {
            text = text.replace(&format!("{marker} {date}"), "");
        }
    }
    for marker in ['🔺', '⏫', '🔼', '🔽', '⏬'] {
        text = text.replace(marker, "");
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn today() -> time::Date {
    use chrono::Datelike;
    let now = chrono::Local::now();
    time::Date::from_calendar_date(
        now.year(),
        (now.month() as u8).try_into().unwrap(),
        now.day() as u8,
    )
    .unwrap()
}
#[cfg(not(test))]
fn until_midnight() -> std::time::Duration {
    let now = chrono::Local::now();
    now.date_naive()
        .succ_opt()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .and_then(|d| d.and_local_timezone(chrono::Local).earliest())
        .and_then(|d| (d - now).to_std().ok())
        .unwrap_or(std::time::Duration::from_secs(3600))
        + std::time::Duration::from_secs(1)
}
impl RenderOnce for TasksList {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(reader) = self.reader.upgrade() else {
            return div().into_any_element();
        };
        let Some(index) = reader.read(cx).tasks_index.clone() else {
            let loading = reader.read(cx).incremental_initializing
                || reader
                    .read(cx)
                    .loading
                    .as_ref()
                    .is_some_and(|load| load.active);
            return div()
                .text_sm()
                .debug_selector(move || {
                    if loading {
                        "tasks-indexing"
                    } else {
                        "tasks-unavailable"
                    }
                    .into()
                })
                .child(if loading {
                    "Indexing tasks…"
                } else {
                    "Tasks unavailable: vault indexing did not complete. Reopen the vault to retry."
                })
                .into_any_element();
        };
        let offset = self.offset;
        let now = today();
        let state = window.use_keyed_state("tasks-results", cx, |_, _cx| {
            #[cfg(not(test))]
            _cx.spawn(async move |state, cx| loop {
                cx.background_executor().timer(until_midnight()).await;
                if state.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            })
            .detach();
            Results {
                index: Arc::default(),
                today: now,
                query: String::new(),
                tasks: Vec::new(),
                rows: Vec::new(),
                unsupported: Vec::new(),
                groups: vec![Group::Filename],
                explicit_groups: false,
                expanded: Default::default(),
                shown: 20,
            }
        });
        state.update(cx, |s, _| s.refresh(index, now, &self.query));
        let results = state.read(cx);
        let muted = cx.theme().muted_foreground;
        let mut list = v_flex().gap_0().py_1().w_full();
        if !results.unsupported.is_empty() {
            for line in &results.unsupported {
                list = list.child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .debug_selector(|| "tasks-unsupported".into())
                        .child(format!("Unsupported: {line}")),
                );
            }
            return list.into_any_element();
        }
        list = list.child(
            div()
                .text_sm()
                .text_color(muted)
                .debug_selector({
                    let count = results.rows.len();
                    move || format!("tasks-count-{count}")
                })
                .child(format!("{} tasks", results.rows.len())),
        );
        let rows = &results.rows;
        let mut visible = Vec::new();
        for row in rows.iter().take(results.shown) {
            let first = row[0];
            visible.push((first, row.len(), false));
            if results
                .expanded
                .contains(&(results.tasks[first].path.clone(), results.tasks[first].line))
            {
                visible.extend(row.iter().skip(1).map(|&ix| (ix, 1, true)));
            }
        }
        let mut previous_group = None;
        for (ix, copies, is_copy) in visible {
            let task = &results.tasks[ix];
            let key = group_key(task, &results.groups);
            if !is_copy && previous_group.as_ref() != Some(&key) {
                list = list.child(
                    div().mt_2().mb_1().text_xs().text_color(muted).child(
                        results
                            .groups
                            .iter()
                            .map(|g| g.label(task))
                            .collect::<Vec<_>>()
                            .join(" · "),
                    ),
                );
                previous_group = Some(key);
            }
            let target = task.clone();
            let root = reader.read(cx).vault_root.clone();
            let reader = self.reader.clone();
            let text = task_label(task);
            let expansion_state = state.clone();
            let task_key = (task.path.clone(), task.line);
            let expanded = results.expanded.contains(&task_key);
            let overdue = !task.checked && task.due.is_some_and(|d| d < now);
            let due_color = if overdue { cx.theme().danger } else { muted };
            list =
                list.child(
                    h_flex()
                        .w_full()
                        .min_h(px(28.))
                        .when(is_copy, |row| row.pl_6())
                        .gap_2()
                        .child(
                            Checkbox::new(("task-check", ix))
                                .checked(task.checked)
                                .disabled(true)
                                .accessibility_label(text.clone())
                                .tooltip("Read-only task — open the source note to edit"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .id(("task-label", ix))
                                .tooltip({
                                    let text = text.clone();
                                    move |window, cx| {
                                        gpui_component::tooltip::Tooltip::new(text.clone())
                                            .build(window, cx)
                                    }
                                })
                                .child(text),
                        )
                        .when(task.priority != 3, |row| {
                            row.child(div().text_xs().text_color(muted).child(
                                match task.priority {
                                    0 => "⇈",
                                    1 | 2 => "↑",
                                    4 => "↓",
                                    _ => "⇊",
                                },
                            ))
                        })
                        .child(
                            Button::new(("task-source", ix))
                                .xsmall()
                                .ghost()
                                .max_w(px(144.))
                                .flex_shrink_0()
                                .label(Group::Filename.label(task))
                                .text_color(muted)
                                .tooltip(format!("Open {} at line {}", task.path, task.line))
                                .debug_selector(move || format!("task-source-{offset}-{ix}"))
                                .on_click(move |_, window, cx| {
                                    let _ = reader.update(cx, |this, cx| {
                                        if this.vault_root == root {
                                            this.prepare_task_document(&target, window, cx);
                                        }
                                    });
                                }),
                        )
                        .when(copies > 1, |row| {
                            row.child(
                                Button::new(("task-copies", ix))
                                    .xsmall()
                                    .ghost()
                                    .label(format!(
                                        "{} ×{} notes",
                                        if expanded { "▾" } else { "▸" },
                                        copies
                                    ))
                                    .tooltip("Show every source of this identical task")
                                    .on_click(move |_, _, cx| {
                                        expansion_state.update(cx, |s, cx| {
                                            if !s.expanded.remove(&task_key) {
                                                s.expanded.insert(task_key.clone());
                                            }
                                            cx.notify();
                                        });
                                    }),
                            )
                        })
                        .when_some(task.due, |row, due| {
                            row.child(
                                div()
                                    .flex_shrink_0()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(due_color)
                                    .bg(due_color.opacity(0.10))
                                    .child(due.to_string()),
                            )
                        }),
                );
        }
        if rows.len() > results.shown {
            list = list.child(
                Button::new("tasks-more")
                    .small()
                    .ghost()
                    .label(format!(
                        "Show {} more",
                        (rows.len() - results.shown).min(20)
                    ))
                    .on_click(move |_, _, cx| {
                        state.update(cx, |s, cx| {
                            s.shown += 20;
                            cx.notify();
                        });
                    }),
            );
        }
        list.into_any_element()
    }
}

impl Reader {
    /// Let block landing finish, then reveal the exact matching text geometry.
    /// Duplicate/unsupported display text stays at its source-bound list, with
    /// an explicit notice instead of selecting another task's first match.
    pub(super) fn land_task_text(&mut self, text: String, cx: &mut Context<Self>) {
        let generation = self.navigation_generation;
        let landing = self.landing_generation;
        cx.spawn(async move |reader, cx| {
            for _ in 0..100 {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                let done = reader.update(cx, |this, cx| {
                    if this.navigation_generation != generation || this.landing_generation != landing { return true; }
                    if this.link_notice.is_some() { return true; }
                    if this.pending_landing.is_some() { return false; }
                    let unique = this.content.update(cx, |state, cx| {
                        state.set_search_query(text.clone(), cx);
                        if state.search_status().1 == 1 { true }
                        else { state.set_search_query("", cx); false }
                    });
                    if !unique {
                        this.link_notice = Some("Opened the task's list. Its text is repeated or cannot be located precisely in the rendered note.".into());
                    }
                    cx.notify();
                    true
                }).unwrap_or(true);
                if done { break; }
            }
        }).detach();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn presentation_preserves_source_and_groups_without_merging_equal_stems() {
        let task = tessera_core::tasks::parse(
            "a/Work.md",
            "- [ ] **Ship** 🔼 📅 2026-10-06 ⏳ 2026-10-05",
        )
        .remove(0);
        assert_eq!(task_label(&task), "Ship");
        assert!(task.text.contains("📅"));
        assert_eq!(Group::Filename.label(&task), "Work");
        let mut other = task.clone();
        other.path = "b/Work.md".into();
        assert_ne!(
            group_key(&task, &[Group::Filename]),
            group_key(&other, &[Group::Filename])
        );
        assert_eq!(
            carried_rows(&[task.clone(), other.clone()], &[Group::Filename], false),
            vec![vec![0, 1]]
        );
        assert_eq!(
            carried_rows(&[task.clone(), other.clone()], &[Group::Filename], true),
            vec![vec![0], vec![1]]
        );
        other.due = None;
        assert_eq!(
            carried_rows(&[task.clone(), other], &[Group::Filename], false).len(),
            2
        );
        assert_eq!(
            carried_rows(&[task.clone(), task], &[Group::Filename], false).len(),
            2
        );
        let query = Query::parse("not done\ngroup by due\ngroup by filename", today());
        assert!(query.unsupported.is_empty());
        assert_eq!(query.groups, vec![Group::Due, Group::Filename]);
        assert!(!Query::parse("group by unknown", today())
            .unsupported
            .is_empty());
    }

    #[test]
    fn index_refresh_keeps_expanded_pagination() {
        let mut results = Results {
            index: Arc::default(),
            today: today(),
            query: "not done".into(),
            tasks: Vec::new(),
            rows: Vec::new(),
            unsupported: Vec::new(),
            groups: vec![Group::Filename],
            explicit_groups: false,
            expanded: Default::default(),
            shown: 150,
        };
        results.expanded.insert(("old.md".into(), 1));
        let mut index = Index::default();
        index.replace("new.md", "- [ ] New task");
        results.refresh(Arc::new(index), today(), "not done");
        assert_eq!(results.tasks.len(), 1);
        assert_eq!(results.shown, 150);
        assert!(results.expanded.contains(&("old.md".into(), 1)));
        results.refresh(results.index.clone(), today(), "done");
        assert_eq!(results.shown, 20);
        assert!(results.expanded.is_empty());
    }

    #[gpui::test]
    fn native_queries_refresh_incrementally_and_open_source_without_writes(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let dashboard = "# Tasks\n\n```tasks\nnot done\n```\n\n```tasks\ndone\n```\n\n```tasks\nunknown filter\n```\n";
        let task_source = "# Work\n\n- [ ] First task 📅 2026-10-06\n";
        std::fs::write(root.join("Dashboard.md"), dashboard).unwrap();
        std::fs::write(root.join("Work.md"), task_source).unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Dashboard.md".into()),
                        index_dir: Some(temp.path().join("cache")),
                        session_directory: Some(temp.path().join("state")),
                        panel_settings_override: Some(temp.path().join("panels.json")),
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
        let open_source = "task-source-9-0";
        let done_source = "task-source-32-0";
        visual.run_until_parked();
        assert!(
            visual.debug_bounds(open_source).is_some(),
            "positive control: a real query result rendered"
        );
        assert!(visual.debug_bounds("tasks-count-1").is_some());
        assert!(visual.debug_bounds("tasks-count-0").is_some());
        assert!(visual.debug_bounds("tasks-unsupported").is_some());
        let button = visual.debug_bounds(open_source).unwrap();
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
        reader.update(visual, |r, cx| {
            assert_eq!(r.current_rel, "Work.md");
            assert_eq!(r.content.read(cx).search_status().1, 1);
            assert!(r.editing.is_none(), "read-only navigation");
        });
        assert_eq!(
            std::fs::read_to_string(root.join("Work.md")).unwrap(),
            task_source
        );
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Dashboard.md", None, window, cx)
        });
        visual.run_until_parked();
        std::fs::write(
            root.join("Work.md"),
            "# Work\n\n- [x] Finished ✅ 2026-10-06\n",
        )
        .unwrap();
        reader.update_in(visual, |r, window, cx| {
            let changes = tessera_core::Changes {
                changed: ["Work.md".into()].into_iter().collect(),
                ..Default::default()
            };
            r.start_incremental(changes, window, cx);
        });
        visual.run_until_parked();
        reader.update(visual, |r, _| {
            let index = r.tasks_index.as_ref().unwrap();
            assert!(index.query(&Query::parse("not done", today())).is_empty());
            assert_eq!(index.query(&Query::parse("done", today())).len(), 1);
        });
        assert!(
            visual.debug_bounds(done_source).is_some(),
            "completed query now renders its result"
        );
        std::fs::remove_file(root.join("Work.md")).unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.start_incremental(
                tessera_core::Changes {
                    removed: ["Work.md".into()].into_iter().collect(),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds(done_source).is_none(),
            "removal invalidates visible results"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("Dashboard.md")).unwrap(),
            dashboard
        );
        reader.update(visual, |r, cx| {
            r.tasks_index = None;
            assert!(!r.loading.as_ref().unwrap().active && !r.incremental_initializing);
            r.link_notice =
                Some("The document did not finish rendering at the requested position.".into());
            r.land_task_text("missing".into(), cx);
            cx.notify();
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
        assert!(visual.debug_bounds("tasks-unavailable").is_some());
        assert!(visual.debug_bounds("tasks-indexing").is_none());
        reader.update(visual, |r, _| {
            assert_eq!(
                r.link_notice.as_deref(),
                Some("The document did not finish rendering at the requested position.")
            )
        });
    }
}
