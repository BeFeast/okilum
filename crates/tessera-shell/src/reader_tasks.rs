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
    has_heading: bool,
}

#[derive(Clone)]
struct TasksHeading {
    query: String,
    offset: usize,
}

// Only attach counts to an immediately preceding, standalone heading. The
// heading itself remains a native Markdown part (links, selection and find).
fn preceding_heading(source: &str) -> Option<usize> {
    let trimmed = source.trim_end();
    let start = trimmed.rfind('\n').map_or(0, |i| i + 1);
    let line = &trimmed[start..];
    let atx = line.trim_start_matches(' ');
    let hashes = atx.chars().take_while(|&c| c == '#').count();
    if line.len() - atx.len() <= 3
        && (1..=6).contains(&hashes)
        && atx[hashes..].chars().next().is_none_or(char::is_whitespace)
    {
        return Some(start);
    }
    let underline = line.trim();
    if !underline.is_empty()
        && (underline.chars().all(|c| c == '=') || underline.chars().all(|c| c == '-'))
    {
        let previous = source[..start]
            .strip_suffix('\n')?
            .strip_suffix('\r')
            .unwrap_or(source[..start].strip_suffix('\n')?);
        let previous_start = previous.rfind('\n').map_or(0, |i| i + 1);
        let standalone = previous_start == 0
            || previous[..previous_start].ends_with("\n\n")
            || previous[..previous_start].ends_with("\r\n\r\n");
        if standalone && !previous[previous_start..].trim().is_empty() {
            return Some(previous_start);
        }
    }
    None
}
fn following_tasks(source: &str, end: usize) -> Option<String> {
    let mut lines = source
        .get(end..)?
        .lines()
        .skip_while(|line| line.trim().is_empty());
    let raw_opener = lines.next()?;
    let opener = raw_opener.trim_start_matches(' ');
    if raw_opener.len() - opener.len() > 3 {
        return None;
    }
    let marker = opener.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let length = opener.chars().take_while(|&c| c == marker).count();
    if length < 3 || opener[length..].split_whitespace().next() != Some("tasks") {
        return None;
    }
    if marker == '`' && opener[length..].contains('`') {
        return None;
    }
    let mut query = Vec::new();
    for line in lines {
        let fence = line.trim_start_matches(' ');
        if line.len() - fence.len() <= 3
            && fence.trim_end().len() >= length
            && fence.trim_end().chars().all(|c| c == marker)
        {
            break;
        }
        query.push(line);
    }
    Some(query.join("\n"))
}
fn query_source_markdown(source: &str) -> String {
    let longest = source.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}text\n{source}\n{fence}")
}

pub(super) fn plugins(view: TextView, reader: WeakEntity<Reader>) -> TextView {
    let counts_reader = reader.clone();
    view.markdown_block_parser(|node, cx| {
        let source = cx.node_source(node).unwrap_or("");
        if let markdown_ast::Node::Heading(_) = node {
            let position = node.position()?;
            if preceding_heading(&cx.source()[..position.end.offset]) != Some(position.start.offset)
            {
                return None;
            }
            let query = following_tasks(cx.source(), position.end.offset)?;
            return Some(
                MarkdownNode::new(
                    "tasks-heading",
                    TasksHeading {
                        query: query.clone(),
                        offset: position.start.offset + cx.offset(),
                    },
                )
                .markdown_part(format!("heading:{query}"), source.to_owned())
                .text(source.to_owned())
                .markdown(source.to_owned()),
            );
        }
        let markdown_ast::Node::Code(code) = node else {
            return None;
        };
        if matches!(code.lang.as_deref(), Some("dataview" | "dataviewjs")) {
            return Some(
                MarkdownNode::new(
                    "unsupported-dataview",
                    UnsupportedQuery {
                        source: code.value.clone(),
                        offset: node.position().map_or(0, |p| p.start.offset) + cx.offset(),
                    },
                )
                .markdown(source.to_owned()),
            );
        }
        if code.lang.as_deref() != Some("tasks") {
            return None;
        }
        let start = node.position().map_or(0, |p| p.start.offset);
        let has_heading = preceding_heading(&cx.source()[..start]).is_some();
        Some(
            MarkdownNode::new(
                "tasks-query",
                TasksBlock {
                    query: code.value.clone(),
                    offset: start + cx.offset(),
                    has_heading,
                },
            )
            .text(source.to_owned())
            .markdown(source.to_owned()),
        )
    })
    .markdown_block_renderer("tasks-heading", move |node, window, cx| {
        let heading = node.data::<TasksHeading>().unwrap();
        let query = heading.query.clone();
        h_flex()
            .id(("tasks-heading", heading.offset))
            .gap_3()
            .flex_wrap()
            .items_center()
            .child(
                div()
                    .debug_selector(|| "tasks-section-heading".into())
                    .child(node.render_part(
                        &format!("heading:{query}"),
                        |style| style,
                        window,
                        cx,
                    )),
            )
            .child(TaskCounts {
                query,
                reader: counts_reader.clone(),
            })
            .into_any_element()
    })
    .markdown_block_renderer("unsupported-dataview", |node, _, _| {
        let block = node.data::<UnsupportedQuery>().unwrap();
        div()
            .id(("dataview-block", block.offset))
            .child(block.clone())
            .into_any_element()
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
                has_heading: block.has_heading,
                layout: Default::default(),
                title: None,
            })
            .into_any_element()
    })
}

#[derive(Clone, IntoElement)]
struct UnsupportedQuery {
    source: String,
    offset: usize,
}
impl RenderOnce for UnsupportedQuery {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state("dataview-source", cx, |_, cx| {
            (
                self.source.clone(),
                false,
                cx.new(|cx| TextViewState::markdown(&query_source_markdown(&self.source), cx)),
            )
        });
        if state.read(cx).0 != self.source {
            state.update(cx, |s, cx| {
                s.0 = self.source.clone();
                s.1 = false;
                s.2.update(cx, |view, cx| {
                    view.set_text(&query_source_markdown(&self.source), cx)
                });
            });
        }
        let expanded = state.read(cx).1;
        let source = state.read(cx).2.clone();
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Dataview query — not supported yet"),
                    )
                    .child(
                        Button::new("dataview-toggle")
                            .small()
                            .ghost()
                            .label(if expanded {
                                "Hide source"
                            } else {
                                "Show source"
                            })
                            .debug_selector(|| "dataview-toggle".into())
                            .on_click(move |_, _, cx| {
                                state.update(cx, |s, cx| {
                                    s.1 = !s.1;
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .when(expanded, |view| {
                view.child(
                    div()
                        .debug_selector(|| "dataview-source".into())
                        .child(TextView::new(&source)),
                )
            })
    }
}

#[derive(IntoElement)]
struct TaskCounts {
    query: String,
    reader: WeakEntity<Reader>,
}
impl RenderOnce for TaskCounts {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(index) = self
            .reader
            .upgrade()
            .and_then(|r| r.read(cx).tasks_index.clone())
        else {
            return div().into_any_element();
        };
        let state = results_state(window, cx);
        state.update(cx, |s, _| s.refresh(index, today(), &self.query));
        let results = state.read(cx);
        if !results.unsupported.is_empty() || results.rows.is_empty() {
            return div().into_any_element();
        }
        let count = results.rows.len();
        div()
            .debug_selector(move || format!("tasks-section-count-{count}"))
            .child(count_badge(results, cx))
            .into_any_element()
    }
}
fn count_badge(results: &Results, cx: &App) -> impl IntoElement {
    let count = results.rows.len();
    if count == 0 {
        return div().into_any_element();
    }
    div()
        .text_sm()
        .font_weight(FontWeight::NORMAL)
        .text_color(cx.theme().muted_foreground)
        .px_2()
        .py_0p5()
        .rounded_md()
        .bg(cx.theme().muted_foreground.opacity(0.08))
        .debug_selector(move || format!("tasks-count-{count}"))
        .child(format!("{count} tasks · {} in notes", results.tasks.len()))
        .into_any_element()
}

#[derive(IntoElement)]
struct TasksList {
    title: Option<String>,
    layout: tessera_core::typed_view::layout::Defaults,
    has_heading: bool,
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
    grouping: tessera_core::typed_view::layout::Grouping,
    expanded: std::collections::BTreeSet<(String, usize)>,
    shown: usize,
}
impl Results {
    fn refresh(&mut self, index: Arc<Index>, now: time::Date, source: &str) {
        self.refresh_grouped(index, now, source, Default::default());
    }
    fn refresh_grouped(
        &mut self,
        index: Arc<Index>,
        now: time::Date,
        source: &str,
        grouping: tessera_core::typed_view::layout::Grouping,
    ) {
        use tessera_core::typed_view::layout::Grouping;
        if !Arc::ptr_eq(&self.index, &index)
            || self.today != now
            || self.query != source
            || self.grouping != grouping
        {
            let query = Query::parse(source, now);
            self.tasks = index.query(&query);
            self.tasks.retain(|task| !task_label(task).is_empty());

            self.explicit_groups = grouping == Grouping::Note
                || (grouping == Grouping::Query && !query.groups.is_empty());
            self.groups = if query.groups.is_empty() {
                vec![Group::Filename]
            } else {
                query.groups
            };
            match grouping {
                Grouping::Note => self.groups = vec![Group::Filename],
                Grouping::None => self.groups.clear(),
                Grouping::Query => {}
            }
            self.grouping = grouping;
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
    static DATES: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let dates = DATES.get_or_init(|| {
        regex::Regex::new(r"[📅⏳🛫✅]\u{fe0f}?\s*([0-9]{4}-[0-9]{2}-[0-9]{2})").unwrap()
    });
    text = dates
        .replace_all(&text, |capture: &regex::Captures<'_>| {
            if time::Date::parse(
                &capture[1],
                &time::macros::format_description!("[year]-[month]-[day]"),
            )
            .is_ok()
            {
                String::new()
            } else {
                capture[0].to_owned()
            }
        })
        .into_owned();
    for marker in ['🔺', '⏫', '🔼', '🔽', '⏬'] {
        text = text.replace(marker, "");
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn source_label(task: &Task) -> String {
    let stem = Group::Filename.label(task);
    time::Date::parse(
        &stem,
        &time::macros::format_description!("[year]-[month]-[day]"),
    )
    .ok()
    .and_then(|date| {
        date.format(&time::macros::format_description!(
            "[weekday repr:short] [day padding:none] [month repr:short] [year]"
        ))
        .ok()
    })
    .unwrap_or(stem)
}
fn group_label(group: Group, task: &Task) -> String {
    if group == Group::Filename {
        source_label(task)
    } else {
        group.label(task)
    }
}
// SVG strokes fill a predictable secondary-text-sized box; font arrow glyphs
// have much smaller ink bounds than the adjacent backlink text.
fn priority_icon(priority: u8, color: Hsla) -> impl IntoElement {
    let down = priority >= 4;
    div()
        .relative()
        .size(rems(1.))
        .flex_shrink_0()
        .text_color(color)
        .debug_selector(|| "task-priority".into())
        .when(matches!(priority, 0 | 5), |el| {
            let chevron = if down {
                IconName::ChevronDown
            } else {
                IconName::ChevronUp
            };
            el.child(
                Icon::new(chevron.clone())
                    .size(rems(0.75))
                    .absolute()
                    .top_0()
                    .left_0(),
            )
            .child(
                Icon::new(chevron)
                    .size(rems(0.75))
                    .absolute()
                    .top(rems(0.25))
                    .left_0(),
            )
        })
        .when(!matches!(priority, 0 | 5), |el| {
            el.child(
                Icon::new(if down {
                    IconName::ArrowDown
                } else {
                    IconName::ArrowUp
                })
                .size(rems(1.)),
            )
        })
}
pub(super) fn today() -> time::Date {
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
fn results_state(window: &mut Window, cx: &mut App) -> Entity<Results> {
    let now = today();
    window.use_keyed_state("tasks-results", cx, |_, _cx| {
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
            grouping: Default::default(),
            expanded: Default::default(),
            shown: 20,
        }
    })
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
        let state = results_state(window, cx);
        state.update(cx, |s, _| {
            s.refresh_grouped(index, now, &self.query, self.layout.grouping)
        });
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
        if results.rows.is_empty() {
            return div().into_any_element();
        }
        if !self.has_heading {
            list = list.child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .text_color(muted)
                    .child(
                        div()
                            .when(self.title.is_some(), |title| {
                                title
                                    .text_base()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                            })
                            .child(self.title.clone().unwrap_or_else(|| "Tasks".into())),
                    )
                    .child(count_badge(results, cx)),
            );
        }
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
            let grouped_by_note = results
                .groups
                .iter()
                .any(|g| matches!(g, Group::Filename | Group::Path));

            if !is_copy && !results.groups.is_empty() && previous_group.as_ref() != Some(&key) {
                let label = results
                    .groups
                    .iter()
                    .map(|g| group_label(*g, task))
                    .collect::<Vec<_>>()
                    .join(" · ");
                let target = task.clone();
                let source_reader = self.reader.clone();
                let root = reader.read(cx).vault_root.clone();
                let heading = if grouped_by_note {
                    Button::new(("task-group", ix))
                        .small()
                        .w_full()
                        .px_0()
                        .h_5()
                        .ghost()
                        .text_sm()
                        .text_color(muted)
                        .accessibility_label(label.clone())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .truncate()
                                .debug_selector(move || format!("task-group-label-{offset}-{ix}"))
                                .child(label),
                        )
                        .tooltip(format!("Open {} at line {}", task.path, task.line))
                        .debug_selector(move || format!("task-group-{offset}-{ix}"))
                        .on_click(move |_, window, cx| {
                            let _ = source_reader.update(cx, |this, cx| {
                                if this.vault_root == root {
                                    this.prepare_task_document(&target, window, cx);
                                }
                            });
                        })
                        .into_any_element()
                } else {
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(label)
                        .into_any_element()
                };
                list = list.child(div().pl_6().mt_1().child(heading));
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
            list = list.child(
                h_flex()
                    .w_full()
                    .min_h(px(
                        if self.layout.density
                            == tessera_core::typed_view::layout::Density::Comfortable
                        {
                            36.
                        } else {
                            28.
                        },
                    ))
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
                        Button::new(("task-title", ix))
                            .small()
                            .ghost()
                            .justify_start()
                            .px_0()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .tooltip(format!("Open {} at line {} — {text}", task.path, task.line))
                            .on_click({
                                let target = target.clone();
                                let root = root.clone();
                                let reader = reader.clone();
                                move |_, window, cx| {
                                    let _ = reader.update(cx, |this, cx| {
                                        if this.vault_root == root {
                                            this.prepare_task_document(&target, window, cx);
                                        }
                                    });
                                }
                            })
                            .debug_selector(move || format!("task-title-{offset}-{ix}"))
                            .accessibility_label(text.clone())
                            .child(div().flex_1().min_w_0().text_sm().truncate().child(text)),
                    )
                    .when(task.priority != 3, |row| {
                        row.child(priority_icon(task.priority, muted))
                    })
                    .when(!grouped_by_note || is_copy, |row| {
                        row.child(
                            Button::new(("task-source", ix))
                                .small()
                                .text_sm()
                                .ghost()
                                .max_w(px(240.))
                                .flex_shrink_0()
                                .accessibility_label(source_label(task))
                                .child(div().text_sm().truncate().child(source_label(task)))
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
                    })
                    .when(copies > 1, |row| {
                        row.child(
                            Button::new(("task-copies", ix))
                                .small()
                                .text_sm()
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
                                .child(if due == now {
                                    "Today".to_owned()
                                } else if Some(due) == now.next_day() {
                                    "Tomorrow".to_owned()
                                } else {
                                    due.format(&time::macros::format_description!(
                                        "[day padding:none] [month repr:short] [year]"
                                    ))
                                    .unwrap_or_default()
                                }),
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
    fn polish_source_labels_empty_tasks_and_fences() {
        let tasks = tessera_core::tasks::parse(
            "Daily/2025-10-27.md",
            "- [ ] Ship\n- [ ] \n- [ ] 🔼 📅2026-10-06",
        );
        assert_eq!(source_label(&tasks[0]), "Mon 27 Oct 2025");
        assert_eq!(
            tasks.iter().filter(|t| !task_label(t).is_empty()).count(),
            1
        );
        assert_eq!(
            following_tasks("## Open\n\n~~~tasks\nnot done\n~~~\n", 7).as_deref(),
            Some("not done")
        );
        assert_eq!(
            following_tasks("## Open\n\n    ```tasks\nnot done\n    ```", 7),
            None
        );
        assert_eq!(preceding_heading("Paragraph\n\n---\n\n"), None);
        assert_eq!(preceding_heading("Open\n====\n"), Some(0));
        assert_eq!(
            query_source_markdown("x\n```\ny"),
            "````text\nx\n```\ny\n````"
        );
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
            grouping: Default::default(),
            expanded: Default::default(),
            shown: 150,
        };
        results.expanded.insert(("old.md".into(), 1));
        let mut index = Index::default();
        index.replace("new.md", "- [ ] New task\n- [ ] ");
        index.replace("copy.md", "- [ ] New task");
        results.refresh(Arc::new(index), today(), "not done");
        assert_eq!(results.tasks.len(), 2);
        assert_eq!(
            results.rows.len(),
            1,
            "counts distinguish tasks from nonempty occurrences"
        );
        assert_eq!(results.shown, 150);
        assert!(results.expanded.contains(&("old.md".into(), 1)));
        results.refresh_grouped(
            results.index.clone(),
            today(),
            "not done\ngroup by filename",
            tessera_core::typed_view::layout::Grouping::Note,
        );
        assert!(results.explicit_groups);
        assert_eq!(results.rows.len(), 2);
        results.refresh_grouped(
            results.index.clone(),
            today(),
            "not done\ngroup by filename",
            tessera_core::typed_view::layout::Grouping::None,
        );
        assert!(!results.explicit_groups);
        assert!(results.groups.is_empty());
        assert_eq!(
            results.rows.len(),
            1,
            "ungrouped carried copies collapse again"
        );
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
        let dashboard = "# Tasks\n\n```tasks\nnot done\n```\n\n```tasks\ndone\n```\n\n```tasks\nunknown filter\n```\n\n```dataview\nTABLE file.name\n```\n";
        let task_source =
            "# Work\n\n- [ ] First task 🔼 📅 2026-10-06\n- [ ] \n- [ ] 🔼 📅2026-10-06\n";
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
        let open_source = "task-title-9-0";
        let done_source = "task-title-32-0";
        visual.run_until_parked();
        assert!(
            visual.debug_bounds(open_source).is_some(),
            "positive control: a real query result rendered"
        );
        assert!(visual.debug_bounds("tasks-count-1").is_some());
        assert!(visual.debug_bounds("tasks-count-0").is_none());
        assert!(visual.debug_bounds("tasks-unsupported").is_some());
        let heading = visual.debug_bounds("tasks-section-heading").unwrap();
        let badge = visual.debug_bounds("tasks-count-1").unwrap();
        assert!(badge.origin.x >= heading.origin.x + heading.size.width);
        assert!((badge.center().y - heading.center().y).abs() < px(12.));
        assert!(
            visual.debug_bounds("task-source-9-0").is_none(),
            "grouped source is not repeated"
        );
        assert!(visual.debug_bounds("task-group-9-0").is_some());
        let group_label = visual.debug_bounds("task-group-label-9-0").unwrap();
        let task_title = visual.debug_bounds(open_source).unwrap();
        let priority = visual.debug_bounds("task-priority").unwrap();
        assert!(
            priority.size.height >= group_label.size.height * 0.8,
            "priority is comparable to backlink text"
        );
        assert!(
            (group_label.origin.x - task_title.origin.x).abs() <= px(2.),
            "group label aligns with the task column"
        );
        assert!(
            task_title.origin.y - (group_label.origin.y + group_label.size.height) <= px(8.),
            "group stays close to its tasks"
        );
        let toggle = visual.debug_bounds("dataview-toggle").unwrap();
        assert!(visual.debug_bounds("dataview-source").is_none());
        visual.simulate_click(toggle.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("dataview-source").is_some(),
            "positive control: source expands"
        );
        visual.simulate_click(toggle.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("dataview-source").is_none());
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
        // A prose edit leaves query rows identical but must publish fresh
        // revision evidence. An action retained from the old render must fail.
        let shown_index = reader.update(visual, |r, _| r.tasks_index.clone().unwrap());
        let shown_rows = shown_index.query(&Query::parse("not done", today()));
        let prose_changed = format!("{task_source}\nNew prose\n");
        std::fs::write(root.join("Work.md"), &prose_changed).unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.start_incremental(
                tessera_core::Changes {
                    changed: ["Work.md".into()].into_iter().collect(),
                    ..Default::default()
                },
                window,
                cx,
            );
        });
        visual.run_until_parked();
        reader.update(visual, |r, _| {
            let fresh_index = r.tasks_index.as_ref().unwrap();
            assert!(!Arc::ptr_eq(&shown_index, fresh_index));
            assert_eq!(
                fresh_index.query(&Query::parse("not done", today())),
                shown_rows
            );
            assert!(shown_index
                .edit_target(&shown_rows[0], &prose_changed)
                .is_err());
            assert!(fresh_index
                .edit_target(&shown_rows[0], &prose_changed)
                .is_ok());
        });
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
        // Changing only the query must refresh its adjacent badge even though
        // the heading's own Markdown and the projected task index are unchanged.
        std::fs::write(
            root.join("Dashboard.md"),
            dashboard.replace("not done", "done    "),
        )
        .unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.start_incremental(
                tessera_core::Changes {
                    changed: ["Dashboard.md".into()].into_iter().collect(),
                    ..Default::default()
                },
                window,
                cx,
            );
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds(open_source).is_some());
        assert!(
            visual.debug_bounds("tasks-count-0").is_none(),
            "heading badge follows query-only edits"
        );
        let refreshed_badge = visual.debug_bounds("tasks-section-count-1").unwrap();
        let refreshed_heading = visual.debug_bounds("tasks-section-heading").unwrap();
        assert!(
            (refreshed_badge.center().y - refreshed_heading.center().y).abs() < px(12.),
            "query-only edits restore the nonzero badge beside the heading"
        );
        std::fs::write(root.join("Dashboard.md"), dashboard).unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.start_incremental(
                tessera_core::Changes {
                    changed: ["Dashboard.md".into()].into_iter().collect(),
                    ..Default::default()
                },
                window,
                cx,
            );
        });
        visual.run_until_parked();
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

pub(super) fn dashboard_section(
    title: Option<String>,
    query: String,
    offset: usize,
    layout: tessera_core::typed_view::layout::Defaults,
    reader: WeakEntity<Reader>,
) -> impl IntoElement {
    div().id(("native-tasks-section", offset)).child(TasksList {
        query,
        offset,
        layout,
        reader,
        title,
        has_heading: false,
    })
}
