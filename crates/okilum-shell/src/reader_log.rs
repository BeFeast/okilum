//! Read-only structured log view (#602): virtualized one-line rows over a
//! `okilum_core::log` index, a filter bar (text, level, time window and an
//! hl-compatible query, #1158) evaluated off the UI thread, a detail pane for
//! the selected record, a raw-line copy, and follow mode (#1159): the file is
//! polled with `okilum_core::log::tail`, appended records are filtered as they
//! arrive, and a truncation or rotation is announced, not hidden.
use super::*;
use gpui_component::scroll::ScrollableElement as _;
use okilum_core::log::query::{Expr, FieldRef, Literal, Test};
use okilum_core::log::tail::{ResetReason, TailEvent};
use okilum_core::log::{
    block, filter, timestamp, FilterGenerations, Format, Level, LogFile, LogFilter, LogIndex,
    LogTail, Query, Record, Role, TimeWindow, ValueKind,
};
use std::ops::Range;

const CONTEXT: &str = "ReaderLog";
const ROW_HEIGHT: f32 = 24.;
/// Detail pane height until the user drags it (#1156).
const DETAIL_HEIGHT: f32 = 240.;
/// The detail pane keeps its header; the rows keep five lines.
const DETAIL_HEADER_HEIGHT: f32 = 36.;
const MIN_LIST_ROWS: f32 = 5.;
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
const QUERY_MIN_WIDTH: f32 = 200.;
const QUERY_ERROR_MAX_WIDTH: f32 = 260.;
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

/// What the view reads: the indexed file, or the followed file (#1159).
pub(crate) trait LogData {
    fn index(&self) -> &LogIndex;
    fn bytes(&self) -> &[u8];
    fn raw(&self, index: usize) -> Option<&[u8]>;
    fn record(&self, index: usize) -> Option<Record>;
    fn path(&self) -> &Path;
}

impl LogData for LogFile {
    fn index(&self) -> &LogIndex {
        LogFile::index(self)
    }
    fn bytes(&self) -> &[u8] {
        LogFile::bytes(self)
    }
    fn raw(&self, index: usize) -> Option<&[u8]> {
        LogFile::raw(self, index)
    }
    fn record(&self, index: usize) -> Option<Record> {
        LogFile::record(self, index)
    }
    fn path(&self) -> &Path {
        LogFile::path(self)
    }
}

impl LogData for LogTail {
    fn index(&self) -> &LogIndex {
        LogTail::index(self)
    }
    fn bytes(&self) -> &[u8] {
        LogTail::bytes(self)
    }
    fn raw(&self, index: usize) -> Option<&[u8]> {
        LogTail::raw(self, index)
    }
    fn record(&self, index: usize) -> Option<Record> {
        LogTail::record(self, index)
    }
    fn path(&self) -> &Path {
        LogTail::path(self)
    }
}

/// A copy of what a background filter run reads.
enum Snapshot {
    File(Arc<LogFile>),
    Owned { index: LogIndex, bytes: Vec<u8> },
}

impl Snapshot {
    fn parts(&self) -> (&LogIndex, &[u8]) {
        match self {
            Snapshot::File(file) => (file.index(), file.bytes()),
            Snapshot::Owned { index, bytes } => (index, bytes),
        }
    }
}

/// The file once follow mode has opened it. Turning follow off keeps what
/// was read, so the rows do not jump back to the older indexed file.
struct Follow {
    tail: LogTail,
    /// Polling; `None` while follow is off.
    poll: Option<Task<()>>,
    /// The newest row stays in view until the user scrolls up.
    pinned: bool,
    /// Rows added while not pinned: the count on the "N new" pill.
    unseen: usize,
    /// The last truncation or rotation, shown above the rows.
    reset: Option<(ResetReason, usize)>,
    /// The poll could not read the file (it may be being rotated).
    error: Option<String>,
    /// When a full run last started for a time window that moves with appends.
    last_full_run: Option<std::time::Instant>,
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
    query_input: Entity<InputState>,
    /// The query field's text as typed; the filter keeps the last one that
    /// parsed, so a typo never clears the rows.
    query_text: String,
    /// Column (1-based, in characters) and message of the typed query's error.
    query_error: Option<(usize, String)>,
    detail_split: reader_split::Split,
    follow: Option<Follow>,
    /// Entries a background run covered; appends past it are filtered when
    /// its result lands.
    filtered_upto: usize,
    /// A wheel event happened: the next frame decides whether the list is at
    /// its bottom, pinning or pausing follow.
    check_pin: bool,
    /// Follow could not open the file.
    follow_failed: Option<String>,
    _follow_open: Option<Task<()>>,
    _text_input_events: Subscription,
    _query_input_events: Subscription,
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
        let query_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Query: level>=warn and status>=500")
        });
        let query_events = cx.subscribe_in(
            &query_input,
            window,
            |this: &mut Self, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    this.set_query(text, cx);
                }
                InputEvent::PressEnter { .. } => {
                    this.remember_query(cx);
                    this.focus.focus(window, cx);
                }
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
            query_input,
            query_text: String::new(),
            query_error: None,
            detail_split: reader_split::Split::new(
                "log-detail",
                reader_split::Initial::Pixels(DETAIL_HEIGHT),
                DETAIL_HEADER_HEIGHT,
                ROW_HEIGHT * MIN_LIST_ROWS,
            ),
            follow: None,
            filtered_upto: 0,
            check_pin: false,
            follow_failed: None,
            _follow_open: None,
            _text_input_events: events,
            _query_input_events: query_events,
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

    /// The records shown: the followed file once follow has opened it.
    fn data(&self) -> Option<&dyn LogData> {
        match &self.follow {
            Some(follow) => Some(&follow.tail),
            None => self.file().map(|file| file.as_ref() as &dyn LogData),
        }
    }

    fn snapshot(&self) -> Option<Snapshot> {
        match &self.follow {
            Some(follow) => Some(Snapshot::Owned {
                index: follow.tail.index().clone(),
                bytes: follow.tail.bytes()[..follow.tail.index().bytes() as usize].to_vec(),
            }),
            None => self.file().cloned().map(Snapshot::File),
        }
    }

    fn following(&self) -> bool {
        self.follow
            .as_ref()
            .is_some_and(|follow| follow.poll.is_some())
    }

    /// Rows currently listed.
    fn visible_len(&self) -> usize {
        match &self.rows {
            Some(rows) => rows.len(),
            None => self.data().map_or(0, |data| data.index().len()),
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

    /// A query that parses replaces the filter's; one that does not leaves
    /// the last valid query (and its rows) in place and shows where it fails.
    fn set_query(&mut self, text: String, cx: &mut Context<Self>) {
        match Query::parse(&text) {
            Ok(query) => {
                self.query_error = None;
                self.update_filter(
                    |filter| filter.query = (!query.is_empty()).then_some(query),
                    cx,
                );
            }
            Err(error) => {
                self.query_error = Some((error.column(&text) + 1, error.message.clone()));
            }
        }
        self.query_text = text;
        cx.notify();
    }

    fn replace_query(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.query_input
            .update(cx, |input, cx| input.set_value(text.clone(), window, cx));
        self.set_query(text, cx);
    }

    /// A field from the detail pane narrows the query: the same model as
    /// typing `key = "value"` (#1158).
    fn add_field_term(
        &mut self,
        key: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let term = field_term(key, value);
        if self.has_term(&term) {
            return;
        }
        let text = match (&self.query_error, &self.filter.query) {
            (None, Some(query)) => Expr::And(vec![query.expr().clone(), term]).to_string(),
            (None, None) => term.to_string(),
            // Keep what is being typed; the user fixes the typo.
            (Some(_), _) => format!("{} and {term}", self.query_text.trim_end()),
        };
        self.replace_query(text, window, cx);
        self.remember_query(cx);
    }

    /// True when the applied query already requires `term`.
    fn has_term(&self, term: &Expr) -> bool {
        match self.filter.query.as_ref().map(Query::expr) {
            Some(Expr::And(items)) => items.contains(term),
            Some(expr) => expr == term,
            None => false,
        }
    }

    fn history_key(&self) -> Option<String> {
        self.data()
            .map(|data| data.path().to_string_lossy().into_owned())
    }

    fn remember_query(&mut self, cx: &mut Context<Self>) {
        if self.query_error.is_some() {
            return;
        }
        if let (Some(key), Some(query)) = (self.history_key(), &self.filter.query) {
            reader_ui_state::remember_log_query(&key, &query.to_string(), cx);
        }
    }

    fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.text_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.query_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.query_text.clear();
        self.query_error = None;
        self.update_filter(|filter| *filter = LogFilter::default(), cx);
    }

    /// Starts a run for the current filter. Every earlier run is cancelled by
    /// the new generation, and a result that arrives late is dropped.
    fn refilter(&mut self, cx: &mut Context<Self>) {
        let ticket = self.generations.next();
        let Some(snapshot) = self.snapshot() else {
            return;
        };
        self.filtered_upto = snapshot.parts().0.len();
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
                .spawn(async move {
                    let (index, bytes) = snapshot.parts();
                    filter::evaluate(&filter, index, bytes, &ticket)
                })
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
                // Follow mode appended while the run read its snapshot.
                let appended = this.filtered_upto..this.data().map_or(0, |d| d.index().len());
                this.extend_rows(appended);
                this.after_rows_changed();
                this.keep_pinned();
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Follow on, off, or on again. The first time it reads the file afresh
    /// with `LogTail`, off the UI thread.
    fn toggle_follow(&mut self, cx: &mut Context<Self>) {
        match &mut self.follow {
            Some(follow) if follow.poll.is_some() => {
                follow.poll = None;
                cx.notify();
            }
            Some(follow) => {
                follow.pinned = true;
                follow.unseen = 0;
                follow.poll = Some(Self::start_poll(cx));
                self.keep_pinned();
                cx.notify();
            }
            None => {
                let Some(path) = self.data().map(|data| data.path().to_owned()) else {
                    return;
                };
                self.follow_failed = None;
                self._follow_open = Some(cx.spawn(async move |this, cx| {
                    let tail = cx
                        .background_executor()
                        .spawn(async move {
                            let interval = okilum_core::log::tail::poll_interval_for(&path);
                            LogTail::open(&path).map(|tail| tail.with_interval(interval))
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        match tail {
                            Ok(tail) => {
                                this.follow = Some(Follow {
                                    tail,
                                    poll: Some(Self::start_poll(cx)),
                                    pinned: true,
                                    unseen: 0,
                                    reset: None,
                                    error: None,
                                    last_full_run: None,
                                });
                                // Same bytes from the start: the selection keeps its entry.
                                this.refilter(cx);
                                this.keep_pinned();
                            }
                            Err(error) => this.follow_failed = Some(format!("{error:#}")),
                        }
                        cx.notify();
                    });
                }));
            }
        }
    }

    /// Polls at the tail's interval: the read runs in the background, the
    /// new bytes are indexed here. Dropping the task stops it.
    fn start_poll(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| loop {
            let Ok(Some(interval)) = this.update(cx, |this, _| {
                this.follow.as_ref().map(|follow| follow.tail.interval())
            }) else {
                return;
            };
            cx.background_executor().timer(interval).await;
            let Ok(Some(request)) = this.update(cx, |this, _| {
                this.follow.as_mut().map(|follow| follow.tail.begin_poll())
            }) else {
                return;
            };
            let read = cx
                .background_executor()
                .spawn(async move { request.read() })
                .await;
            if this
                .update(cx, |this, cx| this.apply_poll(read, cx))
                .is_err()
            {
                return;
            }
        })
    }

    fn apply_poll(
        &mut self,
        read: anyhow::Result<okilum_core::log::tail::PollRead>,
        cx: &mut Context<Self>,
    ) {
        let Some(follow) = self.follow.as_mut() else {
            return;
        };
        let event = match read {
            Ok(read) => {
                follow.error = None;
                follow.tail.apply(read)
            }
            Err(error) => {
                follow.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        match event {
            TailEvent::Unchanged | TailEvent::Stale => return,
            TailEvent::Appended {
                entries,
                reindexed,
                grown,
            } => {
                let time_moves = self.filter.time != TimeWindow::All;
                let due = self.follow.as_ref().is_some_and(|follow| {
                    follow
                        .last_full_run
                        .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1))
                });
                if reindexed || (time_moves && due) {
                    // Every row may change: earlier entries were indexed
                    // again, or the newest record moved the time window.
                    if let Some(follow) = self.follow.as_mut() {
                        follow.last_full_run = Some(std::time::Instant::now());
                    }
                    self.refilter(cx);
                } else {
                    // An entry that took new lines (a stack trace written line
                    // by line, #1161) is filtered again with them; it is not
                    // a new row.
                    let mut entries = entries;
                    let mut regrown = 0;
                    if grown && entries.start > 0 {
                        entries.start -= 1;
                        regrown = usize::from(self.drop_last_row(entries.start));
                    }
                    let added = self.extend_rows(entries).saturating_sub(regrown);
                    if let Some(follow) = self.follow.as_mut() {
                        if !follow.pinned {
                            follow.unseen += added;
                        }
                    }
                }
            }
            TailEvent::Reset {
                reason, previous, ..
            } => {
                if let Some(follow) = self.follow.as_mut() {
                    follow.reset = Some((reason, previous.index().len()));
                    follow.unseen = 0;
                }
                self.selected = None;
                self.refilter(cx);
            }
        }
        self.keep_pinned();
        cx.notify();
    }

    /// Filters the appended entries `range` into the rows; returns how many
    /// rows were added.
    fn extend_rows(&mut self, range: Range<usize>) -> usize {
        if range.is_empty() {
            return 0;
        }
        if self.rows.is_none() {
            return range.len();
        }
        let added = match self.data() {
            Some(data) => filter::evaluate_range(&self.filter, data.index(), data.bytes(), range),
            None => return 0,
        };
        let count = added.len();
        if let Some(rows) = self.rows.as_mut() {
            Arc::make_mut(rows).extend(added);
        }
        count
    }

    /// Removes the last row if it shows entry `index`; returns whether it did.
    fn drop_last_row(&mut self, index: usize) -> bool {
        let Some(rows) = self.rows.as_mut() else {
            return false;
        };
        if rows.last().is_some_and(|&last| last as usize == index) {
            Arc::make_mut(rows).pop();
            return true;
        }
        false
    }

    /// Following and pinned: the newest row stays in view.
    fn keep_pinned(&mut self) {
        let pinned = self.following() && self.follow.as_ref().is_some_and(|follow| follow.pinned);
        let rows = self.visible_len();
        if pinned && rows > 0 {
            self.scroll.scroll_to_item(rows - 1, ScrollStrategy::Bottom);
        }
    }

    /// The "N new" pill: back to the newest row, pinned again.
    fn jump_to_newest(&mut self, cx: &mut Context<Self>) {
        if let Some(follow) = self.follow.as_mut() {
            follow.pinned = true;
            follow.unseen = 0;
        }
        self.keep_pinned();
        cx.notify();
    }

    /// After a wheel event: pinned at the bottom, paused anywhere above it.
    fn update_pin(&mut self) {
        if !std::mem::take(&mut self.check_pin) {
            return;
        }
        let Some(follow) = self.follow.as_mut() else {
            return;
        };
        let handle = self.scroll.0.borrow();
        let offset = handle.base_handle.offset();
        let max = handle.base_handle.max_offset();
        let at_bottom = -offset.y >= max.y - px(2.);
        follow.pinned = at_bottom;
        if at_bottom {
            follow.unseen = 0;
        }
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
        if self.data().is_some_and(|data| !data.index().is_empty()) {
            self.text_input
                .update(cx, |input, cx| input.focus(window, cx));
        }
    }

    fn copy_raw(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(raw) = self
            .selected
            .and_then(|index| self.data().and_then(|data| data.raw(index)))
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
        let this = &*self;
        let Some(file) = this.data() else {
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
                let row = row_text(file, index);
                let lines = row.lines;
                let selected = this.selected == Some(index);
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
                    // A multi-line entry (a stack trace, a crash report) is
                    // one folded row; the detail pane shows every line.
                    .when(lines > 1, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .font_family(mono.clone())
                                .text_size(px(12.))
                                .text_color(muted)
                                .child(format!("▸ {} lines", grouped(lines as u64))),
                        )
                    })
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
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let pane = v_flex().size_full().min_h_0();
        let (Some(file), Some(index)) = (self.data(), self.selected) else {
            return pane
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child(
                    if self.data().is_some_and(|data| !data.index().is_empty()) {
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
        let raw = file.raw(index).unwrap_or_default();
        let lines = block::line_count(raw);
        let header = h_flex()
            .flex_none()
            .h(px(DETAIL_HEADER_HEIGHT))
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
                        "Record {} of {} · {}",
                        grouped(self.row_of(index).unwrap_or(index) as u64 + 1),
                        grouped(self.visible_len() as u64),
                        match lines {
                            1 => format!("line {}", grouped(entry.line())),
                            n => format!(
                                "lines {}–{}",
                                grouped(entry.line()),
                                grouped(entry.line() + n as u64 - 1)
                            ),
                        }
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
        let record = file.record(index);
        let is_record = record.is_some();
        let body = match record {
            Some(record) => body.children(
                detail_rows(&record, entry.timestamp())
                    .into_iter()
                    .zip(&record.fields)
                    .enumerate()
                    .map(|(position, ((key, value, kind), field))| {
                        let group = SharedString::from(format!("log-field-{position}"));
                        let term = field_term(&field.key, &field.value);
                        let present = self.has_term(&term);
                        let (field_key, field_value) = (field.key.clone(), field.value.clone());
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
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.add_field_term(
                                                    &field_key,
                                                    &field_value,
                                                    window,
                                                    cx,
                                                )
                                            })),
                                    ),
                            )
                    }),
            ),
            None => body
                .child(div().text_color(muted).pb_1().child(
                    match (block::block_kind(raw), file.index().format()) {
                        (Some(kind), _) => {
                            format!("{} · {} lines", kind.label(), grouped(lines as u64))
                        }
                        (None, Format::Plain) => "Plain text line".to_owned(),
                        (None, format) => format!("Not a {} record", format.label()),
                    },
                ))
                .child(
                    div()
                        .font_family(mono.clone())
                        .text_size(px(12.))
                        .child(String::from_utf8_lossy(raw).into_owned()),
                ),
        };
        // A record's continuation lines, frame by frame as written; an entry
        // that is no record shows all its lines above.
        let body = body.when_some(
            block::continuation(raw).filter(|_| is_record),
            |body, rest| {
                body.child(
                    div()
                        .id("log-detail-continuation")
                        .pt_2()
                        .font_family(mono)
                        .text_size(px(12.))
                        .child(String::from_utf8_lossy(rest).into_owned()),
                )
            },
        );
        pane.child(header).child(body).into_any_element()
    }

    /// One compact row above the list: text, level and time menus, chips,
    /// and the match counter. Its height never changes, so filtering never
    /// moves the rows.
    fn render_filter_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let palette = brand::palette(cx);
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
        let danger = palette.danger;
        let error = self.query_error.clone();
        let query = div()
            .flex_1()
            .min_w(px(QUERY_MIN_WIDTH))
            .h(px(BAR_CONTROL_HEIGHT))
            .flex()
            .items_center()
            .rounded(px(6.))
            .bg(palette.surface_raised)
            .debug_selector(|| "log-query".into())
            .child(
                Input::new(&self.query_input)
                    .appearance(false)
                    .cleanable(true)
                    .small()
                    .when_some(error, |input, (column, message)| {
                        // The error sits inside the field, so the bar keeps
                        // its height; the rows keep the last valid query.
                        input.suffix(
                            div()
                                .id("log-query-error")
                                .debug_selector(|| "log-query-error".into())
                                .max_w(px(QUERY_ERROR_MAX_WIDTH))
                                .truncate()
                                .text_size(px(12.))
                                .text_color(danger)
                                .child(format!("Column {column}: {message}"))
                                .tooltip(move |window, cx| {
                                    gpui_component::tooltip::Tooltip::new(format!(
                                        "Column {column}: {message}"
                                    ))
                                    .build(window, cx)
                                }),
                        )
                    }),
            );
        let history = self
            .history_key()
            .map(|key| reader_ui_state::log_queries(&key, cx))
            .filter(|queries| !queries.is_empty())
            .map(|queries| {
                let view = view.clone();
                bar_button("log-query-history", false)
                    .icon(Icon::default().path(brand::READER_CLOCK_ICON))
                    .tooltip("Recent queries for this log")
                    .debug_selector(|| "log-query-history".into())
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                        let mut menu = menu;
                        for query in &queries {
                            let view = view.clone();
                            let text = query.clone();
                            menu = menu.item(PopupMenuItem::new(query.clone()).on_click(
                                move |_, window, cx| {
                                    let text = text.clone();
                                    let _ = view.update(cx, |this, cx| {
                                        this.replace_query(text, window, cx);
                                        this.remember_query(cx);
                                    });
                                },
                            ));
                        }
                        menu
                    })
            });
        let counter = match_counter(
            self.rows.as_ref().map(|rows| rows.len()),
            self.data().map_or(0, |data| data.index().len()),
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
            .child(query)
            .children(history)
            .child(self.follow_button(cx))
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

    fn follow_button(&self, cx: &mut Context<Self>) -> Button {
        let following = self.following();
        bar_button("log-follow", following)
            .label(if following { "Following" } else { "Follow" })
            .tooltip(if following {
                "Stop following new records"
            } else {
                "Show records as they are written"
            })
            .debug_selector(|| "log-follow".into())
            .on_click(cx.listener(|this, _, _, cx| this.toggle_follow(cx)))
    }

    /// The truncation or rotation follow mode saw last, above the rows.
    fn render_reset(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (reason, earlier) = self.follow.as_ref()?.reset?;
        let what = match reason {
            ResetReason::Truncated => "The file was truncated",
            ResetReason::Rotated => "The file was rotated",
        };
        let earlier = match earlier {
            0 => String::new(),
            1 => " · 1 earlier record is not shown".to_owned(),
            n => format!(" · {} earlier records are not shown", grouped(n as u64)),
        };
        Some(
            div()
                .debug_selector(|| "log-follow-reset".into())
                .flex_none()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(brand::palette(cx).border_subtle)
                .bg(brand::palette(cx).surface_raised)
                .text_size(px(12.))
                .text_color(cx.theme().muted_foreground)
                .child(format!("{what}{earlier}; following the new content"))
                .into_any_element(),
        )
    }

    /// "N new" while following is paused by scrolling up.
    fn render_unseen(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let follow = self.follow.as_ref()?;
        if !self.following() || follow.pinned || follow.unseen == 0 {
            return None;
        }
        let label = match follow.unseen {
            1 => "1 new record".to_owned(),
            n => format!("{} new records", grouped(n as u64)),
        };
        Some(
            div()
                .absolute()
                .bottom_3()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    Button::new("log-follow-new")
                        .debug_selector(|| "log-follow-new".into())
                        .small()
                        .primary()
                        .label(label)
                        .tooltip("Back to the newest record")
                        .on_click(cx.listener(|this, _, _, cx| this.jump_to_newest(cx))),
                )
                .into_any_element(),
        )
    }

    fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = match (&self.state, &self.follow) {
            (State::Indexing, _) => "Indexing…".to_owned(),
            (State::Failed(error), _) => format!("Cannot show this log: {error}"),
            (State::Ready(_), Some(follow)) => {
                let mut text = index_status(follow.tail.index());
                if let Some(error) = &follow.error {
                    text.push_str(&format!(" · Cannot read the file: {error}"));
                } else if follow.tail.is_missing() {
                    text.push_str(" · Waiting for the file");
                } else if self.following() {
                    text.push_str(" · Following");
                }
                text
            }
            (State::Ready(document), None) => match &self.follow_failed {
                Some(error) => format!("{} · Cannot follow: {error}", status_text(document)),
                None => status_text(document),
            },
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
        self.update_pin();
        let count = self.visible_len();
        let has_entries = self.data().is_some_and(|data| !data.index().is_empty());
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
            .on_scroll_wheel(cx.listener(|this, _: &ScrollWheelEvent, _, cx| {
                if this.follow.is_some() {
                    this.check_pin = true;
                    cx.notify();
                }
            }))
            .children(self.render_unseen(cx))
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
                .flex_col()
                .gap_2()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(message)
                // An empty log can still be followed until records arrive.
                .when(
                    matches!(self.state, State::Ready(_)) && self.follow.is_none(),
                    |list| list.child(self.follow_button(cx)),
                )
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
            .when(has_entries || self.follow.is_some(), |view| {
                view.child(self.render_filter_bar(cx))
            })
            .children(self.render_reset(cx))
            .child(reader_split::vertical(
                &self.detail_split,
                |view: &mut Self| &mut view.detail_split,
                list,
                self.render_detail(cx),
                cx,
            ))
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
/// The query term a detail-pane field adds: the raw key equals the value as
/// text, which is what the field shows (#1158).
fn field_term(key: &str, value: &str) -> Expr {
    Expr::Field {
        field: FieldRef::Key(key.to_owned()),
        test: Test::Eq(Literal {
            text: value.to_owned(),
            number: None,
        }),
        include_absent: false,
    }
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
    /// Lines the entry spans; more than one is shown folded.
    pub lines: usize,
}

pub(crate) fn row_text(file: &dyn LogData, index: usize) -> RowText {
    let raw = file.raw(index).unwrap_or_default();
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
        None => row.push(&String::from_utf8_lossy(block::head(raw)), false),
    }
    RowText {
        time,
        text: row.text,
        muted: row.muted,
        lines: block::line_count(raw),
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
    index_status(document.file.index())
}

fn index_status(index: &LogIndex) -> String {
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
    fn a_multi_line_entry_is_one_row_with_its_first_line() {
        let (_dir, file) = log(concat!(
            r#"{"level":"error","msg":"request failed"}"#,
            "\n\tat com.example.Pool.take(Pool.java:88)\n",
            "panic: boom\n\ngoroutine 1 [running]:\n",
        ));
        let record = row_text(&file, 0);
        assert_eq!((record.text.as_str(), record.lines), ("request failed", 2));
        let crash = row_text(&file, 1);
        assert_eq!((crash.text.as_str(), crash.lines), ("panic: boom", 3));
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

    /// #1156: the detail pane is resized with the handle above it, keeps
    /// the rows' and its header's minimums, collapses on a double-click and
    /// keeps its height for the next log and the next launch.
    #[gpui::test]
    fn detail_pane_resizes_collapses_and_remembers_its_height(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let (root, state) = (
            directory.path().join("vault"),
            directory.path().join("state"),
        );
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("start.md"), "# Start").unwrap();
        let text =
            "{\"level\":\"info\",\"msg\":\"first\"}\n{\"level\":\"warn\",\"msg\":\"second\"}\n";
        std::fs::write(root.join("events.jsonl"), text).unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::bind_keys(cx);
            reader_ui_state::install(&state, cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        index_dir: Some(directory.path().join("index")),
                        session_directory: Some(state.clone()),
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
        visual.simulate_resize(gpui::size(px(1440.), px(900.)));
        visual.run_until_parked();
        let open_log = |visual: &mut VisualTestContext| {
            reader.update_in(visual, |reader, window, cx| {
                let row = reader
                    .tree
                    .rows
                    .iter()
                    .find(|row| row.path == "events.jsonl")
                    .unwrap()
                    .clone();
                reader.activate_tree_row(&row, window, cx);
            });
            visual.run_until_parked();
            let view = log_view(&reader, visual);
            view.update_in(visual, |view, window, cx| view.select(0, true, window, cx));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        let pane = |visual: &mut VisualTestContext| {
            visual
                .debug_bounds("log-detail-pane")
                .expect("detail pane shown")
        };
        open_log(visual);
        let before = pane(visual);
        assert!(
            (before.size.height - px(DETAIL_HEIGHT)).abs() < px(1.),
            "positive control: the default height, {before:?}"
        );
        // Drag the handle up until the pane fills about 60% of the split.
        let handle = visual.debug_bounds("log-detail-handle").unwrap();
        let split_bottom = before.bottom();
        let target = point(handle.center().x, split_bottom - px(540.));
        visual.simulate_mouse_down(handle.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(
            handle.center() - point(px(0.), px(20.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.simulate_mouse_move(target, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let dragged = pane(visual);
        assert!(
            (dragged.size.height - px(540.)).abs() < px(HANDLE_TOLERANCE),
            "the pane follows the handle: {dragged:?}"
        );
        // Not past the rows' minimum.
        let rows_minimum = px(ROW_HEIGHT * MIN_LIST_ROWS);
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| {
            view.detail_split.drag_to(5_000., 600.);
            cx.notify();
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let tallest = pane(visual);
        assert!(
            tallest.size.height <= px(600. - reader_split::HANDLE_HEIGHT) - rows_minimum + px(1.),
            "the rows keep five lines: {tallest:?}"
        );
        view.update(visual, |view, cx| {
            view.detail_split.drag_to(540., 900.);
            cx.notify();
        });
        // A double-click collapses the pane to its header, another restores it.
        let double_click = |visual: &mut VisualTestContext| {
            let at = visual.debug_bounds("log-detail-handle").unwrap().center();
            for click_count in [1, 2] {
                visual.simulate_event(MouseDownEvent {
                    position: at,
                    button: MouseButton::Left,
                    click_count,
                    ..Default::default()
                });
                visual.simulate_event(MouseUpEvent {
                    position: at,
                    button: MouseButton::Left,
                    click_count,
                    ..Default::default()
                });
            }
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        double_click(visual);
        assert!(
            (pane(visual).size.height - px(DETAIL_HEADER_HEIGHT)).abs() < px(1.),
            "collapsed to the header"
        );
        double_click(visual);
        assert!(
            (pane(visual).size.height - px(540.)).abs() < px(HANDLE_TOLERANCE),
            "restored"
        );
        // Remembered for the next log view and written for the next launch.
        reader.update_in(visual, |reader, window, cx| {
            reader.open_note("start.md", None, window, cx)
        });
        visual.run_until_parked();
        open_log(visual);
        assert!(
            (pane(visual).size.height - px(540.)).abs() < px(HANDLE_TOLERANCE),
            "the reopened log keeps the height"
        );
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.join("reader-ui.json")).unwrap()).unwrap();
        let saved = saved["split_sizes"]["log-detail"].as_f64().unwrap() as f32;
        assert!(
            (saved - 540.).abs() < HANDLE_TOLERANCE,
            "saved for the next launch: {saved}"
        );
    }

    /// The handle's hit area is a few pixels tall; the drop point lands
    /// somewhere inside it.
    const HANDLE_TOLERANCE: f32 = 8.;

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
        assert!(!root.join(".okilum-index").exists());
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
    }

    /// #1158: a detail-pane field becomes canonical query text that parses
    /// back to the same term, quoting what a bare word cannot carry.
    #[test]
    fn field_terms_are_query_text() {
        for (key, value, text) in [
            (
                "kubernetes.pod_name",
                "checkout-api-85b4",
                "kubernetes.pod_name = checkout-api-85b4",
            ),
            ("status", "500", r#"status = "500""#),
            ("level", "NOTICE", r#""level" = NOTICE"#),
            ("msg", "a \"b\"\nc", r#""msg" = "a \"b\"\nc""#),
            ("symbol", "FDAX Z6", r#"symbol = "FDAX Z6""#),
        ] {
            let term = field_term(key, value);
            assert_eq!(term.to_string(), text);
            assert_eq!(Query::parse(text).unwrap().expr(), &term, "{text}");
        }
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
        view.update_in(visual, |view, window, cx| {
            view.add_field_term("status", "500", window, cx)
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
        view.update_in(visual, |view, window, cx| {
            view.replace_query(String::new(), window, cx)
        });
        view.update(visual, |view, cx| view.set_text(String::new(), cx));
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [3], "only the time window is left");
        assert_eq!(
            std::fs::read_to_string(directory.path().join("app.jsonl")).unwrap(),
            text
        );
    }

    /// #1158: the query bar filters in the background, a typo shows its
    /// column and keeps the last valid rows, a detail-pane field narrows the
    /// same query, and applied queries are kept per file.
    #[gpui::test]
    fn query_bar_filters_reports_typos_and_remembers_queries(cx: &mut TestAppContext) {
        let text = concat!(
            r#"{"level":"info","msg":"boot","status":200,"pod":"checkout-1"}"#,
            "\n",
            r#"{"level":"warn","msg":"slow","status":200,"pod":"checkout-2"}"#,
            "\n",
            r#"{"level":"error","msg":"upstream","status":503,"pod":"checkout-2","err":"timeout"}"#,
            "\n",
            r#"{"level":"error","msg":"bad gateway","status":502,"pod":"payments-1"}"#,
            "\n",
        );
        let state = tempfile::tempdir().unwrap();
        cx.update(|cx| reader_ui_state::install(state.path(), cx));
        let (_directory, reader, visual) = open_log(text, cx);
        let view = log_view(&reader, visual);
        let type_query = |visual: &mut VisualTestContext, query: &str| {
            view.update_in(visual, |view, window, cx| {
                view.query_input
                    .update(cx, |input, cx| input.set_value(query, window, cx));
                view.set_query(query.into(), cx);
            });
            visual.run_until_parked();
        };
        assert_eq!(shown(&view, visual), [0, 1, 2, 3], "positive control");
        type_query(visual, "level>=warn and status>=500");
        assert_eq!(shown(&view, visual), [2, 3]);
        assert_eq!(counter(&view, visual).as_deref(), Some("2 of 4"));
        type_query(visual, "exists(err)");
        assert_eq!(shown(&view, visual), [2]);

        // A typo: the error names its column, the rows stay as they were.
        type_query(visual, "exists(err) and pod ~ checkout");
        view.read_with(visual, |view, _| {
            let (column, _) = view.query_error.clone().expect("a parse error");
            assert_eq!(column, 21, "{:?}", view.query_error);
        });
        assert_eq!(shown(&view, visual), [2], "the last valid query stays");
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let error = visual
            .debug_bounds("log-query-error")
            .expect("the error shows in the query field");
        let field = visual.debug_bounds("log-query").expect("query field");
        assert!(field.contains(&error.center()), "inside the field");

        // A detail-pane field narrows the same query.
        type_query(visual, "level>=error");
        assert_eq!(shown(&view, visual), [2, 3]);
        view.update_in(visual, |view, window, cx| {
            view.add_field_term("pod", "payments-1", window, cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [3]);
        view.read_with(visual, |view, cx| {
            assert_eq!(
                view.query_input.read(cx).value().as_ref(),
                "level >= error and pod = payments-1"
            );
            assert!(view.query_error.is_none());
        });
        // The same field again changes nothing.
        view.update_in(visual, |view, window, cx| {
            view.add_field_term("pod", "payments-1", window, cx)
        });
        view.read_with(visual, |view, cx| {
            assert_eq!(
                view.query_input.read(cx).value().as_ref(),
                "level >= error and pod = payments-1"
            );
        });

        // History: newest first, applied by the menu, kept for the file.
        view.update(visual, |view, cx| view.remember_query(cx));
        type_query(visual, "status=200");
        view.update(visual, |view, cx| view.remember_query(cx));
        let key = view.read_with(visual, |view, _| view.history_key().unwrap());
        let history = visual.update(|_, cx| reader_ui_state::log_queries(&key, cx));
        assert_eq!(
            history,
            ["status = 200", "level >= error and pod = payments-1"]
        );
        view.update_in(visual, |view, window, cx| {
            view.replace_query(history[1].clone(), window, cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [3]);
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.path().join("reader-ui.json")).unwrap())
                .unwrap();
        assert_eq!(saved["log_queries"][&key][0], "status = 200");

        // Clear filters empties the query and its error.
        view.update_in(visual, |view, window, cx| view.clear_filters(window, cx));
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [0, 1, 2, 3]);
        view.read_with(visual, |view, cx| {
            assert!(view.query_input.read(cx).value().is_empty());
            assert!(view.filter.query.is_none());
        });
    }

    /// #1159 acceptance, simulated: a writer appends every poll interval for
    /// 600 polls (150 s at 4 polls/s) while a query is active, and the file
    /// is moved and recreated halfway, as logrotate does. Every poll the
    /// rows are exactly the matching records written so far, in file order,
    /// none twice and none missing, and the view stays pinned.
    #[gpui::test]
    fn follow_mode_stays_exact_over_many_polls_and_a_rotation(cx: &mut TestAppContext) {
        use std::io::Write as _;
        let line = |n: usize| {
            let level = if n.is_multiple_of(10) { "warn" } else { "info" };
            format!("{{\"level\":\"{level}\",\"msg\":\"tick {n}\"}}\n")
        };
        let (directory, reader, visual) = open_log(&line(1), cx);
        let path = directory.path().canonicalize().unwrap().join("app.jsonl");
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        view.update(visual, |view, cx| view.set_query("level>=warn".into(), cx));
        visual.run_until_parked();
        let mut written = 1;
        let mut since_rotation = 1;
        for poll in 0..600usize {
            if poll == 300 {
                std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
                std::fs::write(&path, "").unwrap();
                since_rotation = 0;
            }
            // 2–3 records per 250 ms: 10 records a second.
            let burst = if poll.is_multiple_of(2) { 3 } else { 2 };
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            for _ in 0..burst {
                written += 1;
                since_rotation += 1;
                file.write_all(line(written).as_bytes()).unwrap();
            }
            drop(file);
            visual.executor().advance_clock(Duration::from_millis(250));
            visual.run_until_parked();
            if poll % 50 == 49 || poll == 299 || poll == 300 || poll == 599 {
                // The rows are the warn records of the current file, in order.
                let first = written - since_rotation + 1;
                let expected: Vec<usize> = (first..=written)
                    .filter(|n| n.is_multiple_of(10))
                    .map(|n| n - first)
                    .collect();
                assert_eq!(shown(&view, visual), expected, "poll {poll}");
                view.read_with(visual, |view, _| {
                    let follow = view.follow.as_ref().unwrap();
                    assert!(follow.pinned && follow.unseen == 0, "poll {poll}");
                    assert_eq!(
                        follow.reset.map(|reset| reset.0),
                        (poll >= 300).then_some(ResetReason::Rotated),
                        "poll {poll}"
                    );
                });
            }
        }
        assert!(
            written > 1_200,
            "positive control: {written} records written"
        );
    }

    /// #1159: follow mode shows appended records, filters them as they
    /// arrive, pauses when scrolled up with an "N new" pill that resumes it,
    /// and announces a rotation and a truncation instead of a silent reset.
    #[gpui::test]
    fn follow_mode_appends_filters_pauses_and_announces_resets(cx: &mut TestAppContext) {
        use std::io::Write as _;
        let line =
            |level: &str, msg: &str| format!("{{\"level\":\"{level}\",\"msg\":\"{msg}\"}}\n");
        let text: String = (0..60)
            .map(|i| line("info", &format!("tick {i}")))
            .collect();
        let (directory, reader, visual) = open_log(&text, cx);
        let path = directory.path().canonicalize().unwrap().join("app.jsonl");
        let append = |text: &str| {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(text.as_bytes()).unwrap();
        };
        let tick = |visual: &mut VisualTestContext| {
            visual.executor().advance_clock(Duration::from_millis(600));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        let view = log_view(&reader, visual);
        let len = |visual: &mut VisualTestContext| view.read_with(visual, |v, _| v.visible_len());

        // Positive control: without follow, an append is not shown.
        append(&line("error", "early"));
        tick(visual);
        assert_eq!(len(visual), 60);

        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        tick(visual);
        assert_eq!(len(visual), 61, "follow reads the file afresh");
        view.read_with(visual, |view, _| {
            assert!(view.following());
            assert!(view.follow.as_ref().unwrap().pinned);
        });

        // A query applies to records as they arrive.
        view.update(visual, |view, cx| view.set_query("level>=warn".into(), cx));
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [60]);
        append(&(line("warn", "w1") + &line("info", "quiet") + &line("error", "e1")));
        tick(visual);
        assert_eq!(
            shown(&view, visual),
            [60, 61, 63],
            "appended rows are filtered"
        );

        // Scrolling up pauses; new records are counted on a pill.
        view.update(visual, |view, cx| view.set_query(String::new(), cx));
        visual.run_until_parked();
        tick(visual);
        assert_eq!(len(visual), 64);
        let list = visual.debug_bounds("reader-log").unwrap();
        visual.simulate_event(ScrollWheelEvent {
            position: list.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(600.))),
            ..Default::default()
        });
        visual.run_until_parked();
        tick(visual);
        view.read_with(visual, |view, _| {
            assert!(!view.follow.as_ref().unwrap().pinned, "scrolled up: paused")
        });
        append(&(line("info", "n1") + &line("info", "n2") + &line("info", "n3")));
        tick(visual);
        assert_eq!(len(visual), 67);
        view.read_with(visual, |view, _| {
            assert_eq!(view.follow.as_ref().unwrap().unseen, 3)
        });
        let pill = visual
            .debug_bounds("log-follow-new")
            .expect("the N new pill shows");
        visual.simulate_click(pill.center(), Modifiers::default());
        visual.run_until_parked();
        tick(visual);
        view.read_with(visual, |view, _| {
            let follow = view.follow.as_ref().unwrap();
            assert!(follow.pinned && follow.unseen == 0, "resumed");
        });
        assert!(visual.debug_bounds("log-follow-new").is_none());

        // Rotation: move and recreate; the new file is followed and the
        // earlier records are announced, not silently dropped.
        std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
        std::fs::write(&path, line("info", "fresh 1") + &line("warn", "fresh 2")).unwrap();
        tick(visual);
        tick(visual);
        assert_eq!(len(visual), 2, "the new file's records");
        let reset = visual
            .debug_bounds("log-follow-reset")
            .expect("a separator");
        assert!(reset.size.height > px(0.));
        view.read_with(visual, |view, _| {
            assert_eq!(
                view.follow.as_ref().unwrap().reset,
                Some((ResetReason::Rotated, 67))
            );
        });

        // Truncation of the followed file.
        std::fs::write(&path, line("info", "after truncate")).unwrap();
        tick(visual);
        assert_eq!(len(visual), 1);
        view.read_with(visual, |view, _| {
            assert_eq!(
                view.follow.as_ref().unwrap().reset.map(|r| r.0),
                Some(ResetReason::Truncated)
            );
        });

        // Follow off: appends wait until it is on again.
        view.update(visual, |view, cx| view.toggle_follow(cx));
        append(&line("info", "later"));
        tick(visual);
        assert_eq!(len(visual), 1);
        view.update(visual, |view, cx| view.toggle_follow(cx));
        tick(visual);
        assert_eq!(len(visual), 2);
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
