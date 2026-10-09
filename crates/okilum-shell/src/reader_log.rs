//! Read-only structured log view (#602): virtualized one-line rows over a
//! `okilum_core::log` index, a filter bar (level, text, time window, field
//! chips) evaluated off the UI thread, a detail pane for the selected record,
//! and a raw-line copy. Query and follow mode are later slices.
use super::*;
use gpui_component::scroll::ScrollableElement as _;
use okilum_core::log::{
    filter, timestamp, FieldChip, FilterGenerations, Format, Level, LogFile, LogFilter, Record,
    Role, TimeWindow, ValueKind,
};
use std::ops::Range;

const CONTEXT: &str = "ReaderLog";
const ROW_HEIGHT: f32 = 24.;
const DETAIL_HEIGHT: f32 = 240.;
const KEY_WIDTH: f32 = 168.;
/// Characters of a row shaped per frame; longer rows end in an ellipsis and
/// stay complete in the detail pane and the raw copy.
const ROW_TEXT_LIMIT: usize = 480;
const COPY_KEY: &str = "secondary-c";
const FILTER_KEY: &str = "secondary-f";
/// Filter bar controls follow the Reader's 28 px icon-button geometry.
const BAR_CONTROL_HEIGHT: f32 = 28.;
const BAR_HEIGHT: f32 = 40.;
const TEXT_FILTER_WIDTH: f32 = 220.;
const CHIP_MAX_WIDTH: f32 = 240.;
/// Level menu choices: the lowest severity shown.
const LEVEL_CHOICES: [(Option<Level>, &str); 6] = [
    (None, "All levels"),
    (Some(Level::Debug), "Debug and above"),
    (Some(Level::Info), "Info and above"),
    (Some(Level::Warn), "Warning and above"),
    (Some(Level::Error), "Error and above"),
    (Some(Level::Fatal), "Fatal only"),
];

actions!(
    reader_log,
    [
        SelectPreviousRecord,
        SelectNextRecord,
        CopyRawLine,
        FocusLogFilter
    ]
);

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPreviousRecord, Some(CONTEXT)),
        KeyBinding::new("down", SelectNextRecord, Some(CONTEXT)),
        KeyBinding::new(COPY_KEY, CopyRawLine, Some(CONTEXT)),
        KeyBinding::new(FILTER_KEY, FocusLogFilter, Some(CONTEXT)),
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
    /// Entry index (into the log index) of the selected record.
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    filter: LogFilter,
    generations: FilterGenerations,
    /// Entry indices shown, ascending; `None` while every entry is shown.
    rows: Option<Arc<Vec<u32>>>,
    /// A run for the latest filter is under way; the previous rows stay up.
    filtering: bool,
    text_input: Entity<InputState>,
    _text_input_events: Subscription,
    _filtering: Option<Task<()>>,
    _indexing: Option<Task<()>>,
}

impl LogView {
    fn new(state: State, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter text…"));
        let events = cx.subscribe_in(
            &text_input,
            window,
            |this: &mut Self, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    this.set_text(text, cx);
                }
                InputEvent::PressEnter { .. } => this.focus.focus(window, cx),
                _ => {}
            },
        );
        Self {
            state,
            selected: None,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            filter: LogFilter::default(),
            generations: FilterGenerations::default(),
            rows: None,
            filtering: false,
            text_input,
            _text_input_events: events,
            _filtering: None,
            _indexing: None,
        }
    }

    pub(crate) fn ready(
        document: LogDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        record_ready(&document, cx);
        Self::new(State::Ready(document), window, cx)
    }

    /// Indexes `path` off the UI thread; the view says so meanwhile.
    pub(crate) fn indexing(
        rel: String,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
            _indexing: Some(task),
            ..Self::new(State::Indexing, window, cx)
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

    /// Rows currently listed.
    fn visible_len(&self) -> usize {
        match &self.rows {
            Some(rows) => rows.len(),
            None => self.file().map_or(0, |file| file.index().len()),
        }
    }

    /// The entry shown at list position `row`.
    fn entry_at(&self, row: usize) -> Option<usize> {
        match &self.rows {
            Some(rows) => rows.get(row).map(|entry| *entry as usize),
            None => (row < self.visible_len()).then_some(row),
        }
    }

    /// The list position of entry `index`, if it is listed.
    fn row_of(&self, index: usize) -> Option<usize> {
        match &self.rows {
            Some(rows) => rows.binary_search(&(index as u32)).ok(),
            None => (index < self.visible_len()).then_some(index),
        }
    }

    fn select(&mut self, index: usize, reveal: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(index);
        if let Some(row) = self.row_of(index).filter(|_| reveal) {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.visible_len();
        if count == 0 {
            return;
        }
        let next = match self.selected.and_then(|index| self.row_of(index)) {
            Some(row) => (row as isize + delta).clamp(0, count as isize - 1) as usize,
            None if delta < 0 => count - 1,
            None => 0,
        };
        if let Some(index) = self.entry_at(next) {
            self.select(index, true, window, cx);
        }
    }

    fn update_filter(&mut self, change: impl FnOnce(&mut LogFilter), cx: &mut Context<Self>) {
        let before = self.filter.clone();
        change(&mut self.filter);
        if self.filter != before {
            self.refilter(cx);
        }
    }

    fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.update_filter(|filter| filter.text = text, cx);
    }

    fn add_chip(&mut self, chip: FieldChip, cx: &mut Context<Self>) {
        self.update_filter(|filter| _ = filter.add_chip(chip), cx);
    }

    fn remove_chip(&mut self, index: usize, cx: &mut Context<Self>) {
        self.update_filter(|filter| _ = filter.remove_chip(index), cx);
    }

    fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.text_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.update_filter(|filter| *filter = LogFilter::default(), cx);
    }

    /// Starts a run for the current filter. Every earlier run is cancelled by
    /// the new generation, and a result that arrives late is dropped.
    fn refilter(&mut self, cx: &mut Context<Self>) {
        let ticket = self.generations.next();
        let Some(file) = self.file().cloned() else {
            return;
        };
        if !self.filter.is_active() {
            self.rows = None;
            self.filtering = false;
            self._filtering = None;
            self.after_rows_changed();
            cx.notify();
            return;
        }
        self.filtering = true;
        let filter = self.filter.clone();
        self._filtering = Some(cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(
                    async move { filter::evaluate(&filter, file.index(), file.bytes(), &ticket) },
                )
                .await;
            let Some(outcome) = outcome else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if !this.generations.is_current(outcome.generation) {
                    return;
                }
                this.rows = Some(Arc::new(outcome.rows));
                this.filtering = false;
                this.after_rows_changed();
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Keeps a selection the new rows still list, in view; otherwise drops
    /// it and returns to the top.
    fn after_rows_changed(&mut self) {
        match self.selected.and_then(|index| self.row_of(index)) {
            Some(row) => self.scroll.scroll_to_item(row, ScrollStrategy::Nearest),
            None => {
                self.selected = None;
                if self.visible_len() > 0 {
                    self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
            }
        }
    }

    fn focus_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file().is_some_and(|file| !file.index().is_empty()) {
            self.text_input
                .update(cx, |input, cx| input.focus(window, cx));
        }
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
            .map(|row| {
                let Some((index, entry)) = self
                    .entry_at(row)
                    .and_then(|index| Some((index, file.index().get(index)?)))
                else {
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
                        grouped(self.row_of(index).unwrap_or(index) as u64 + 1),
                        grouped(self.visible_len() as u64),
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
        let faint = brand::reader_palette(cx).text_faint;
        let body = match file.record(index) {
            Some(record) => body.children(
                detail_rows(&record, entry.timestamp())
                    .into_iter()
                    .zip(&record.fields)
                    .enumerate()
                    .map(|(position, ((key, value, kind), field))| {
                        let group = SharedString::from(format!("log-field-{position}"));
                        let chip = FieldChip {
                            key: field.key.clone(),
                            value: field.value.clone(),
                        };
                        let present = self.filter.chips.contains(&chip);
                        h_flex()
                            .group(group.clone())
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
                            // A fixed slot, shown on hover, so rows never shift.
                            .child(
                                div()
                                    .flex_none()
                                    .opacity(if present { 1. } else { 0. })
                                    .group_hover(group, |s| s.opacity(1.))
                                    .child(
                                        Button::new(("log-add-chip", position))
                                            .ghost()
                                            .xsmall()
                                            .icon(
                                                Icon::new(if present {
                                                    IconName::Check
                                                } else {
                                                    IconName::Plus
                                                })
                                                .text_color(faint),
                                            )
                                            .tooltip(if present {
                                                "Filtered by this value"
                                            } else {
                                                "Show only records with this value"
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.add_chip(chip.clone(), cx)
                                            })),
                                    ),
                            )
                    }),
            ),
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

    /// One compact row above the list: text, level and time menus, chips,
    /// and the match counter. Its height never changes, so filtering never
    /// moves the rows.
    fn render_filter_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let palette = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        let muted = cx.theme().muted_foreground;
        let view = cx.entity().downgrade();
        let level = self.filter.level;
        let time = self.filter.time;
        let timestamped = self
            .file()
            .is_some_and(|file| file.index().stats().timestamped > 0);
        let text = div()
            .flex_none()
            .w(px(TEXT_FILTER_WIDTH))
            .h(px(BAR_CONTROL_HEIGHT))
            .flex()
            .items_center()
            .rounded(px(6.))
            .bg(palette.surface_raised)
            .child(
                Input::new(&self.text_input)
                    .appearance(false)
                    .cleanable(true)
                    .small()
                    .prefix(Icon::new(IconName::Search).small().text_color(muted)),
            );
        let level_menu = {
            let view = view.clone();
            move |menu: gpui_component::menu::PopupMenu, _: &mut Window, _: &mut Context<_>| {
                let mut menu = menu;
                for (min, label) in LEVEL_CHOICES {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(level.min == min)
                            .on_click(move |_, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.update_filter(|filter| filter.level.min = min, cx)
                                });
                            }),
                    );
                }
                let view = view.clone();
                menu.separator().item(
                    PopupMenuItem::new("Include lines without a level")
                        .checked(level.without_level)
                        .on_click(move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.update_filter(
                                    |filter| {
                                        filter.level.without_level = !filter.level.without_level
                                    },
                                    cx,
                                )
                            });
                        }),
                )
            }
        };
        let level_button = bar_button("log-level", level.is_active())
            .label(level_label(level))
            .dropdown_caret(true)
            .tooltip("Levels shown")
            .debug_selector(|| "log-level".into())
            .dropdown_menu_with_anchor(Anchor::TopLeft, level_menu);
        let time_button = timestamped.then(|| {
            let view = view.clone();
            bar_button("log-time", time != TimeWindow::All)
                .icon(Icon::default().path(brand::READER_CLOCK_ICON))
                .label(time.label())
                .dropdown_caret(true)
                .tooltip("Time range, ending at the newest record")
                .debug_selector(|| "log-time".into())
                .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                    let mut menu = menu;
                    for window in TimeWindow::ALL {
                        let view = view.clone();
                        menu = menu.item(
                            PopupMenuItem::new(window.label())
                                .checked(time == window)
                                .on_click(move |_, _, cx| {
                                    let _ = view.update(cx, |this, cx| {
                                        this.update_filter(|filter| filter.time = window, cx)
                                    });
                                }),
                        );
                    }
                    menu
                })
        });
        let chips = h_flex()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .gap_1()
            .children(
                self.filter
                    .chips
                    .iter()
                    .enumerate()
                    .map(|(position, chip)| {
                        h_flex()
                            .id(("log-chip", position))
                            .flex_none()
                            .max_w(px(CHIP_MAX_WIDTH))
                            .h(px(24.))
                            .pl_2()
                            .gap_1()
                            .rounded(px(6.))
                            .bg(palette.selected)
                            .text_size(px(12.))
                            .child(div().min_w_0().truncate().child(chip_label(chip)))
                            .child(
                                Button::new(("log-chip-remove", position))
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::Close).text_color(faint))
                                    .tooltip("Remove filter")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_chip(position, cx)
                                    })),
                            )
                    }),
            );
        let counter = match_counter(
            self.rows.as_ref().map(|rows| rows.len()),
            self.file().map_or(0, |file| file.index().len()),
            self.filtering,
        );
        h_flex()
            .id("log-filter-bar")
            .flex_none()
            .h(px(BAR_HEIGHT))
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(palette.border_subtle)
            .child(text)
            .child(level_button)
            .children(time_button)
            .child(chips)
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(muted)
                    .debug_selector(|| "log-match-counter".into())
                    .child(counter.unwrap_or_default()),
            )
            .child(
                reader_icon_button("log-clear-filters", IconName::CircleX, "Clear filters", cx)
                    .when(!self.filter.is_active(), |button| button.invisible())
                    .on_click(cx.listener(|this, _, window, cx| this.clear_filters(window, cx))),
            )
            .into_any_element()
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
        let count = self.visible_len();
        let has_entries = self.file().is_some_and(|file| !file.index().is_empty());
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
        } else if has_entries {
            // Everything is filtered out: a quiet state with the way back.
            list.child(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.filtering {
                        "Filtering…"
                    } else {
                        "No records match these filters"
                    })
                    .when(!self.filtering, |state| {
                        state.child(
                            Button::new("log-clear-filters-empty")
                                .ghost()
                                .small()
                                .label("Clear filters")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.clear_filters(window, cx)
                                })),
                        )
                    }),
            )
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
            .on_action(
                cx.listener(|this, _: &FocusLogFilter, window, cx| this.focus_filter(window, cx)),
            )
            .when(has_entries, |view| view.child(self.render_filter_bar(cx)))
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

/// A filter bar menu button: Reader icon-button height and radius, quiet at
/// rest, drawn in the accent color while it narrows the list.
fn bar_button(id: &'static str, active: bool) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .h(px(BAR_CONTROL_HEIGHT))
        .rounded(px(6.))
        .selected(active)
}

fn level_label(level: filter::LevelFilter) -> String {
    let base = LEVEL_CHOICES
        .iter()
        .find(|(min, _)| *min == level.min)
        .map_or("All levels", |(_, label)| label);
    match (level.min, level.without_level) {
        (_, true) => base.to_owned(),
        (None, false) => "Records with a level".to_owned(),
        (Some(_), false) => format!("{base}, with a level"),
    }
}

/// `key = value` on one line, cut short for the bar; the chip filters on
/// the whole value.
fn chip_label(chip: &FieldChip) -> String {
    const LIMIT: usize = 80;
    let mut label = format!("{} = ", chip.key);
    for (count, c) in chip.value.chars().enumerate() {
        if count == LIMIT {
            label.push('…');
            break;
        }
        label.push(if c.is_control() { ' ' } else { c });
    }
    label
}

/// "N of M" once a filter has produced rows. Nothing while no filter runs,
/// and nothing for an empty result: the list says that itself.
fn match_counter(matched: Option<usize>, total: usize, filtering: bool) -> Option<String> {
    if filtering {
        return Some("Filtering…".into());
    }
    match matched? {
        0 => None,
        n => Some(format!(
            "{} of {}",
            grouped(n as u64),
            grouped(total as u64)
        )),
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
            for _ in 0..2 {
                if self.full() {
                    self.text.push('…');
                    return;
                }
                self.text.push(' ');
                self.chars += 1;
            }
        }
        let start = self.text.len();
        for c in segment.chars() {
            if self.full() {
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
        let view = cx.new(|cx| LogView::ready(document, window, cx));
        view.read(cx).focus_handle().clone().focus(window, cx);
        preview.log = Some(view);
        self.file_preview = Some(preview);
        self.navigation.history = vec![rel.clone()];
        self.navigation.history_positions = vec![ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        }];
        self.navigation.history_ix = 0;
        self.find_open = false;
        self.link_notice = None;
        window.set_window_title(&format!("Okilum — {rel}"));
        self.record_usable_document(cx);
        cx.notify();
    }

    pub(crate) fn render_log_preview(
        &self,
        view: &Entity<LogView>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .id("reader-log-preview")
            .key_context("ReaderFile")
            .track_focus(&self.focus_handle)
            .size_full()
            .min_h_0()
            .child(self.render_document_header(window, cx))
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
        // Okilum's own diagnostic shape: no message, no level, fields only.
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
    fn field_separators_cannot_bypass_the_row_limit() {
        for prefix_len in [ROW_TEXT_LIMIT - 2, ROW_TEXT_LIMIT - 1] {
            let (_dir, file) = log(&format!(
                "{{\"msg\":\"{}\",\"k\":\"{}\"}}\n",
                "ש".repeat(prefix_len),
                "א".repeat(2_000),
            ));
            let row = row_text(&file, 0);
            assert_eq!(row.text.chars().count(), ROW_TEXT_LIMIT + 1);
            assert!(row.text.ends_with('…'));
            for range in row.muted {
                assert!(row.text.get(range).is_some());
            }
        }
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
    fn vault_tree_logs_publish_records_copy_and_return_to_notes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("start.md"), "# Start").unwrap();
        let text = "{\"level\":\"info\",\"msg\":\"first\"}\nnot json\n{\"level\":\"error\",\"msg\":\"second\"}\n";
        for name in ["events.log", "events.jsonl"] {
            std::fs::write(root.join(name), text).unwrap();
        }
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        index_dir: Some(directory.path().join("index")),
                        session_directory: Some(directory.path().join("state")),
                        panel_settings_override: Some(directory.path().join("panels.json")),
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
        for name in ["events.log", "events.jsonl"] {
            reader.update_in(visual, |reader, window, cx| {
                assert!(!reader.single_file && reader.searcher.is_some());
                let row = reader
                    .tree
                    .rows
                    .iter()
                    .find(|row| row.path == name)
                    .unwrap()
                    .clone();
                assert_eq!(row.kind, okilum_core::vault::EntryKind::Attachment);
                reader.activate_tree_row(&row, window, cx);
                let preview = reader.file_preview.as_ref().unwrap();
                assert!(preview.log.is_some());
                assert!(preview.text.is_none());
                assert_eq!(reader.selected_file(), name);
            });
            visual.run_until_parked();
            let view = log_view(&reader, visual);
            view.read_with(visual, |view, _| {
                let file = view
                    .file()
                    .expect("vault log completed background indexing");
                assert_eq!(file.index().len(), 3);
                assert_eq!(file.index().get(0).unwrap().level(), Level::Info);
                assert_eq!(file.index().get(2).unwrap().level(), Level::Error);
            });
            // Tree activation retains navigator focus; a row selection gives
            // the log its normal keyboard context before Copy.
            view.update_in(visual, |view, window, cx| view.select(1, true, window, cx));
            visual.simulate_keystrokes(if cfg!(target_os = "macos") {
                "cmd-c"
            } else {
                "ctrl-c"
            });
            visual.read(|cx| {
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().unwrap(),
                    "not json"
                )
            });
            reader.update_in(visual, |reader, window, cx| {
                reader.open_note("start.md", None, window, cx)
            });
            visual.run_until_parked();
            reader.read_with(visual, |reader, _| {
                assert!(reader.file_preview.is_none());
                assert_eq!(reader.current_rel, "start.md");
            });
            assert_eq!(std::fs::read_to_string(root.join(name)).unwrap(), text);
        }
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
            assert!(reader.file_preview.as_ref().unwrap().text.is_none());
            assert_eq!(reader.navigation.history, ["app.jsonl"]);
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
        reader.read_with(visual, |reader, _| {
            assert!(reader.file_preview.as_ref().unwrap().text.is_none());
        });
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

    #[gpui::test]
    fn queued_note_open_wins_over_the_initial_log_publication(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::bind_keys(cx);
        });
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("files");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("app.log"), "initial log\n").unwrap();
        std::fs::write(root.join("notes.md"), "# Queued note\n").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(root.join("app.log")),
                        session_directory: Some(fixture.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader.update(cx, |reader, _| {
                assert!(!reader.loading.as_ref().unwrap().published);
                assert!(reader.file_preview.is_none());
                reader.queued_open_note = Some("notes.md".into());
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(
                reader.queued_open_note.is_none(),
                "publication consumed the request"
            );
            assert_eq!(reader.selected_file(), "notes.md");
            assert!(
                reader.file_preview.is_none(),
                "the initial log cannot replace the newer note"
            );
            assert!(reader.document_ready());
        });
        assert_eq!(
            std::fs::read_to_string(root.join("app.log")).unwrap(),
            "initial log\n"
        );
    }

    #[test]
    fn counter_reads_n_of_m_and_never_shows_zero() {
        assert_eq!(match_counter(None, 480_974, false), None, "no filter");
        assert_eq!(
            match_counter(Some(1_203), 480_974, false).as_deref(),
            Some("1,203 of 480,974")
        );
        assert_eq!(match_counter(Some(0), 480_974, false), None);
        assert_eq!(
            match_counter(Some(0), 10, true).as_deref(),
            Some("Filtering…")
        );
        let level = |min, without_level| filter::LevelFilter { min, without_level };
        assert_eq!(level_label(level(None, true)), "All levels");
        assert_eq!(
            level_label(level(Some(Level::Warn), true)),
            "Warning and above"
        );
        assert_eq!(
            level_label(level(Some(Level::Error), false)),
            "Error and above, with a level"
        );
        assert_eq!(level_label(level(None, false)), "Records with a level");
        let chip = FieldChip {
            key: "msg".into(),
            value: format!("a\nb{}", "x".repeat(200)),
        };
        let label = chip_label(&chip);
        assert!(label.starts_with("msg = a b"));
        assert!(label.ends_with('…'));
    }

    fn open_log<'a>(
        text: &str,
        cx: &'a mut TestAppContext,
    ) -> (tempfile::TempDir, Entity<Reader>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("app.jsonl"), text).unwrap();
        let state = root.join(".state");
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(root.join("app.jsonl")),
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
        visual.run_until_parked();
        (directory, entity.unwrap(), visual)
    }

    fn shown(view: &Entity<LogView>, visual: &mut VisualTestContext) -> Vec<usize> {
        view.read_with(visual, |view, _| {
            (0..view.visible_len())
                .map(|row| view.entry_at(row).unwrap())
                .collect()
        })
    }

    fn counter(view: &Entity<LogView>, visual: &mut VisualTestContext) -> Option<String> {
        view.read_with(visual, |view, _| {
            match_counter(
                view.rows.as_ref().map(|rows| rows.len()),
                view.file().unwrap().index().len(),
                view.filtering,
            )
        })
    }

    #[gpui::test]
    fn filters_narrow_rows_in_the_background_and_keep_the_selection(cx: &mut TestAppContext) {
        let text = concat!(
            r#"{"ts":"2025-10-01T00:00:00Z","level":"info","msg":"boot","status":200}"#,
            "\n",
            r#"{"ts":"2025-10-01T00:50:00Z","level":"error","msg":"Cache miss","status":500}"#,
            "\n",
            "panicked at main.rs\n",
            r#"{"ts":"2025-10-01T01:00:00Z","msg":"no level","status":500}"#,
            "\n",
        );
        let (directory, reader, visual) = open_log(text, cx);
        let view = log_view(&reader, visual);
        assert_eq!(shown(&view, visual), [0, 1, 2, 3]);
        assert_eq!(counter(&view, visual), None, "no counter without a filter");

        // The keyboard shortcut reaches the filter field, and typing filters.
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-f"
        } else {
            "ctrl-f"
        });
        visual.simulate_input("CACHE");
        visual.run_until_parked();
        view.read_with(visual, |view, _| assert_eq!(view.filter.text, "CACHE"));
        assert_eq!(shown(&view, visual), [1]);
        assert_eq!(counter(&view, visual).as_deref(), Some("1 of 4"));

        // Clearing returns every row and hides the counter.
        view.update_in(visual, |view, window, cx| view.clear_filters(window, cx));
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [0, 1, 2, 3]);
        view.read_with(visual, |view, cx| {
            assert!(view.text_input.read(cx).value().is_empty());
            assert!(!view.filter.is_active());
        });

        // Level: lines without a level stay unless the toggle hides them.
        view.update(visual, |view, cx| {
            view.update_filter(|filter| filter.level.min = Some(Level::Error), cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [1, 2, 3]);
        assert_eq!(counter(&view, visual).as_deref(), Some("3 of 4"));
        view.update(visual, |view, cx| {
            view.update_filter(|filter| filter.level.without_level = false, cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [1]);

        // Selection and arrow keys walk the filtered rows.
        view.update(visual, |view, cx| {
            view.update_filter(|filter| filter.level = Default::default(), cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| view.select(3, true, window, cx));
        view.update(visual, |view, cx| {
            view.add_chip(
                FieldChip {
                    key: "status".into(),
                    value: "500".into(),
                },
                cx,
            )
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [1, 3]);
        view.read_with(visual, |view, _| {
            assert_eq!(view.selected, Some(3), "the chip's own record stays");
            assert_eq!(view.row_of(3), Some(1));
        });
        visual.simulate_keystrokes("up");
        view.read_with(visual, |view, _| assert_eq!(view.selected, Some(1)));

        // Time window from the newest record; a selection it hides is dropped.
        view.update(visual, |view, cx| {
            view.update_filter(|filter| filter.time = TimeWindow::Last5Minutes, cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [3]);
        view.read_with(visual, |view, _| assert_eq!(view.selected, None));

        // Nothing matches: no zero count, a quiet empty state instead.
        view.update(visual, |view, cx| {
            view.set_text("nothing like it".into(), cx)
        });
        visual.run_until_parked();
        assert!(shown(&view, visual).is_empty());
        assert_eq!(counter(&view, visual), None);
        view.update(visual, |view, cx| view.remove_chip(0, cx));
        view.update(visual, |view, cx| view.set_text(String::new(), cx));
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [3], "only the time window is left");
        assert_eq!(
            std::fs::read_to_string(directory.path().join("app.jsonl")).unwrap(),
            text
        );
    }

    #[gpui::test]
    fn a_late_result_never_replaces_the_latest_filter(cx: &mut TestAppContext) {
        let mut text = String::new();
        for i in 0..40_000 {
            text.push_str(&format!(
                "{{\"level\":\"{}\",\"msg\":\"m{i}\"}}\n",
                if i % 4 == 0 { "error" } else { "info" }
            ));
        }
        let (_directory, reader, visual) = open_log(&text, cx);
        let view = log_view(&reader, visual);
        // Three changes before any run can finish; only the last may land.
        view.update(visual, |view, cx| {
            view.set_text("m1".into(), cx);
            view.update_filter(|filter| filter.level.min = Some(Level::Error), cx);
            view.set_text("m12".into(), cx);
            assert!(view.filtering);
        });
        visual.run_until_parked();
        let rows = shown(&view, visual);
        let expected: Vec<usize> = (0..40_000)
            .filter(|i| i % 4 == 0 && format!("m{i}").contains("m12"))
            .collect();
        assert!(!expected.is_empty());
        assert_eq!(rows, expected);
        view.read_with(visual, |view, _| assert!(!view.filtering));
        // Removing every filter while a run is pending also wins.
        view.update(visual, |view, cx| {
            view.set_text("m3".into(), cx);
            view.set_text(String::new(), cx);
            view.update_filter(|filter| filter.level = Default::default(), cx);
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual).len(), 40_000);
        assert_eq!(counter(&view, visual), None);
    }

    #[test]
    fn epoch_time_shows_its_utc_date_beside_the_raw_value() {
        let record = Record::parse(br#"{"time":1759276800,"x":"1"}"#, Format::JsonLines).unwrap();
        let rows = detail_rows(&record, Some(1_759_276_800_000_000_000));
        assert_eq!(rows[0].1, "1759276800 · 2025-10-01 00:00:00.000 UTC");
        assert_eq!(rows[1].1, "1");
    }
}
