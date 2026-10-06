//! Native, read-only Tasks blocks. Index ownership stays with Reader snapshots.
use super::*;
use tessera_core::tasks::{Index, Query, Task};

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
                reader: reader.clone(),
            })
            .into_any_element()
    })
}

#[derive(IntoElement)]
struct TasksList {
    query: String,
    reader: WeakEntity<Reader>,
}
struct Results {
    index: Arc<Index>,
    today: time::Date,
    query: String,
    tasks: Vec<Task>,
    unsupported: Vec<String>,
    shown: usize,
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
            return div().text_sm().child("Indexing tasks…").into_any_element();
        };
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
                unsupported: Vec::new(),
                shown: 50,
            }
        });
        state.update(cx, |s, _| {
            if !Arc::ptr_eq(&s.index, &index) || s.today != now || s.query != self.query {
                let query = Query::parse(&self.query, now);
                s.tasks = index.query(&query);
                s.unsupported = query.unsupported;
                s.index = index;
                s.today = now;
                s.query = self.query;
                s.shown = 50;
            }
        });
        let results = state.read(cx);
        let muted = cx.theme().muted_foreground;
        let mut list = v_flex().gap_2().py_2().w_full();
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
                    let count = results.tasks.len();
                    move || format!("tasks-count-{count}")
                })
                .child(format!("{} tasks · read-only", results.tasks.len())),
        );
        for (ix, task) in results.tasks.iter().take(results.shown).enumerate() {
            let target = task.clone();
            let root = reader.read(cx).vault_root.clone();
            let reader = self.reader.clone();
            let due = task.due.map(|d| format!("📅 {d}"));
            let text = task
                .display
                .clone()
                .unwrap_or_else(|| tessera_core::render::strip_inline_markdown(&task.text));
            let text = due
                .as_ref()
                .map_or_else(|| text.clone(), |due| text.replace(due, ""));
            list = list.child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .items_start()
                            .gap_2()
                            .child(div().text_color(muted).child(if task.checked {
                                "☑"
                            } else {
                                "☐"
                            }))
                            .child(div().flex_1().child(text.trim().to_owned()))
                            .when_some(due, |row, due| {
                                row.child(div().text_xs().text_color(muted).child(due))
                            }),
                    )
                    .child(
                        Button::new(("task-source", ix))
                            .xsmall()
                            .ghost()
                            .label(format!("{}:{}", task.path, task.line))
                            .debug_selector(move || format!("task-source-{ix}"))
                            .on_click(move |_, window, cx| {
                                let _ = reader.update(cx, |this, cx| {
                                    if this.vault_root == root {
                                        this.prepare_task_document(&target, window, cx);
                                    }
                                });
                            }),
                    ),
            );
        }
        if results.tasks.len() > results.shown {
            list = list.child(
                Button::new("tasks-more")
                    .small()
                    .ghost()
                    .label("Show 50 more")
                    .on_click(move |_, _, cx| {
                        state.update(cx, |s, cx| {
                            s.shown += 50;
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
        cx.spawn(async move |reader, cx| {
            for _ in 0..100 {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                let done = reader.update(cx, |this, cx| {
                    if this.navigation_generation != generation { return true; }
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
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("task-source-0").is_some(),
            "positive control: a real query result rendered"
        );
        assert!(visual.debug_bounds("tasks-count-1").is_some());
        assert!(visual.debug_bounds("tasks-count-0").is_some());
        assert!(visual.debug_bounds("tasks-unsupported").is_some());
        let button = visual.debug_bounds("task-source-0").unwrap();
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
            visual.debug_bounds("task-source-0").is_some(),
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
            visual.debug_bounds("task-source-0").is_none(),
            "removal invalidates visible results"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("Dashboard.md")).unwrap(),
            dashboard
        );
    }
}
