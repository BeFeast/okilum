//! Native query blocks and their shared source navigation and explicit row controls.
//! Writes stay on the revision-bound FileEditor + Undo path in `reader_task_actions`.
use super::*;
use gpui_component::checkbox::Checkbox;
use okilum_core::{
    records::{FieldSchema, Projection, Value as RecordValue},
    tasks::{Index as TasksIndex, Task as IndexedTask},
    view::{Row, Section, Show},
};
use serde_json::Value as Json;
use std::ops::Range;

const ROW_LABEL_LINE: Rems = Rems(1.25);
const ACTION_FENCE_PREFIX: &str = "okilum-view-actions-";

pub(super) struct Blocks {
    nonce: String,
    lang: String,
    items: Vec<ActionBlock>,
}

struct ActionBlock {
    show: Show,
    cols: Vec<String>,
    rows: Vec<ActionRow>,
    groups: Vec<DisplayGroup>,
}

pub(super) struct DisplayGroup {
    label: String,
    summary: String,
    pub(super) rows: Range<usize>,
}

impl DisplayGroup {
    pub(super) fn heading(&self, cx: &App) -> Div {
        v_flex()
            .gap_1()
            .when(!self.label.is_empty(), |heading| {
                heading.child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(self.label.clone()),
                )
            })
            .when(!self.summary.is_empty(), |heading| {
                heading.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(self.summary.clone()),
                )
            })
    }
}

pub(super) struct ActionRow {
    path: String,
    line: Option<usize>,
    anchor: Option<String>,
    rev: String,
    primary: String,
    meta: Vec<String>,
    check: Option<CheckAction>,
    thread: Option<String>,
    open_task: Option<IndexedTask>,
    pub(super) date: Option<timeline::Date>,
}

#[derive(Clone)]
struct CheckAction {
    checked: bool,
    schema: Arc<FieldSchema>,
    index: Option<Arc<TasksIndex>>,
    task: Option<IndexedTask>,
}

#[derive(Clone)]
struct BlockRef {
    nonce: String,
    index: usize,
    raw: String,
}

impl Blocks {
    pub(super) fn new() -> Self {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let lang = format!("{ACTION_FENCE_PREFIX}{nonce}");
        Self {
            nonce,
            lang,
            items: Vec::new(),
        }
    }

    pub(super) fn push(
        &mut self,
        section: &Section,
        rows: &[Row],
        projection: &Projection,
        tasks: Option<&Arc<TasksIndex>>,
    ) -> usize {
        let index = self.items.len();
        let rows = rows
            .iter()
            .map(|row| build_row(section, row, projection, tasks))
            .collect();
        self.items.push(ActionBlock {
            show: section.show,
            cols: section.cols.clone(),
            rows,
            groups: Vec::new(),
        });
        index
    }

    pub(super) fn push_display(
        &mut self,
        section: &Section,
        projection: &Projection,
        tasks: Option<&Arc<TasksIndex>>,
    ) -> usize {
        let index = self.push(section, &section.rows, projection, tasks);
        let mut offset = 0;
        let mut groups: Vec<_> = section
            .groups
            .iter()
            .map(|group| {
                let end = offset + group.count.min(section.rows.len() - offset);
                let rows = offset..end;
                offset = end;
                DisplayGroup {
                    label: group
                        .key
                        .iter()
                        .map(|key| {
                            if key.is_null() {
                                "(none)".into()
                            } else {
                                plain_value(key)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" / "),
                    summary: group
                        .agg
                        .iter()
                        .map(|(name, value)| format!("{name} {}", plain_value(value)))
                        .collect::<Vec<_>>()
                        .join(" · "),
                    rows,
                }
            })
            .collect();
        if groups.is_empty() {
            groups.push(DisplayGroup {
                label: String::new(),
                summary: String::new(),
                rows: 0..section.rows.len(),
            });
        }
        let block = &mut self.items[index];
        if block.show == Show::Timeline {
            for group in &groups {
                timeline::sort(&mut block.rows[group.rows.clone()]);
            }
        }
        block.groups = groups;
        index
    }

    pub(super) fn fence(&self, index: usize) -> String {
        format!("```{}\n{index}\n```\n", self.lang)
    }
}

pub(super) fn has_actions(rows: &[Row]) -> bool {
    rows.iter().any(|row| {
        row.actions
            .iter()
            .any(|a| a == "check" || a == "open:thread")
    })
}

fn plain_value(value: &Json) -> String {
    match value {
        Json::Null => String::new(),
        Json::String(text) => text.replace(['\r', '\n', '\t'], " "),
        Json::Array(items) => items
            .iter()
            .map(plain_value)
            .filter(|item| !item.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        Json::Object(object) if object.get("link").and_then(Json::as_str).is_some() => {
            let mut label = object["link"]
                .as_str()
                .unwrap_or_default()
                .replace(['\r', '\n', '\t'], " ");
            if let Some(paths) = object.get("ambiguous").and_then(Json::as_array) {
                let candidates = paths.iter().map(plain_value).collect::<Vec<_>>().join(", ");
                label.push_str(&format!(" (ambiguous: {candidates})"));
            }
            label
        }
        other => other.to_string(),
    }
}

fn row_source(path: &str, line: Option<usize>) -> String {
    match line {
        Some(line) => format!("{path}:{line}"),
        None => path.to_owned(),
    }
}

fn find_indexed_task(
    tasks: Option<&Arc<TasksIndex>>,
    path: &str,
    line: Option<usize>,
    expected_rev: Option<&str>,
) -> Option<IndexedTask> {
    let index = tasks?;
    let line = line?;
    if let Some(rev) = expected_rev {
        if index.note_revision(path) != Some(rev) {
            return None;
        }
    }
    index
        .note_tasks(path)
        .into_iter()
        .find(|task| task.line == line)
}

fn row_checked(projection: &Projection, row: &Row, task: Option<&IndexedTask>) -> bool {
    if let Some(checked) = row.line.and_then(|line| {
        projection
            .note(&row.path)?
            .tasks
            .iter()
            .find(|t| t.line == line)
            .and_then(|t| match t.fields.get("checked") {
                Some(RecordValue::Bool(checked)) => Some(*checked),
                _ => None,
            })
    }) {
        return checked;
    }
    task.is_some_and(|task| task.checked)
}

fn row_thread(projection: &Projection, row: &Row) -> Option<String> {
    if !row.actions.iter().any(|action| action == "open:thread") {
        return None;
    }
    let note = projection.note(&row.path)?;
    let value = row
        .line
        .and_then(|line| {
            note.tasks
                .iter()
                .find(|task| task.line == line)
                .and_then(|task| task.fields.get("thread"))
        })
        .or_else(|| note.fields.get("thread"));
    match value {
        Some(RecordValue::String(url)) if !url.is_empty() => Some(url.clone()),
        _ => None,
    }
}

fn build_row(
    section: &Section,
    row: &Row,
    projection: &Projection,
    tasks: Option<&Arc<TasksIndex>>,
) -> ActionRow {
    let at = section
        .at
        .as_deref()
        .filter(|_| section.show == Show::Timeline);
    let mut values: Vec<String> = section
        .cols
        .iter()
        .filter(|key| Some(key.as_str()) != at)
        .filter_map(|key| row.fields.get(key))
        .map(plain_value)
        .filter(|cell| !cell.is_empty())
        .collect();
    let primary = if values.is_empty() {
        row_source(&row.path, row.line)
    } else {
        values.remove(0)
    };
    let open_task = find_indexed_task(tasks, &row.path, row.line, None);
    let check = row.actions.iter().any(|action| action == "check").then(|| {
        let task = find_indexed_task(tasks, &row.path, row.line, Some(&row.rev));
        let checked = row_checked(projection, row, task.as_ref());
        CheckAction {
            checked,
            schema: projection.schema_arc(),
            index: tasks.cloned(),
            task,
        }
    });
    let thread = row_thread(projection, row);
    ActionRow {
        path: row.path.clone(),
        line: row.line,
        anchor: row.anchor.clone(),
        rev: row.rev.clone(),
        primary,
        meta: values,
        check,
        thread,
        open_task,
        date: at.and_then(|key| timeline::Date::from_value(row.fields.get(key)?)),
    }
}

fn parse_block(
    node: &markdown_ast::Node,
    cx: &gpui_component::text::MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::Code(code) = node else {
        return None;
    };
    let nonce = code.lang.as_deref()?.strip_prefix(ACTION_FENCE_PREFIX)?;
    if nonce.is_empty() {
        return None;
    }
    let index = code.value.trim().parse::<usize>().ok()?;
    let raw = cx.node_source(node).unwrap_or("").to_owned();
    Some(
        MarkdownNode::new(
            "view-action-rows",
            BlockRef {
                nonce: nonce.to_owned(),
                index,
                raw: raw.clone(),
            },
        )
        .text(raw.clone())
        .markdown(raw),
    )
}

pub(super) fn install(view: TextView, reader: WeakEntity<Reader>) -> TextView {
    view.markdown_block_parser(parse_block)
        .markdown_block_renderer("view-action-rows", move |node, _, cx| {
            let Some(reference) = node.data::<BlockRef>() else {
                return div().into_any_element();
            };
            let Some(entity) = reader.upgrade() else {
                return div().into_any_element();
            };
            let (blocks, root) = {
                let this = entity.read(cx);
                let blocks = this
                    .pane
                    .query_navigation
                    .active
                    .borrow()
                    .as_ref()
                    .map(|content| content.actions.clone());
                (blocks, this.vault_root.clone())
            };
            let Some(blocks) = blocks.filter(|blocks| blocks.nonce == reference.nonce) else {
                return div().child(reference.raw.clone()).into_any_element();
            };
            let Some(block) = blocks.items.get(reference.index) else {
                return div().into_any_element();
            };
            render_block(block, &reference.nonce, reference.index, &reader, &root, cx)
        })
}

fn render_block(
    block: &ActionBlock,
    nonce: &str,
    block_ix: usize,
    reader: &WeakEntity<Reader>,
    root: &Path,
    cx: &App,
) -> AnyElement {
    let render = RowRenderer {
        nonce,
        block_ix,
        reader,
        root,
        cx,
    };
    let muted = cx.theme().muted_foreground;
    let mut column = v_flex()
        .id(SharedString::from(format!(
            "view-action-block-{nonce}-{block_ix}"
        )))
        .debug_selector(move || format!("view-action-block-{block_ix}"))
        .w_full()
        .min_w_0()
        .gap_1()
        .py_1();
    let display = match block.show {
        Show::Board => Some(board::render(&block.groups, &block.rows, &render).into_any_element()),
        Show::Cards => Some(cards::render(&block.groups, &block.rows, &render).into_any_element()),
        Show::Timeline => {
            Some(timeline::render(&block.groups, &block.rows, &render).into_any_element())
        }
        _ => None,
    };
    if let Some(display) = display {
        return column.child(display).into_any_element();
    }
    if block.show != Show::List && !block.cols.is_empty() {
        column = column.child(
            h_flex()
                .w_full()
                .gap_2()
                .pb_1()
                .border_b_1()
                .border_color(cx.theme().border)
                .text_xs()
                .text_color(muted)
                .children(block.cols.iter().enumerate().map(|(ix, col)| {
                    if ix == 0 {
                        div().flex_1().min_w_0().child(col.clone())
                    } else {
                        div().flex_shrink_0().px_2().child(col.clone())
                    }
                })),
        );
    }
    column
        .children(
            block
                .rows
                .iter()
                .enumerate()
                .map(|(row_ix, row)| render.row(row_ix, row, false)),
        )
        .into_any_element()
}

pub(super) struct RowRenderer<'a> {
    nonce: &'a str,
    block_ix: usize,
    reader: &'a WeakEntity<Reader>,
    root: &'a Path,
    pub(super) cx: &'a App,
}

impl RowRenderer<'_> {
    pub(super) fn row(&self, row_ix: usize, row: &ActionRow, stacked: bool) -> AnyElement {
        let Self {
            nonce,
            block_ix,
            reader,
            root,
            cx,
        } = *self;
        let muted = cx.theme().muted_foreground;
        let open_reader = reader.clone();
        let open_root = root.to_path_buf();
        let open_path = row.path.clone();
        let open_line = row.line;
        let open_anchor = row.anchor.clone();
        let open_task = row.open_task.clone();
        let open_tooltip = format!("Open {} — {}", row_source(&row.path, row.line), row.primary);
        let mut line = h_flex()
            .id(SharedString::from(format!(
                "view-action-row-{nonce}-{block_ix}-{row_ix}"
            )))
            .debug_selector(move || format!("view-action-row-{block_ix}-{row_ix}"))
            .w_full()
            .min_w_0()
            .min_h(px(30.))
            .gap_2()
            .items_center();

        if let Some(check) = &row.check {
            let check_reader = reader.clone();
            let check_root = root.to_path_buf();
            let check_index = check.index.clone();
            let check_task = check.task.clone();
            let check_rev = row.rev.clone();
            let schema = check.schema.clone();
            let checked = check.checked;
            line = line.child(
                div()
                    .flex_shrink_0()
                    .debug_selector(move || format!("view-action-check-{block_ix}-{row_ix}"))
                    .child(
                        Checkbox::new(SharedString::from(format!(
                            "view-check-{nonce}-{block_ix}-{row_ix}"
                        )))
                        .checked(checked)
                        .accessibility_label(row.primary.clone())
                        .tooltip(if checked {
                            "Reopen task"
                        } else {
                            "Complete task"
                        })
                        .on_click(move |next, window, cx| {
                            let _ = check_reader.update(cx, |this, cx| {
                                if this.vault_root != check_root {
                                    return;
                                }
                                let Some((index, task)) =
                                    check_index.clone().zip(check_task.clone())
                                else {
                                    reader_toast::error(
                                        "Task unchanged: The note changed, refresh.",
                                        window,
                                        cx,
                                    );
                                    return;
                                };
                                this.apply_view_task_check(
                                    index,
                                    task,
                                    &check_rev,
                                    *next,
                                    schema.today_now(),
                                    window,
                                    cx,
                                );
                            });
                        }),
                    ),
            );
        }

        line = line
            .child(
                Button::new(SharedString::from(format!(
                    "view-open-{nonce}-{block_ix}-{row_ix}"
                )))
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
                .tooltip(open_tooltip)
                .accessibility_label(row.primary.clone())
                .debug_selector(move || format!("view-action-open-{block_ix}-{row_ix}"))
                .on_click(move |_, window, cx| {
                    let _ = open_reader.update(cx, |this, cx| {
                        if this.vault_root != open_root {
                            return;
                        }
                        if let Some(task) = &open_task {
                            this.prepare_task_document(task, window, cx);
                        } else if open_anchor.is_some() {
                            this.open_note_at(&open_path, None, open_anchor.as_deref(), window, cx);
                        } else if let Some(line_no) = open_line {
                            this.prepare_document_at_place(
                                &open_path,
                                Some(line_no as u32),
                                window,
                                cx,
                            );
                        } else {
                            this.open_note_at(&open_path, None, None, window, cx);
                        }
                    });
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .line_height(ROW_LABEL_LINE)
                        .when(stacked, |title| title.font_weight(FontWeight::SEMIBOLD))
                        .truncate()
                        .child(row.primary.clone()),
                ),
            )
            .when(!stacked, |line| {
                line.children(row.meta.iter().map(|cell| {
                    div()
                        .flex_shrink_0()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .text_xs()
                        .text_color(muted)
                        .bg(muted.opacity(0.08))
                        .child(cell.clone())
                }))
            });

        if let Some(thread_url) = &row.thread {
            let thread_reader = reader.clone();
            let thread_url = thread_url.clone();
            line = line.child(
                Button::new(SharedString::from(format!(
                    "view-thread-{nonce}-{block_ix}-{row_ix}"
                )))
                .small()
                .ghost()
                .flex_shrink_0()
                .label("Open in T3")
                .tooltip(format!("Open in T3 ({thread_url})"))
                .debug_selector(move || format!("view-action-thread-{block_ix}-{row_ix}"))
                .on_click(move |_, window, cx| {
                    let _ = thread_reader.update(cx, |this, cx| {
                        if let Err(error) = crate::project::desktop::open_thread_url(
                            &thread_url,
                            this.session_directory.as_deref(),
                            cx,
                        ) {
                            reader_toast::error(
                                format!("Could not open thread in T3: {error}"),
                                window,
                                cx,
                            );
                        }
                    });
                }),
            );
        }

        if stacked && !row.meta.is_empty() {
            v_flex()
                .w_full()
                .min_w_0()
                .child(line)
                .children(row.meta.iter().map(|cell| {
                    div()
                        .text_xs()
                        .text_color(muted)
                        .whitespace_normal()
                        .child(cell.clone())
                }))
                .into_any_element()
        } else {
            line.into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{TestAppContext, VisualTestContext};

    const FIXTURE_VIEWS: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../okilum-core/tests/fixtures/views"
    );

    fn copy_tree(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let target = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn mount<'a>(
        cx: &'a mut TestAppContext,
        temp: &tempfile::TempDir,
        open: &str,
    ) -> (Entity<Reader>, PathBuf, &'a mut VisualTestContext) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
            bind_keys(cx);
        });
        let vault = temp.path().join("vault");
        copy_tree(Path::new(FIXTURE_VIEWS), &vault);
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some(open.into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        panel_settings_override: Some(temp.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(1400.), px(960.)));
        (entity.unwrap(), vault, visual)
    }

    fn wait_for_bounds(visual: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
        for _ in 0..40 {
            visual.run_until_parked();
            if let Some(bounds) = visual.debug_bounds(selector) {
                return bounds;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("expected debug bounds for {selector}");
    }

    fn click_undo(visual: &mut VisualTestContext) {
        std::thread::sleep(Duration::from_millis(450));
        visual.update(|window, _| window.refresh());
        visual.executor().advance_clock(Duration::from_millis(400));
        visual.run_until_parked();
        let undo = wait_for_bounds(visual, "task-undo");
        visual.simulate_mouse_move(undo.center(), None, Modifiers::default());
        visual.simulate_click(undo.center(), Modifiers::default());
        visual.run_until_parked();
    }

    #[cfg(unix)]
    #[gpui::test]
    fn view_check_preserves_bom_crlf_trailing_spaces_shows_undo_and_restores_exact_bytes(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        copy_tree(Path::new(FIXTURE_VIEWS), &vault);
        let questions_path = vault.join("state/dev/okilum/questions.md");
        let original = std::fs::read_to_string(&questions_path).unwrap();
        let with_bom_crlf = format!(
            "\u{feff}{}",
            original
                .replace('\n', "\r\n")
                .replace("📅 2026-11-26\r\n", "📅 2026-11-26  \r\n")
        );
        std::fs::write(&questions_path, &with_bom_crlf).unwrap();

        cx.executor().allow_parking();
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
            bind_keys(cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("@views/questions.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        panel_settings_override: Some(temp.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let _reader = entity.unwrap();
        visual.simulate_resize(size(px(1400.), px(960.)));
        let check = wait_for_bounds(visual, "view-action-check-0-0");
        assert!(
            visual.debug_bounds("view-action-check-0-1").is_some(),
            "two rows in okilum group initially"
        );
        visual.simulate_click(check.center(), Modifiers::default());
        for _ in 0..40 {
            visual.run_until_parked();
            if visual.debug_bounds("view-action-check-0-1").is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            visual.debug_bounds("view-action-check-0-0").is_some(),
            "remaining unchecked row stays interactive"
        );
        assert!(
            visual.debug_bounds("view-action-check-0-1").is_none(),
            "checking removes the completed row from the unchecked view"
        );
        let schema = FieldSchema::load(&vault).unwrap();
        let today = schema.today_now();
        let done_str = format!(
            "{:04}-{:02}-{:02}",
            today.year(),
            u8::from(today.month()),
            today.day()
        );
        let expected = with_bom_crlf.replacen(
            "- [ ] Approve `@views/out/` as the only snapshot folder [owner:: oleg] [default:: approve] [thread:: t3code://threads/env-fixture/thread-views] ➕ 2026-11-10 📅 2026-11-26  \r\n",
            &format!(
                "- [x] Approve `@views/out/` as the only snapshot folder [owner:: oleg] [default:: approve] [thread:: t3code://threads/env-fixture/thread-views] ➕ 2026-11-10 📅 2026-11-26 [done:: {done_str}]  \r\n"
            ),
            1,
        );
        assert_eq!(
            std::fs::read(&questions_path).unwrap(),
            expected.as_bytes(),
            "only the checkbox char and inline done date changed; BOM, CRLF and trailing spaces stayed intact"
        );

        // Undo restores the exact previous bytes and brings the row back.
        click_undo(visual);
        for _ in 0..40 {
            visual.run_until_parked();
            if visual.debug_bounds("view-action-check-0-1").is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            std::fs::read(&questions_path).unwrap(),
            with_bom_crlf.as_bytes(),
            "Undo restores exact previous bytes"
        );
        assert!(
            visual.debug_bounds("view-action-check-0-1").is_some(),
            "Undo brings the checked row back into the view"
        );
    }

    #[cfg(unix)]
    #[gpui::test]
    fn view_open_thread_visibility_dispatch_and_stale_revision_refusal(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let (_reader, vault, visual) = mount(cx, &temp, "@views/questions.md");
        let t3 = wait_for_bounds(visual, "view-action-thread-0-0");
        assert!(
            visual.debug_bounds("view-action-thread-0-1").is_none(),
            "second okilum row has no thread"
        );
        assert!(
            visual.debug_bounds("view-action-thread-1-0").is_none(),
            "tasker row has no thread"
        );
        let questions_path = vault.join("state/dev/okilum/questions.md");
        let before = std::fs::read(&questions_path).unwrap();
        visual.simulate_click(t3.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            visual.opened_url().as_deref(),
            Some("t3code://threads/env-fixture/thread-views")
        );
        assert_eq!(
            std::fs::read(&questions_path).unwrap(),
            before,
            "open: thread is navigation only"
        );

        // External write makes the displayed revision stale: click must refuse,
        // leave the file & row unchanged, and show an error toast.
        let external = format!("{}\nExternal edit\n", String::from_utf8(before).unwrap());
        std::fs::write(&questions_path, &external).unwrap();
        let check = wait_for_bounds(visual, "view-action-check-0-0");
        visual.simulate_click(check.center(), Modifiers::default());
        let error_toast = wait_for_bounds(visual, "reader-error-toast");
        assert!(error_toast.size.width > px(0.));
        assert_eq!(
            std::fs::read(&questions_path).unwrap(),
            external.as_bytes(),
            "stale revision refuses without writing"
        );
        assert!(
            visual.debug_bounds("view-action-check-0-1").is_some(),
            "failed write leaves the view rows unchanged"
        );
    }

    #[test]
    fn row_thread_prefers_task_thread_and_falls_back_to_note_thread() {
        use okilum_core::{
            records::{FieldSchema, Source},
            view::{self, RunContext},
            Vault,
        };
        let schema = Arc::new(FieldSchema::load(Path::new(FIXTURE_VIEWS)).unwrap());
        let note = concat!(
            "---\n",
            "type: questions\n",
            "area: Dev\n",
            "updated: 2026-11-24\n",
            "thread: t3code://threads/env-note/thread-parent\n",
            "---\n",
            "# Questions\n",
            "- [ ] Own thread [owner:: oleg] [thread:: t3code://threads/env-row/thread-child]\n",
            "- [ ] Inherited thread [owner:: oleg]\n",
        );
        let plain_note = concat!(
            "---\n",
            "type: questions\n",
            "area: Dev\n",
            "updated: 2026-11-24\n",
            "---\n",
            "# Plain\n",
            "- [ ] No thread [owner:: oleg]\n",
        );
        let projection = Projection::build(
            schema,
            [
                Source {
                    path: "state/dev/okilum/a.md",
                    bytes: note.as_bytes(),
                    mtime: None,
                },
                Source {
                    path: "state/dev/okilum/b.md",
                    bytes: plain_note.as_bytes(),
                    mtime: None,
                },
            ],
        );
        let vault = Vault::from_note_paths([
            "state/dev/okilum/a.md".into(),
            "state/dev/okilum/b.md".into(),
        ]);
        let view_doc = view::parse(
            "@views/t.md",
            "---\ntype: view\n---\n```view\nfrom: tasks\nsort: [path, line]\nshow: list\nactions: [check, {open: thread}]\n```\n",
        )
        .unwrap();
        let ctx = RunContext::new(
            &projection,
            &vault,
            chrono::DateTime::parse_from_rfc3339("2026-11-24T10:00:00+02:00").unwrap(),
        );
        let result = view::run(&view_doc, &ctx);
        let rows = &result.sections[0].rows;
        assert_eq!(rows.len(), 3);
        assert_eq!(
            row_thread(&projection, &rows[0]).as_deref(),
            Some("t3code://threads/env-row/thread-child")
        );
        assert_eq!(
            row_thread(&projection, &rows[1]).as_deref(),
            Some("t3code://threads/env-note/thread-parent")
        );
        assert_eq!(row_thread(&projection, &rows[2]), None);
    }
}
