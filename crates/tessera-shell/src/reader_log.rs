//! Read-only structured log view (#602): virtualized one-line rows over a
//! `tessera_core::log` index, a detail pane for the selected record, and a
//! raw-line copy. Filters, query and follow mode are later slices.
use super::*;
use gpui_component::scroll::ScrollableElement as _;
use std::ops::Range;
use tessera_core::log::{timestamp, Format, Level, LogFile, Record, Role, ValueKind};

const CONTEXT: &str = "ReaderLog";
const ROW_HEIGHT: f32 = 24.;
const DETAIL_HEIGHT: f32 = 240.;
const KEY_WIDTH: f32 = 168.;
/// Characters of a row shaped per frame; longer rows end in an ellipsis and
/// stay complete in the detail pane and the raw copy.
const ROW_TEXT_LIMIT: usize = 480;
const COPY_KEY: &str = "secondary-c";

actions!(
    reader_log,
    [SelectPreviousRecord, SelectNextRecord, CopyRawLine]
);

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPreviousRecord, Some(CONTEXT)),
        KeyBinding::new("down", SelectNextRecord, Some(CONTEXT)),
        KeyBinding::new(COPY_KEY, CopyRawLine, Some(CONTEXT)),
    ]);
}

/// An indexed log, ready to show. `rel` is relative to the Reader root.
#[derive(Clone)]
pub(crate) struct LogDocument {
    pub rel: String,
    pub file: Arc<LogFile>,
    pub elapsed: Duration,
}

enum State {
    Indexing,
    Ready(LogDocument),
    Failed(String),
}

pub(crate) struct LogView {
    state: State,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _indexing: Option<Task<()>>,
}

impl LogView {
    pub(crate) fn ready(document: LogDocument, cx: &mut Context<Self>) -> Self {
        record_ready(&document, cx);
        Self {
            state: State::Ready(document),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _indexing: None,
        }
    }

    /// Indexes `path` off the UI thread; the view says so meanwhile.
    pub(crate) fn indexing(rel: String, path: PathBuf, cx: &mut Context<Self>) -> Self {
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let started = std::time::Instant::now();
                    LogFile::open(&path).map(|file| (file, started.elapsed()))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.state = match result {
                    Ok((file, elapsed)) => State::Ready(LogDocument {
                        rel,
                        file: Arc::new(file),
                        elapsed,
                    }),
                    Err(error) => State::Failed(format!("{error:#}")),
                };
                if let State::Ready(document) = &this.state {
                    record_ready(document, cx);
                }
                cx.notify();
            });
        });
        Self {
            state: State::Indexing,
            selected: None,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _indexing: Some(task),
        }
    }

    pub(crate) fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn file(&self) -> Option<&Arc<LogFile>> {
        match &self.state {
            State::Ready(document) => Some(&document.file),
            _ => None,
        }
    }

    fn select(&mut self, index: usize, reveal: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(index);
        if reveal {
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(count) = self.file().map(|file| file.index().len()) else {
            return;
        };
        if count == 0 {
            return;
        }
        let next = match self.selected {
            Some(index) => (index as isize + delta).clamp(0, count as isize - 1) as usize,
            None if delta < 0 => count - 1,
            None => 0,
        };
        self.select(next, true, window, cx);
    }

    fn copy_raw(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(raw) = self
            .selected
            .and_then(|index| self.file().and_then(|file| file.raw(index)))
        else {
            return;
        };
        let text = String::from_utf8_lossy(raw).into_owned();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        reader_toast::transient("Copied raw line", window, cx);
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(file) = self.file().cloned() else {
            return Vec::new();
        };
        let palette = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        let hover = brand::reader_palette(cx).hover;
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        range
            .map(|index| {
                let Some(entry) = file.index().get(index) else {
                    return div().into_any_element();
                };
                let level = entry.level();
                let row = row_text(&file, index);
                let selected = self.selected == Some(index);
                let text_color = if level == Level::Unparsed {
                    faint
                } else {
                    palette.text
                };
                let highlights = row
                    .muted
                    .iter()
                    .map(|range| {
                        (
                            range.clone(),
                            HighlightStyle {
                                color: Some(muted),
                                ..Default::default()
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                h_flex()
                    .id(("log-row", index))
                    .h(px(ROW_HEIGHT))
                    .w_full()
                    .min_w_0()
                    .pl_3()
                    .pr_4()
                    .gap_3()
                    .text_sm()
                    .cursor_pointer()
                    .border_l_2()
                    .border_color(if matches!(level, Level::Error | Level::Fatal) {
                        palette.danger
                    } else {
                        transparent_black()
                    })
                    .when(selected, |row| row.bg(palette.selected))
                    .when(!selected, |row| row.hover(move |style| style.bg(hover)))
                    .child(
                        div()
                            .flex_none()
                            .w(px(92.))
                            .font_family(mono.clone())
                            .text_size(px(12.))
                            .text_color(muted)
                            .child(row.time),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(28.))
                            .font_family(mono.clone())
                            .text_size(px(12.))
                            .text_color(level_color(level, cx))
                            .when(level == Level::Fatal, |badge| {
                                badge.font_weight(FontWeight::BOLD)
                            })
                            .child(level.badge()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(text_color)
                            .when(level == Level::Unparsed, |text| {
                                text.font_family(mono.clone())
                            })
                            .child(StyledText::new(row.text).with_highlights(highlights)),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.select(index, false, window, cx)
                        }),
                    )
                    .into_any_element()
            })
            .collect()
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = brand::palette(cx);
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let pane = v_flex()
            .flex_none()
            .h(px(DETAIL_HEIGHT))
            .border_t_1()
            .border_color(palette.border_subtle);
        let (Some(file), Some(index)) = (self.file(), self.selected) else {
            return pane
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child(
                    if self.file().is_some_and(|file| !file.index().is_empty()) {
                        "Select a record to see its fields"
                    } else {
                        ""
                    },
                )
                .into_any_element();
        };
        let Some(entry) = file.index().get(index) else {
            return pane.into_any_element();
        };
        let header = h_flex()
            .flex_none()
            .h(px(36.))
            .px_3()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(muted)
                    .child(format!(
                        "Record {} of {} · line {}",
                        grouped(index as u64 + 1),
                        grouped(file.index().len() as u64),
                        grouped(entry.line())
                    )),
            )
            .child(
                reader_icon_button(
                    "log-copy-raw",
                    IconName::Copy,
                    with_shortcut("Copy raw line", COPY_KEY),
                    cx,
                )
                .debug_selector(|| "log-copy-raw".into())
                .on_click(cx.listener(|this, _, window, cx| this.copy_raw(window, cx))),
            );
        let body = div()
            .id("log-detail-fields")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_3()
            .pb_3()
            .text_sm();
        let body = match file.record(index) {
            Some(record) => body.children(detail_rows(&record, entry.timestamp()).into_iter().map(
                |(key, value, kind)| {
                    h_flex()
                        .items_start()
                        .gap_3()
                        .py(px(2.))
                        .child(
                            div()
                                .flex_none()
                                .w(px(KEY_WIDTH))
                                .truncate()
                                .font_family(mono.clone())
                                .text_size(px(12.))
                                .text_color(muted)
                                .child(key),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .when(kind != ValueKind::String, |value| {
                                    value.font_family(mono.clone()).text_size(px(12.))
                                })
                                .child(value),
                        )
                },
            )),
            None => body
                .child(
                    div()
                        .text_color(muted)
                        .pb_1()
                        .child(match file.index().format() {
                            Format::Plain => "Plain text line".to_owned(),
                            format => format!("Not a {} record", format.label()),
                        }),
                )
                .child(div().font_family(mono).text_size(px(12.)).child(
                    String::from_utf8_lossy(file.raw(index).unwrap_or_default()).into_owned(),
                )),
        };
        pane.child(header).child(body).into_any_element()
    }

    fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = match &self.state {
            State::Indexing => "Indexing…".to_owned(),
            State::Failed(error) => format!("Cannot show this log: {error}"),
            State::Ready(document) => status_text(document),
        };
        h_flex()
            .flex_none()
            .h(px(28.))
            .px_3()
            .border_t_1()
            .border_color(brand::palette(cx).border_subtle)
            .text_size(px(12.))
            .text_color(cx.theme().muted_foreground)
            .child(div().truncate().child(text))
            .into_any_element()
    }
}

impl Render for LogView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.file().map_or(0, |file| file.index().len());
        let list = div().relative().flex_1().min_h_0().w_full();
        let list = if count > 0 {
            list.child(
                uniform_list(
                    "reader-log-rows",
                    count,
                    cx.processor(|this, range: Range<usize>, window, cx| {
                        this.render_rows(range, window, cx)
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full(),
            )
            .vertical_scrollbar(&self.scroll)
        } else {
            let message = match &self.state {
                State::Ready(_) => "This log is empty",
                State::Indexing | State::Failed(_) => "",
            };
            list.flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(message)
        };
        v_flex()
            .id("reader-log")
            .debug_selector(|| "reader-log".into())
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .on_action(
                cx.listener(|this, _: &SelectNextRecord, window, cx| this.step(1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectPreviousRecord, window, cx| this.step(-1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &CopyRawLine, window, cx| this.copy_raw(window, cx)))
            .child(list)
            .child(self.render_detail(cx))
            .child(self.render_status(cx))
    }
}

/// Keep indexing performance in diagnostics instead of the document chrome.
fn record_ready(document: &LogDocument, cx: &App) {
    if let Some(trace) = reader_diagnostics::trace(cx) {
        trace.event(
            "LOG_READY",
            serde_json::json!({
                "path": document.rel,
                "records": document.file.index().len(),
                "duration_ms": document.elapsed.as_secs_f64() * 1000.,
            }),
        );
    }
}

/// Level colors come from the semantic tokens, never literals.
fn level_color(level: Level, cx: &App) -> Hsla {
    let palette = brand::palette(cx);
    match level {
        Level::Trace | Level::Unparsed => brand::reader_palette(cx).text_faint,
        Level::Debug | Level::Unknown | Level::Missing => cx.theme().muted_foreground,
        Level::Info => palette.info,
        Level::Warn => palette.warning,
        Level::Error | Level::Fatal => palette.danger,
    }
}

/// One row's text and the byte ranges drawn muted (logger, other fields).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RowText {
    pub time: String,
    pub text: String,
    pub muted: Vec<Range<usize>>,
}

pub(crate) fn row_text(file: &LogFile, index: usize) -> RowText {
    let time = file
        .index()
        .get(index)
        .and_then(|entry| entry.timestamp())
        .map(timestamp::clock_utc)
        .unwrap_or_default();
    let mut row = RowBuilder::default();
    match file.record(index) {
        Some(record) => {
            if let Some(logger) = record.role(Role::Logger) {
                row.push(&logger.value, true);
            }
            if let Some(message) = record.role(Role::Message) {
                row.push(&message.value, false);
            }
            for field in record.rest() {
                if row.full() {
                    break;
                }
                row.push(&format!("{}={}", field.key, field.value), true);
            }
        }
        None => row.push(
            &String::from_utf8_lossy(file.raw(index).unwrap_or_default()),
            false,
        ),
    }
    RowText {
        time,
        text: row.text,
        muted: row.muted,
    }
}

#[derive(Default)]
struct RowBuilder {
    text: String,
    muted: Vec<Range<usize>>,
    chars: usize,
}

impl RowBuilder {
    fn full(&self) -> bool {
        self.chars >= ROW_TEXT_LIMIT
    }

    /// Appends one segment on the single row line: control characters become
    /// spaces and the row stops at the character limit with an ellipsis.
    fn push(&mut self, segment: &str, muted: bool) {
        if self.full() || segment.is_empty() {
            return;
        }
        if !self.text.is_empty() {
            self.text.push_str("  ");
            self.chars += 2;
        }
        let start = self.text.len();
        for c in segment.chars() {
            if self.chars == ROW_TEXT_LIMIT {
                self.text.push('…');
                break;
            }
            self.text.push(if c.is_control() { ' ' } else { c });
            self.chars += 1;
        }
        if muted {
            self.muted.push(start..self.text.len());
        }
    }
}

/// Detail pane rows: every field in source order, with the parsed UTC time
/// added beside an epoch number so the raw value stays as written.
fn detail_rows(record: &Record, timestamp: Option<i64>) -> Vec<(String, String, ValueKind)> {
    let time_field = record.time;
    record
        .fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let mut value = field.value.clone();
            if Some(index) == time_field && field.kind == ValueKind::Number {
                if let Some(nanos) = timestamp {
                    value = format!("{value} · {}", timestamp::datetime_utc(nanos));
                }
            }
            (field.key.clone(), value, field.kind)
        })
        .collect()
}

/// Facts about the file. Zero counts are left out rather than shown.
fn status_text(document: &LogDocument) -> String {
    let index = document.file.index();
    let stats = index.stats();
    let format = index.format();
    let mut parts = vec![format.label().to_owned()];
    match index.len() {
        0 => {}
        1 => parts.push("1 record".into()),
        n => parts.push(format!("{} records", grouped(n as u64))),
    }
    let unparsed = stats.unparsed();
    if format != Format::Plain && unparsed > 0 {
        parts.push(match unparsed {
            1 => "1 unparsed line".to_owned(),
            n => format!("{} unparsed lines", grouped(n)),
        });
    }
    if format != Format::Plain && !index.is_empty() && !index.has_level_field() {
        parts.push("No record has a level".into());
    }
    if stats.timestamped > 0 {
        parts.push("Times in UTC".into());
    }
    parts.push(size(index.bytes()));
    parts.join(" · ")
}

fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1_000_000_000 => format!("{:.1} GB", b as f64 / 1e9),
        b if b >= 1_000_000 => format!("{:.1} MB", b as f64 / 1e6),
        b if b >= 1_000 => format!("{:.0} KB", b as f64 / 1e3),
        1 => "1 byte".into(),
        b => format!("{b} bytes"),
    }
}

impl Reader {
    /// Shows an already indexed log as the quick viewer's selection.
    pub(crate) fn mount_log(
        &mut self,
        document: LogDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rel = document.rel.clone();
        let mut preview = match reader_files::FilePreview::load(&self.vault_root, &rel) {
            Ok(preview) => preview,
            Err(error) => {
                reader_toast::error(format!("Cannot show this log: {error:#}"), window, cx);
                return;
            }
        };
        let view = cx.new(|cx| LogView::ready(document, cx));
        view.read(cx).focus_handle().clone().focus(window, cx);
        preview.log = Some(view);
        self.file_preview = Some(preview);
        self.history = vec![rel.clone()];
        self.history_positions = vec![ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        }];
        self.history_ix = 0;
        self.find_open = false;
        self.link_notice = None;
        window.set_window_title(&format!("Tessera — {rel}"));
        self.record_usable_document(cx);
        cx.notify();
    }

    pub(crate) fn render_log_preview(
        &self,
        view: &Entity<LogView>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .id("reader-log-preview")
            .key_context("ReaderFile")
            .track_focus(&self.focus_handle)
            .size_full()
            .min_h_0()
            .child(self.render_document_header(cx))
            .child(view.clone())
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn log(text: &str) -> (tempfile::TempDir, LogFile) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.jsonl");
        std::fs::write(&path, text).unwrap();
        let file = LogFile::open(&path).unwrap();
        (dir, file)
    }

    #[test]
    fn rows_put_logger_message_then_fields_on_one_line() {
        let (_dir, file) = log(concat!(
            r#"{"ts":"2025-10-01T00:00:00.240Z","level":"error","logger":"vault.scan","msg":"scan\ncomplete","status":500,"user":{"role":"owner"}}"#,
            "\n",
            "panicked at\tmain.rs\n",
            r#"{"time":1759276801,"phase":"warm_cache","details":{"found":false}}"#,
            "\n",
        ));
        let row = row_text(&file, 0);
        assert_eq!(row.time, "00:00:00.240");
        assert_eq!(
            row.text,
            "vault.scan  scan complete  status=500  user.role=owner"
        );
        let muted: Vec<_> = row.muted.iter().map(|r| &row.text[r.clone()]).collect();
        assert_eq!(muted, ["vault.scan", "status=500", "user.role=owner"]);
        let raw = row_text(&file, 1);
        assert_eq!(
            (raw.time.as_str(), raw.text.as_str()),
            ("", "panicked at main.rs")
        );
        assert!(raw.muted.is_empty());
        // Tessera's own diagnostic shape: no message, no level, fields only.
        assert_eq!(
            row_text(&file, 2).text,
            "phase=warm_cache  details.found=false"
        );
    }

    #[test]
    fn long_rows_stop_with_an_ellipsis() {
        let (_dir, file) = log(&format!("{{\"msg\":\"{}\",\"k\":1}}\n", "x".repeat(2_000)));
        let row = row_text(&file, 0);
        assert_eq!(row.text.chars().count(), ROW_TEXT_LIMIT + 1);
        assert!(row.text.ends_with('…'));
    }

    #[test]
    fn status_leaves_out_zero_counts_and_surfaces_missing_levels() {
        let (_dir, file) = log("{\"time\":1759276800,\"phase\":\"a\"}\n");
        let document = LogDocument {
            rel: "app.jsonl".into(),
            file: Arc::new(file),
            elapsed: Duration::from_millis(12),
        };
        let status = status_text(&document);
        assert_eq!(
            status,
            "JSON lines · 1 record · No record has a level · Times in UTC · 32 bytes"
        );
        assert!(!status.contains(" 0 "));
        let (_dir, file) = log("{\"level\":\"info\"}\n{\"a\":1}\n{\"a\":2}\nraw\n");
        let document = LogDocument {
            rel: "app.jsonl".into(),
            file: Arc::new(file),
            elapsed: Duration::ZERO,
        };
        let status = status_text(&document);
        assert!(status.contains("4 records · 1 unparsed line ·"), "{status}");
        assert!(!status.contains("level"), "{status}");
        assert!(!status.contains("UTC"), "{status}");
        assert_eq!(grouped(480_974), "480,974");
        assert_eq!(grouped(12), "12");
        assert_eq!(size(104_857_627), "104.9 MB");
    }

    fn log_view(reader: &Entity<Reader>, visual: &mut VisualTestContext) -> Entity<LogView> {
        reader.read_with(visual, |reader, _| {
            reader.file_preview.as_ref().unwrap().log.clone().unwrap()
        })
    }

    #[gpui::test]
    fn a_log_opens_in_the_quick_viewer_and_copies_its_raw_line(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("logs")).unwrap();
        let root = directory.path().join("logs").canonicalize().unwrap();
        let state = directory.path().join("state");
        let text = "{\"level\":\"info\",\"msg\":\"first\"}\nnot json\n{\"level\":\"error\",\"msg\":\"second\"}\n";
        std::fs::write(root.join("app.jsonl"), text).unwrap();
        std::fs::write(root.join("older.log"), "{\"level\":\"warn\"}\n").unwrap();
        std::fs::write(root.join("notes.md"), "# Notes").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(root.join("app.jsonl")),
                        // An open folder containing the log must not take it
                        // down the Markdown path.
                        reusable_roots: vec![root.clone()],
                        session_directory: Some(state.clone()),
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
        reader.read_with(visual, |reader, _| {
            assert!(reader.single_file && reader.searcher.is_none());
            assert_eq!(reader.selected_file(), "app.jsonl");
            assert_eq!(reader.history, ["app.jsonl"]);
            assert!(reader.vault.entries.iter().any(|e| e.path == "older.log"));
        });
        let view = log_view(&reader, visual);
        view.read_with(visual, |view, _| {
            assert_eq!(view.file().unwrap().index().len(), 3);
            assert_eq!(view.selected, None);
        });
        // Keyboard bindings reach the focused view.
        visual.simulate_keystrokes("down down");
        view.read_with(visual, |view, _| assert_eq!(view.selected, Some(1)));
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-c"
        } else {
            "ctrl-c"
        });
        reader.update_in(visual, |_, _, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "not json"
            );
        });
        assert_eq!(
            crate::reader_history::ReadingHistory::quick_document(&state, &root)
                .unwrap()
                .as_deref(),
            Some("app.jsonl"),
            "the log is what a restart reopens"
        );
        // A sibling log in the quick viewer is indexed off the UI thread.
        reader.update_in(visual, |reader, window, cx| {
            reader.open_note("older.log", None, window, cx)
        });
        visual.run_until_parked();
        let sibling = log_view(&reader, visual);
        sibling.read_with(visual, |view, _| {
            assert_eq!(view.file().unwrap().index().len(), 1);
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.open_note("notes.md", None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.file_preview.is_none());
            assert_eq!(reader.current_rel, "notes.md");
        });
        assert_eq!(
            std::fs::read_to_string(root.join("app.jsonl")).unwrap(),
            text
        );
        assert!(!root.join(".tessera-index").exists());
    }

    #[test]
    fn epoch_time_shows_its_utc_date_beside_the_raw_value() {
        let record = Record::parse(br#"{"time":1759276800,"x":"1"}"#, Format::JsonLines).unwrap();
        let rows = detail_rows(&record, Some(1_759_276_800_000_000_000));
        assert_eq!(rows[0].1, "1759276800 · 2025-10-01 00:00:00.000 UTC");
        assert_eq!(rows[1].1, "1");
    }
}
