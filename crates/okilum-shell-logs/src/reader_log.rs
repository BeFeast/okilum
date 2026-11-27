//! Read-only structured log view (#602): virtualized one-line rows over a
//! `okilum_core::log` index, a filter bar (text, level, time window and an
//! hl-compatible query, #1158) evaluated off the UI thread, a detail pane for
//! the selected record, a raw-line copy, and follow mode (#1159): the file is
//! polled with `okilum_core::log::tail`, appended records are filtered as they
//! arrive, and a truncation or rotation is announced, not hidden. More files
//! can join the view (#1160): their records interleave in time order through
//! `okilum_core::log::merge`, each with its colour and name, and any file can
//! be hidden without indexing anything again.
use crate::prelude::*;
pub mod chrome;
pub mod filters;
pub mod view_stack;
use filters::FilterUi;
use gpui_component::scroll::ScrollableElement as _;
use okilum_core::log::chips::{self, FilterChip, FilterMode, FilterValue};
use okilum_core::log::correlation;
use okilum_core::log::elapsed::{self, Stamp};
use okilum_core::log::export::{self, CsvColumns, ExportFormat};
use okilum_core::log::histogram::Histogram;
use okilum_core::log::merge::MergedLog;
use okilum_core::log::query::{Expr, FieldRef};
use okilum_core::log::saved_query::SavedQuery;
use okilum_core::log::source_link::{self, Open, SourceLocation};
use okilum_core::log::tail::{ResetReason, TailEvent};
use okilum_core::log::{
    ansi, block, filter, timestamp, FilterGenerations, Format, Level, LogFile, LogFilter, LogIndex,
    LogSnapshot, LogTail, Query, Record, Role, TimeWindow, ValueKind, WindowInfo,
};
use std::ops::Range;
#[path = "reader_log_http.rs"]
pub mod http;
#[path = "reader_log_remote.rs"]
pub mod remote;
#[path = "reader_log_virtual.rs"]
pub mod virtual_sources;

const CONTEXT: &str = "ReaderLog";
pub const ROW_HEIGHT: f32 = 24.;
/// Detail pane height until the user drags it (#1156).
pub const DETAIL_HEIGHT: f32 = 240.;
/// The detail pane keeps its header; the rows keep five lines.
pub const DETAIL_HEADER_HEIGHT: f32 = 36.;
pub const MIN_LIST_ROWS: f32 = 5.;
/// The file name column of a view over several files (#1160).
const SOURCE_NAME_WIDTH: f32 = 140.;
/// Field columns (#1167): narrow for numbers and short values, wide for
/// paths and URLs.
const COLUMN_WIDTH: f32 = 96.;
pub const WIDE_COLUMN_WIDTH: f32 = 240.;
const MIN_COLUMN_WIDTH: f32 = 48.;
const MAX_COLUMN_WIDTH: f32 = 640.;
/// Records looked at for the Columns menu's field names.
const COLUMN_MENU_SAMPLE: usize = 200;
/// The row grid's gap and the widths before the first field column.
const ROW_GAP: f32 = 12.;
const TIME_WIDTH: f32 = 92.;
const BADGE_WIDTH: f32 = 28.;
/// The level histogram above the rows (#1168).
const HISTOGRAM_HEIGHT: f32 = 56.;
/// A press that moves less than this is a click on a bar, not a range.
const HISTOGRAM_DRAG_MIN: f32 = 4.;
/// The most rows one copy takes; a longer selection is exported instead.
const COPY_ROWS_MAX: usize = 20_000;
/// While a log is followed the bars are counted again at most this often.
const HISTOGRAM_FOLLOW_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// A header handle being dragged to resize field column `.0` (#1167).
#[derive(Clone)]
struct ColumnDrag(usize);

impl Render for ColumnDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}
const KEY_WIDTH: f32 = 168.;
/// Characters of a row shaped per frame; longer rows end in an ellipsis and
/// stay complete in the detail pane and the raw copy.
const ROW_TEXT_LIMIT: usize = 480;
const COPY_KEY: &str = "secondary-c";
const FILTER_KEY: &str = "secondary-f";
/// Filter bar controls follow the Reader's 28 px icon-button geometry.
const BAR_CONTROL_HEIGHT: f32 = 28.;
const QUERY_ROW_HEIGHT: f32 = 40.;
const QUERY_MIN_WIDTH: f32 = 80.;
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
        ExtendPreviousRecord,
        ExtendNextRecord,
        CopyRawLine,
        FocusLogFilter,
        ToggleMark,
        NextMark,
        PreviousMark,
        InvestigationBack,
        ToggleLogQuery,
        ToggleLogFollow,
        ClearLogFilters,
        OpenLogLevels,
        OpenLogTimeRange,
        OpenLogSavedViews,
        OpenLogColumns,
        OpenLogExport,
        OpenLogFiles
    ]
);

/// Marks (#1386), as in most editors and LogMX: toggle, next, previous.
const MARK_KEY: &str = "secondary-f2";

pub fn bind_keys(cx: &mut App) {
    #[cfg(target_os = "macos")]
    cx.bind_keys([KeyBinding::new("cmd-[", InvestigationBack, Some(CONTEXT))]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([KeyBinding::new(
        "alt-left",
        InvestigationBack,
        Some(CONTEXT),
    )]);
    cx.bind_keys([
        KeyBinding::new("up", SelectPreviousRecord, Some(CONTEXT)),
        KeyBinding::new("down", SelectNextRecord, Some(CONTEXT)),
        KeyBinding::new("shift-up", ExtendPreviousRecord, Some(CONTEXT)),
        KeyBinding::new("shift-down", ExtendNextRecord, Some(CONTEXT)),
        KeyBinding::new(COPY_KEY, CopyRawLine, Some(CONTEXT)),
        KeyBinding::new(FILTER_KEY, FocusLogFilter, Some(CONTEXT)),
        KeyBinding::new(MARK_KEY, ToggleMark, Some(CONTEXT)),
        KeyBinding::new("f2", NextMark, Some(CONTEXT)),
        KeyBinding::new("shift-f2", PreviousMark, Some(CONTEXT)),
    ]);
}

/// An indexed log, ready to show. `rel` is relative to the Reader root.
#[derive(Clone)]
pub struct LogDocument {
    pub rel: String,
    pub file: Arc<LogFile>,
    pub elapsed: Duration,
}

pub struct RegexEditor {
    pub name: Entity<InputState>,
    pub pattern: Entity<InputState>,
    pub error: Option<String>,
    _events: Subscription,
}

/// One saved definition applies automatically only when detection says Plain.
/// Several definitions require an explicit choice: no invisible winner.
pub fn automatic_regex(cx: &App) -> Option<Arc<okilum_core::log::regex_text::RegexParser>> {
    let definitions = reader_ui_state::log_regex_definitions(cx);
    if definitions.len() == 1 {
        definitions[0].compile().ok()
    } else {
        None
    }
}

fn open_with_regex(
    path: &Path,
    regex: Option<Arc<okilum_core::log::regex_text::RegexParser>>,
    only_plain: bool,
) -> anyhow::Result<LogFile> {
    let file = LogFile::open(path)?;
    match regex {
        Some(regex) if !only_plain || file.index().format() == Format::Plain => {
            file.with_regex(regex)
        }
        _ => Ok(file),
    }
}

pub enum State {
    Indexing,
    Ready(LogDocument),
    Held(Arc<HeldSource>),
    Following { rel: String },
    Failed(String),
    Remote(Arc<okilum_core::remote_log::EventWindow>),
    Stream(Arc<okilum_core::log::stream::Snapshot>),
}

/// What the view reads: the indexed file, or the followed file (#1159).
pub trait LogData {
    fn index(&self) -> &LogIndex;
    fn bytes(&self) -> &[u8];
    fn raw(&self, index: usize) -> Option<&[u8]>;
    fn record(&self, index: usize) -> Option<Record>;
    fn path(&self) -> &Path;
    fn window(&self) -> WindowInfo;
}

impl LogData for okilum_core::remote_log::EventWindow {
    fn index(&self) -> &LogIndex {
        &self.index
    }
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    fn raw(&self, index: usize) -> Option<&[u8]> {
        self.index.raw(&self.bytes, index)
    }
    fn record(&self, index: usize) -> Option<Record> {
        self.index.record(self.raw(index)?)
    }
    fn path(&self) -> &Path {
        &self.path
    }
    fn window(&self) -> WindowInfo {
        WindowInfo {
            end: self.bytes.len() as u64,
            file_len: self.bytes.len() as u64,
            ..Default::default()
        }
    }
}

impl LogData for okilum_core::log::stream::Snapshot {
    fn index(&self) -> &LogIndex {
        self.index()
    }
    fn bytes(&self) -> &[u8] {
        self.bytes()
    }
    fn raw(&self, index: usize) -> Option<&[u8]> {
        self.index().raw(self.bytes(), index)
    }
    fn record(&self, index: usize) -> Option<Record> {
        self.index().record(self.raw(index)?)
    }
    fn path(&self) -> &Path {
        self.path()
    }
    fn window(&self) -> WindowInfo {
        self.window()
    }
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
    fn window(&self) -> WindowInfo {
        LogFile::window(self)
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
    fn window(&self) -> WindowInfo {
        LogTail::window(self)
    }
}

/// A copy of what a background filter run reads.
pub enum Snapshot {
    File(Arc<LogFile>),
    Segment(Arc<Segment>),
    Held(Arc<LogSnapshot>),
    Remote(Arc<okilum_core::remote_log::EventWindow>),
    Stream(Arc<okilum_core::log::stream::Snapshot>),
}

/// The name being typed to save the shown filter (#1169).
pub struct SavingFilter {
    pub input: Entity<InputState>,
    error: Option<String>,
    _subscription: Subscription,
}

/// Which rows an export takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportScope {
    /// Every row the filters show.
    Shown,
    /// The selected rows.
    Selected,
    /// The marked rows the filters show (#1386).
    Marked,
}

/// The rows of an export, as the view held them when it was asked for.
enum ExportIds {
    All(usize),
    List(Arc<Vec<u32>>),
    Picked(Vec<u32>),
}

/// What a background export reads: copies of the shown files and the rows to
/// write, so a followed log growing meanwhile changes nothing (#1169).
struct ExportJob {
    /// Each shown file with its name, in the order of the merged view.
    files: Vec<(Snapshot, String)>,
    merged: Option<MergedLog>,
    ids: ExportIds,
}

impl ExportJob {
    fn count(&self) -> usize {
        match &self.ids {
            ExportIds::All(count) => *count,
            ExportIds::List(rows) => rows.len(),
            ExportIds::Picked(rows) => rows.len(),
        }
    }

    /// File and entry of row `id`.
    pub fn locate(&self, id: usize) -> Option<(usize, usize)> {
        match &self.merged {
            Some(log) => log.get(id).map(|row| (row.file().index(), row.entry())),
            None => Some((0, id)),
        }
    }

    /// Writes every row in order; returns how many.
    fn write(&self, out: &mut dyn std::io::Write, format: &ExportFormat) -> std::io::Result<usize> {
        let several = self.merged.is_some();
        export::write_header(out, format)?;
        let mut written = 0;
        let mut one = |id: usize| -> std::io::Result<()> {
            let Some((file, entry)) = self.locate(id) else {
                return Ok(());
            };
            let Some((snapshot, name)) = self.files.get(file) else {
                return Ok(());
            };
            let (index, data) = snapshot.parts();
            let Some(entry) = index.get(entry) else {
                return Ok(());
            };
            let row = export::Row {
                window: Some(snapshot.window()),
                data,
                entry,
                format: index.format(),
                regex: index.regex().map(|regex| regex.as_ref()),
                source: several.then_some(name.as_str()),
            };
            export::write_row(out, &row, format)?;
            written += 1;
            Ok(())
        };
        match &self.ids {
            ExportIds::All(count) => (0..*count).try_for_each(&mut one)?,
            ExportIds::List(rows) => rows.iter().try_for_each(|&id| one(id as usize))?,
            ExportIds::Picked(rows) => rows.iter().try_for_each(|&id| one(id as usize))?,
        }
        Ok(written)
    }
}

/// The index of a source for a background count, without its bytes.
struct IndexCopy(Snapshot);
impl IndexCopy {
    fn get(&self) -> &LogIndex {
        self.0.parts().0
    }
}

impl Snapshot {
    fn window(&self) -> WindowInfo {
        match self {
            Self::Remote(data) => data.window(),
            Self::Stream(data) => data.window(),
            Self::File(file) => file.window(),
            Self::Segment(segment) => segment.window(),
            Self::Held(snapshot) => snapshot.window(),
        }
    }
    fn parts(&self) -> (&LogIndex, &[u8]) {
        match self {
            Self::Remote(data) => (&data.index, &data.bytes),
            Self::Stream(data) => (data.index(), data.bytes()),
            Snapshot::File(file) => (file.index(), file.bytes()),
            Snapshot::Segment(segment) => (segment.index(), segment.bytes()),
            Snapshot::Held(snapshot) => (snapshot.index(), snapshot.bytes()),
        }
    }
}

/// The file once follow mode has opened it. Turning follow off keeps what
/// was read, so the rows do not jump back to the older indexed file.
pub struct Follow {
    pub tail: LogTail,
    /// Polling; `None` while follow is off.
    poll: Option<Task<()>>,
    /// The newest row stays in view until the user scrolls up.
    pub pinned: bool,
    /// Rows added while not pinned: the count on the "N new" pill.
    pub unseen: usize,
    /// The last truncation or rotation, shown above the rows.
    pub reset: Option<(ResetReason, usize)>,
    /// The poll could not read the file (it may be being rotated).
    error: Option<String>,
    /// When a full run last started for a time window that moves with appends.
    pub last_full_run: Option<std::time::Instant>,
}

/// A file added to the view (#1160).
pub struct ExtraSource {
    pub data: SourceData,
    enabled: bool,
    /// The source a segment came from (its own index for an added file),
    /// and the segment's place among that source's segments: a file's
    /// earlier content comes before it when nothing else orders them.
    owner: usize,
    seq: usize,
}

pub enum SourceData {
    File(Arc<LogFile>),
    /// A file's content before follow mode saw it rotated or truncated
    /// (#1159, #1160): kept, listed under its own name, never polled.
    Segment(Arc<Segment>),
    /// An added file while follow mode is on.
    Tail(Box<LogTail>),
    Held(Arc<HeldSource>),
}

impl SourceData {
    pub fn data(&self) -> &dyn LogData {
        match self {
            SourceData::File(file) => file.as_ref(),
            SourceData::Held(source) => source.as_ref(),
            SourceData::Segment(segment) => segment.as_ref(),
            SourceData::Tail(tail) => tail.as_ref(),
        }
    }
}

/// An immutable revision from an externally owned virtual-source session.
struct HeldSource {
    label: Option<&'static str>,
    path: PathBuf,
    snapshot: Arc<LogSnapshot>,
}
impl LogData for HeldSource {
    fn index(&self) -> &LogIndex {
        self.snapshot.index()
    }
    fn bytes(&self) -> &[u8] {
        self.snapshot.bytes()
    }
    fn raw(&self, row: usize) -> Option<&[u8]> {
        self.snapshot.raw(row)
    }
    fn record(&self, row: usize) -> Option<Record> {
        self.snapshot.record(row)
    }
    fn path(&self) -> &Path {
        &self.path
    }
    fn window(&self) -> WindowInfo {
        self.snapshot.window()
    }
}

/// The finished content of a followed file, under a label of its own.
pub struct Segment {
    path: PathBuf,
    pub segment: okilum_core::log::tail::LogSegment,
}

impl LogData for Segment {
    fn index(&self) -> &LogIndex {
        self.segment.index()
    }
    fn bytes(&self) -> &[u8] {
        self.segment.bytes()
    }
    fn raw(&self, index: usize) -> Option<&[u8]> {
        self.segment.raw(index)
    }
    fn record(&self, index: usize) -> Option<Record> {
        self.segment.record(index)
    }
    fn path(&self) -> &Path {
        &self.path
    }
    fn window(&self) -> WindowInfo {
        self.segment.window()
    }
}

/// Several shown sources in time order. Rows of the view are rows of `log`.
pub struct Merge {
    log: MergedLog,
    /// The source behind each merged file.
    sources: Vec<usize>,
    /// For each merged file, the row of each of its entries.
    positions: Arc<Vec<Vec<u32>>>,
}

/// Per-file stripe colours, from the theme's chart series.
fn source_color(source: usize, cx: &App) -> Hsla {
    let theme = cx.theme();
    [
        theme.chart_1,
        theme.chart_2,
        theme.chart_3,
        theme.chart_4,
        theme.chart_5,
    ][source % 5]
}

pub struct LogView {
    pub state: State,
    pub hosted_header: bool,
    pane_bounds: Rc<std::cell::Cell<Bounds<Pixels>>>,
    query_open: bool,
    pub command_menu: Option<chrome::CommandMenu>,
    /// Entry index (into the log index) of the selected record.
    pub selected: Option<usize>,
    /// Where a range of selected rows started (#1169); `selected` is its other end.
    pub anchor: Option<usize>,
    /// Marked records (#1386) as (source, entry): they keep their record when
    /// the query, the filter or the shown files change and while follow adds
    /// rows. Only while this view is open: never stored (Oleg, 18.10).
    pub marks: std::collections::BTreeSet<(usize, usize)>,
    /// A CSV export writes the cells as they are in the log, without the
    /// formula guard (#1387 follow-up). Off by default; only while this view is
    /// open, like the marks.
    pub csv_raw_values: bool,
    /// An export is being chosen or written; one at a time.
    exporting: bool,
    pub scroll: UniformListScrollHandle,
    pub focus: FocusHandle,
    pub filter: LogFilter,
    pub filter_ui: FilterUi,
    generations: FilterGenerations,
    /// Entry indices shown, ascending; `None` while every entry is shown.
    pub rows: Option<Arc<Vec<u32>>>,
    /// A run for the latest filter is under way; the previous rows stay up.
    pub filtering: bool,
    pub filter_again: bool,
    pub filter_running: Option<u64>,
    pub text_input: Entity<InputState>,
    pub query_input: Entity<InputState>,
    /// The query field's text as typed; the filter keeps the last one that
    /// parsed, so a typo never clears the rows.
    pub query_text: String,
    /// Column (1-based, in characters) and message of the typed query's error.
    pub query_error: Option<(usize, String)>,
    pub detail_split: reader_split::Split,
    pub follow: Option<Follow>,
    /// Entries a background run covered; appends past it are filtered when
    /// its result lands.
    filtered_upto: usize,
    /// A wheel event happened: the next frame decides whether the list is at
    /// its bottom, pinning or pausing follow.
    check_pin: bool,
    /// A session stream's owner follows its bottom (#3010): `Some(true)`
    /// keeps the newest row in view, `Some(false)` after scrolling up keeps
    /// the viewport where it is while new rows still arrive. Independent of
    /// whether data is being received; `None` keeps the earlier behavior.
    stream_bottom: Option<bool>,
    /// Follow could not open the file.
    follow_failed: Option<String>,
    _follow_open: Option<Task<()>>,
    /// Files added to this view (#1160); the view's own file is source 0.
    pub extra: Vec<ExtraSource>,
    /// Source 0 is shown.
    primary_enabled: bool,
    /// Definition-selected immutable sources cannot acquire unrelated files or
    /// reopen canonical paths through the ordinary file Follow controls.
    source_locked: bool,
    /// The time order of the shown sources, when there is more than one.
    pub merge: Option<Merge>,
    /// Showing one request (#1166): the query text to go back to.
    pub investigation: view_stack::Investigation,
    /// Records per level over time for the shown rows (#1168), the rows it
    /// was counted for, and where the bars were last drawn.
    pub histogram: Option<Arc<Histogram>>,
    histogram_for: Option<(usize, usize, u64)>,
    histogram_running: Option<(usize, usize, u64)>,
    window_revision: u64,
    pub history_limited: bool,
    history_reclaim_pending: bool,
    histogram_at: Option<std::time::Instant>,
    histogram_bounds: Rc<std::cell::Cell<Bounds<Pixels>>>,
    /// A press on the bars: where it started and where the pointer is.
    histogram_press: Option<(Pixels, Pixels)>,
    _histogram: Option<Task<()>>,
    /// A count that waits for the next second of a followed log.
    _histogram_wait: Option<Task<()>>,
    /// Naming the shown filter to save it (#1169).
    pub saving: Option<SavingFilter>,
    pub regex_editor: Option<RegexEditor>,
    regex_error: Option<String>,
    /// What the last source-link click asked for, instead of launching (#1165).
    #[cfg(any(test, feature = "test-support"))]
    pub last_open: Option<Open>,
    _text_input_events: Subscription,
    _query_input_events: Subscription,
    _filtering: Option<Task<()>>,
    _indexing: Option<Task<()>>,
}

impl LogView {
    pub fn stream(
        data: Arc<okilum_core::log::stream::Snapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(State::Stream(data), window, cx)
    }

    pub fn replace_stream(
        &mut self,
        data: Arc<okilum_core::log::stream::Snapshot>,
        cx: &mut Context<Self>,
    ) {
        let positions: std::collections::HashMap<u64, usize> = data
            .index()
            .entries()
            .iter()
            .enumerate()
            .map(|(i, entry)| (entry.offset(), i))
            .collect();
        let old = match &self.state {
            State::Stream(old) if old.scope() == data.scope() => Some(old),
            _ => None,
        };
        let remap = |i: usize| {
            old.and_then(|old| old.index().get(i))
                .and_then(|entry| positions.get(&entry.offset()).copied())
        };
        self.selected = self.selected.and_then(remap);
        self.anchor = self.anchor.and_then(remap);
        self.marks = self
            .marks
            .iter()
            .filter_map(|&(source, i)| (source == 0).then(|| remap(i)).flatten().map(|i| (0, i)))
            .collect();
        self.generations.next();
        self.window_revision = self.window_revision.wrapping_add(1);
        self.histogram_for = None;
        self._filtering = None;
        self.filter_running = None;
        self.filter_again = false;
        self.histogram = None;
        self._histogram = None;
        self.state = State::Stream(data);
        self.rows = None;
        self.refilter(cx);
        self.after_rows_changed();
        cx.notify();
    }
    pub fn remote(
        data: Arc<okilum_core::remote_log::EventWindow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(State::Remote(data), window, cx)
    }

    pub fn replace_remote(
        &mut self,
        data: Arc<okilum_core::remote_log::EventWindow>,
        cx: &mut Context<Self>,
    ) {
        let positions = match &self.state {
            State::Remote(old) if old.identity_scope == data.identity_scope => {
                let identity = |event: &serde_json::Value| -> Option<(String, String)> {
                    Some((
                        event["_index"].as_str()?.into(),
                        event["_id"].as_str()?.into(),
                    ))
                };
                let next: std::collections::HashMap<_, _> = data
                    .events
                    .iter()
                    .enumerate()
                    .filter_map(|(index, event)| Some((identity(event)?, index)))
                    .collect();
                old.events
                    .iter()
                    .map(|event| identity(event).and_then(|key| next.get(&key).copied()))
                    .collect::<Vec<_>>()
            }
            _ => Vec::new(),
        };
        let remap = |index: usize| positions.get(index).copied().flatten();
        self.selected = self.selected.and_then(remap);
        self.anchor = self.anchor.and_then(remap);
        self.marks = self
            .marks
            .iter()
            .filter_map(|&(source, index)| {
                if source == 0 {
                    remap(index).map(|index| (0, index))
                } else {
                    None
                }
            })
            .collect();
        self.generations.next();
        self._filtering = None;
        // The old task can no longer clear this flag. Start a filter for the
        // replacement now, rather than queueing behind its cancelled task.
        self.filtering = false;
        self.filter_running = None;
        self.filter_again = false;
        self.histogram = None;
        self._histogram = None;
        self.state = State::Remote(data);
        self.rows = None;
        self.refilter(cx);
        self.after_rows_changed();
        cx.notify();
    }

    fn new(state: State, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in records"));
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
        let remote = matches!(&state, State::Remote(_) | State::Stream(_));
        let query_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(if remote {
                "Local loaded-row query (not KQL)"
            } else {
                "Query: level>=warn and status>=500"
            })
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
            hosted_header: false,
            pane_bounds: Rc::new(std::cell::Cell::new(Bounds::default())),
            query_open: false,
            command_menu: None,
            selected: None,
            anchor: None,
            marks: Default::default(),
            csv_raw_values: false,
            exporting: false,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            filter: LogFilter::default(),
            filter_ui: FilterUi::default(),
            generations: FilterGenerations::default(),
            rows: None,
            filtering: false,
            filter_again: false,
            filter_running: None,
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
            stream_bottom: None,
            follow_failed: None,
            _follow_open: None,
            extra: Vec::new(),
            primary_enabled: true,
            source_locked: false,
            merge: None,
            investigation: Default::default(),
            histogram: None,
            histogram_for: None,
            histogram_running: None,
            window_revision: 0,
            history_limited: false,
            history_reclaim_pending: false,
            histogram_at: None,
            histogram_bounds: Rc::default(),
            histogram_press: None,
            _histogram: None,
            _histogram_wait: None,
            saving: None,
            regex_editor: None,
            regex_error: None,
            #[cfg(any(test, feature = "test-support"))]
            last_open: None,
            _text_input_events: events,
            _query_input_events: query_events,
            _filtering: None,
            _indexing: None,
        }
    }

    pub fn ready(document: LogDocument, window: &mut Window, cx: &mut Context<Self>) -> Self {
        record_ready(&document, cx);
        let apply = (document.file.index().format() == Format::Plain)
            .then(|| automatic_regex(cx))
            .flatten();
        let mut view = Self::new(State::Ready(document), window, cx);
        if let Some(regex) = apply {
            view.apply_regex(Some(regex), cx);
        }
        view.reveal_tail();
        view
    }

    /// Indexes `path` off the UI thread; the view says so meanwhile.
    pub fn indexing(
        rel: String,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let regex = automatic_regex(cx);
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let started = std::time::Instant::now();
                    open_with_regex(&path, regex, true).map(|file| (file, started.elapsed()))
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
                this.refilter(cx);
                this.reveal_tail();
                cx.notify();
            });
        });
        Self {
            _indexing: Some(task),
            ..Self::new(State::Indexing, window, cx)
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn compressed(&self) -> bool {
        self.file().is_some_and(|file| file.is_compressed())
    }

    pub fn file(&self) -> Option<&Arc<LogFile>> {
        match &self.state {
            State::Ready(document) => Some(&document.file),
            _ => None,
        }
    }

    /// The records shown: the followed file once follow has opened it.
    pub fn data(&self) -> Option<&dyn LogData> {
        if let State::Held(source) = &self.state {
            return Some(source.as_ref());
        }
        if let State::Remote(data) = &self.state {
            return Some(data.as_ref());
        }
        if let State::Stream(data) = &self.state {
            return Some(data.as_ref());
        }
        match &self.follow {
            Some(follow) => Some(&follow.tail),
            None => self.file().map(|file| file.as_ref() as &dyn LogData),
        }
    }

    pub fn snapshot_of(&self, source: usize) -> Option<Snapshot> {
        if source == 0 {
            if let State::Held(source) = &self.state {
                return Some(Snapshot::Held(source.snapshot.clone()));
            }
            if let State::Remote(data) = &self.state {
                return Some(Snapshot::Remote(data.clone()));
            }
            if let State::Stream(data) = &self.state {
                return Some(Snapshot::Stream(data.clone()));
            }
        }
        match (source, &self.follow) {
            (0, Some(follow)) => Some(Snapshot::Held(follow.tail.snapshot())),
            (0, None) => self.file().cloned().map(Snapshot::File),
            (n, _) => self.extra.get(n - 1).map(|extra| match &extra.data {
                SourceData::File(file) => Snapshot::File(file.clone()),
                SourceData::Held(source) => Snapshot::Held(source.snapshot.clone()),
                SourceData::Segment(segment) => Snapshot::Segment(segment.clone()),
                SourceData::Tail(tail) => Snapshot::Held(tail.snapshot()),
            }),
        }
    }

    /// Source `source`: 0 is the view's own file (or its followed copy).
    pub fn source(&self, source: usize) -> Option<&dyn LogData> {
        match source {
            0 => self.data(),
            n => self.extra.get(n - 1).map(|extra| extra.data.data()),
        }
    }

    pub fn source_count(&self) -> usize {
        1 + self.extra.len()
    }

    fn source_enabled(&self, source: usize) -> bool {
        match source {
            0 => self.primary_enabled,
            n => self.extra.get(n - 1).is_some_and(|extra| extra.enabled),
        }
    }

    /// Shown sources in merge order: each file after its earlier segments.
    fn enabled_sources(&self) -> Vec<usize> {
        let mut sources: Vec<usize> = (0..self.source_count())
            .filter(|&source| self.source_enabled(source) && self.source(source).is_some())
            .collect();
        sources.sort_by_key(|&source| match source {
            0 => (0, 1, 0),
            n => {
                let extra = &self.extra[n - 1];
                let live = !matches!(extra.data, SourceData::Segment(_));
                (extra.owner, usize::from(live), extra.seq)
            }
        });
        sources
    }

    /// The newest timestamp of the shown sources: where a time window ends.
    fn newest(&self) -> Option<i64> {
        self.enabled_sources()
            .into_iter()
            .filter_map(|source| {
                let index = self.source(source)?.index();
                index.entries().iter().filter_map(|e| e.timestamp()).max()
            })
            .max()
    }

    pub fn source_name(&self, source: usize) -> String {
        let name = self
            .source(source)
            .and_then(|data| data.path().file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let held = match source {
            0 => match &self.state {
                State::Held(source) => Some(source),
                _ => None,
            },
            n => self.extra.get(n - 1).and_then(|extra| match &extra.data {
                SourceData::Held(source) => Some(source),
                _ => None,
            }),
        };
        match held.and_then(|source| source.label) {
            Some(label) => format!("{name} ({label})"),
            None => name,
        }
    }

    /// Rows of the view before any filter: every record of the shown sources.
    pub fn total(&self) -> usize {
        match &self.merge {
            Some(merge) => merge.log.len(),
            None => self
                .enabled_sources()
                .first()
                .and_then(|&source| self.source(source))
                .map_or(0, |data| data.index().len()),
        }
    }

    /// Source and entry of row id `id`.
    pub fn locate(&self, id: usize) -> Option<(usize, usize)> {
        match &self.merge {
            Some(merge) => merge
                .log
                .get(id)
                .map(|row| (merge.sources[row.file().index()], row.entry())),
            None => {
                let source = *self.enabled_sources().first()?;
                (id < self.source(source)?.index().len()).then_some((source, id))
            }
        }
    }

    /// The row id of `entry` of `source`, if that source is shown.
    pub fn id_of(&self, source: usize, entry: usize) -> Option<usize> {
        match &self.merge {
            Some(merge) => {
                let file = merge.sources.iter().position(|&s| s == source)?;
                merge.positions[file].get(entry).map(|row| *row as usize)
            }
            None => (self.enabled_sources().first() == Some(&source)).then_some(entry),
        }
    }

    pub fn raw_at(&self, id: usize) -> Option<&[u8]> {
        let (source, entry) = self.locate(id)?;
        self.source(source)?.raw(entry)
    }

    /// Orders the shown sources again; entries are not indexed again. The
    /// selection keeps its record when that record is still shown.
    fn rebuild_merge(&mut self) {
        let selected = self.selected.and_then(|id| self.locate(id));
        let sources = self.enabled_sources();
        self.merge = (sources.len() > 1).then(|| {
            let indexes: Vec<&LogIndex> = sources
                .iter()
                .filter_map(|&source| self.source(source).map(LogData::index))
                .collect();
            let log = MergedLog::build(&indexes);
            let mut positions: Vec<Vec<u32>> =
                indexes.iter().map(|index| vec![0; index.len()]).collect();
            for (row, merged) in log.rows().iter().enumerate() {
                positions[merged.file().index()][merged.entry()] = row as u32;
            }
            Merge {
                log,
                sources,
                positions: Arc::new(positions),
            }
        });
        self.selected = selected.and_then(|(source, entry)| self.id_of(source, entry));
        self.anchor = None;
    }

    pub fn set_source_enabled(&mut self, source: usize, enabled: bool, cx: &mut Context<Self>) {
        match source {
            0 => self.primary_enabled = enabled,
            n => match self.extra.get_mut(n - 1) {
                Some(extra) => extra.enabled = enabled,
                None => return,
            },
        }
        self.rebuild_merge();
        self.refilter(cx);
        cx.notify();
    }

    /// Indexes `paths` off the UI thread and adds them to the view (#1160);
    /// while following, they are followed too.
    pub fn add_files(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.source_locked {
            return;
        }
        let regex = automatic_regex(cx);
        cx.spawn(async move |this, cx| {
            let opened = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .map(|path| {
                            open_with_regex(&path, regex.clone(), true)
                                .map_err(|error| (path, error))
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                for file in opened {
                    match file {
                        Ok(file) => {
                            let known = (0..this.source_count()).any(|source| {
                                this.source(source).map(LogData::path) == Some(file.path())
                            });
                            if !known {
                                let owner = this.source_count();
                                this.extra.push(ExtraSource {
                                    data: SourceData::File(Arc::new(file)),
                                    enabled: true,
                                    owner,
                                    seq: 0,
                                });
                            }
                        }
                        Err((path, error)) => {
                            this.follow_failed = Some(format!("{}: {error:#}", path.display()));
                        }
                    }
                }
                this.rebuild_merge();
                this.refilter(cx);
                // Following: the new files are followed too.
                if this.following() {
                    this.open_tails(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Log files dropped on the view join it (#1160); other files are
    /// named and left out.
    fn drop_files(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        if self.source_locked {
            reader_toast::transient(
                "Change virtual source criteria and Rescan to add files",
                window,
                cx,
            );
            return;
        }
        let (logs, others): (Vec<PathBuf>, Vec<PathBuf>) = paths
            .paths()
            .iter()
            .cloned()
            .partition(|path| okilum_core::log::is_log_path(path));
        if let Some(other) = others.first() {
            let name = other.file_name().unwrap_or_default().to_string_lossy();
            reader_toast::transient(format!("Not a log file: {name}"), window, cx);
        }
        if !logs.is_empty() {
            self.add_files(logs, cx);
        }
    }

    /// The file is already a source of this view.
    pub fn has_file(&self, path: &Path) -> bool {
        (0..self.source_count()).any(|source| self.source(source).map(LogData::path) == Some(path))
    }

    pub fn add_file_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.add_files(paths, cx);
    }

    fn prompt_add_files(&mut self, cx: &mut Context<Self>) {
        if self.source_locked {
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add to this log view".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = this.update(cx, |this, cx| this.add_files(paths, cx));
            }
        })
        .detach();
    }

    /// Opens a source location of the selected record in the chosen editor
    /// (#1165). A relative path is looked up next to the log first.
    pub fn open_location(
        &mut self,
        location: &SourceLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let base = self
            .selected
            .and_then(|id| self.locate(id))
            .and_then(|(source, _)| self.source(source))
            .and_then(|data| data.path().parent().map(Path::to_path_buf))
            .unwrap_or_default();
        // A build made elsewhere opens in the local checkout (#1165).
        let maps = reader_ui_state::source_maps_for(&base, cx);
        let path = source_link::map(&location.path, &maps)
            .unwrap_or_else(|| source_link::resolve(&location.path, &base));
        let editor = reader_ui_state::log_editor(cx);
        match source_link::open(&editor, &path, location.line, location.column) {
            Ok(open) => self.launch(open, window, cx),
            Err(error) => reader_toast::error(error, window, cx),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn launch(&mut self, open: Open, _: &mut Window, _: &mut Context<Self>) {
        self.last_open = Some(open);
    }

    #[cfg(not(any(test, feature = "test-support")))]
    fn launch(&mut self, open: Open, window: &mut Window, cx: &mut Context<Self>) {
        match open {
            Open::Url(url) => cx.open_url(&url),
            Open::Command(words) => {
                let (program, args) = words.split_first().expect("a non-empty command");
                if let Err(error) = std::process::Command::new(program)
                    .args(args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    reader_toast::error(format!("Could not start {program}: {error}"), window, cx);
                }
            }
        }
    }

    /// `text` with its source locations as links (#1165).
    fn linked(
        &self,
        id: impl Into<ElementId>,
        text: String,
        locations: Vec<SourceLocation>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if locations.is_empty() {
            return text.into_any_element();
        }
        let link = brand::palette(cx).link;
        let highlights = locations
            .iter()
            .map(|location| {
                (
                    location.span.clone(),
                    HighlightStyle {
                        color: Some(link),
                        underline: Some(UnderlineStyle {
                            thickness: px(1.),
                            color: Some(link),
                            wavy: false,
                        }),
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();
        let ranges = locations.iter().map(|l| l.span.clone()).collect();
        let view = cx.entity().downgrade();
        let tips = locations.clone();
        InteractiveText::new(id, StyledText::new(text).with_highlights(highlights))
            .on_click(ranges, move |ix, window, cx| {
                cx.stop_propagation();
                let location = locations[ix].clone();
                let _ = view.update(cx, |this, cx| this.open_location(&location, window, cx));
            })
            .tooltip(move |ix, window, cx| {
                let location = tips.iter().find(|l| l.span.contains(&ix))?;
                Some(
                    gpui_component::tooltip::Tooltip::new(format!(
                        "Open {}:{} in the editor",
                        location.path, location.line
                    ))
                    .build(window, cx),
                )
            })
            .into_any_element()
    }

    /// Field columns of this view (#1167), remembered per file.
    pub fn columns(&self, cx: &App) -> Vec<String> {
        self.history_key()
            .map(|key| reader_ui_state::log_columns(&key, cx))
            .unwrap_or_default()
    }

    /// What a CSV export writes as columns (#1387): the field columns shown
    /// now, in their order, and the file's name when several files are merged.
    /// Cells carry the formula guard unless "Raw values" is on.
    pub fn csv_columns(&self, cx: &App) -> CsvColumns {
        CsvColumns::new(self.columns(cx), self.merge.is_some()).with_raw_values(self.csv_raw_values)
    }

    /// The "Raw values in CSV" switch of the export menu.
    pub fn set_csv_raw_values(&mut self, raw: bool, cx: &mut Context<Self>) {
        if self.csv_raw_values != raw {
            self.csv_raw_values = raw;
            cx.notify();
        }
    }

    /// Column `key`'s width: the dragged one, or the default for its key.
    fn width_of(&self, key: &str, cx: &App) -> f32 {
        self.history_key()
            .and_then(|file| reader_ui_state::log_column_width(&file, key, cx))
            .unwrap_or_else(|| column_width(key))
    }

    /// Where field column `at` starts, from the left edge of a row: the
    /// rows' fixed cells and the widths of the columns before it.
    fn column_start(&self, at: usize, cx: &App) -> f32 {
        let merged = self.merge.is_some();
        let mut x = 12. + 2.;
        if merged {
            x += 3. + ROW_GAP;
        }
        x += TIME_WIDTH + ROW_GAP;
        if merged {
            x += SOURCE_NAME_WIDTH + ROW_GAP;
        }
        x += BADGE_WIDTH + ROW_GAP;
        for key in self.columns(cx).iter().take(at) {
            x += self.width_of(key, cx) + ROW_GAP;
        }
        x
    }

    fn resize_column(&mut self, at: usize, width: f32, cx: &mut Context<Self>) {
        let (Some(file), Some(key)) = (self.history_key(), self.columns(cx).get(at).cloned())
        else {
            return;
        };
        let width = width.clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH);
        reader_ui_state::set_log_column_width(&file, &key, width, cx);
        cx.notify();
    }

    /// Field names of the first records of the shown files, in the order
    /// first seen: the choices of the Columns menu (#1167).
    pub fn field_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        // A named group remains available even if it is optional or its first
        // match lies beyond the column menu's bounded record sample.
        for source in 0..self.source_count() {
            if let Some(regex) = self.source(source).and_then(|data| data.index().regex()) {
                for name in regex.named_groups() {
                    if names.len() >= 256 {
                        return names;
                    }
                    if !names.iter().any(|known| known == name) {
                        names.push(name.to_owned());
                    }
                }
            }
        }
        let total = self.total().min(COLUMN_MENU_SAMPLE);
        for id in 0..total {
            let Some((source, entry)) = self.locate(id) else {
                continue;
            };
            let Some(record) = self.source(source).and_then(|data| data.record(entry)) else {
                continue;
            };
            for field in record.fields {
                if names.len() >= 256 {
                    return names;
                }
                if !names.contains(&field.key) {
                    names.push(field.key);
                }
            }
        }
        names
    }

    pub fn toggle_column(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(file) = self.history_key() else {
            return;
        };
        let mut columns = reader_ui_state::log_columns(&file, cx);
        match columns.iter().position(|column| column == key) {
            Some(at) => {
                columns.remove(at);
            }
            None if columns.len() < 256 => columns.push(key.to_owned()),
            None => return,
        }
        reader_ui_state::set_log_columns(&file, columns, cx);
        cx.notify();
    }

    /// Counts the shown rows per level over time off the UI thread when they
    /// changed since the last count (#1168). While a log is followed a count
    /// starts at most once a second, like a time window that moves with it.
    fn recount(&mut self, cx: &mut Context<Self>) {
        let signature = (self.visible_len(), self.total(), self.window_revision);
        if self.histogram_for == Some(signature)
            || self._histogram_wait.is_some()
            || self.histogram_running.is_some()
        {
            return;
        }
        if let Some(last) = self.histogram_at.filter(|_| self.following()) {
            let wait = HISTOGRAM_FOLLOW_EVERY.saturating_sub(last.elapsed());
            if !wait.is_zero() {
                self._histogram_wait = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    let _ = this.update(cx, |this, cx| {
                        // The wait is over, whatever the wall clock says.
                        this._histogram_wait = None;
                        this.histogram_at = None;
                        this.recount(cx);
                    });
                }));
                return;
            }
        }
        self.histogram_for = Some(signature);
        self.histogram_running = Some(signature);
        self.histogram_at = Some(std::time::Instant::now());
        let sources: Vec<usize> = match &self.merge {
            Some(merge) => merge.sources.clone(),
            None => self.enabled_sources().into_iter().take(1).collect(),
        };
        let indexes: Vec<IndexCopy> = sources
            .iter()
            .filter_map(|&source| self.index_copy(source))
            .collect();
        let merged = self.merge.as_ref().map(|merge| merge.log.clone());
        let rows = self.rows.clone();
        self._histogram = Some(cx.spawn(async move |this, cx| {
            let counted = cx
                .background_executor()
                .spawn(async move {
                    let mut histogram = Histogram::new();
                    match (&rows, &merged) {
                        (None, _) => {
                            for index in &indexes {
                                histogram.add_index(index.get(), None);
                            }
                        }
                        (Some(rows), None) => {
                            if let Some(index) = indexes.first() {
                                histogram.add_index(index.get(), Some(rows.as_slice()));
                            }
                        }
                        (Some(rows), Some(log)) => {
                            // The rows of a merged view are placed by time;
                            // the histogram wants each file's entries in order.
                            let mut entries: Vec<Vec<u32>> = vec![Vec::new(); indexes.len()];
                            for &id in rows.iter() {
                                if let Some(row) = log.get(id as usize) {
                                    if let Some(file) = entries.get_mut(row.file().index()) {
                                        file.push(row.entry() as u32);
                                    }
                                }
                            }
                            for (index, mut file) in indexes.iter().zip(entries) {
                                file.sort_unstable();
                                histogram.add_index(index.get(), Some(&file));
                            }
                        }
                    }
                    histogram
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.histogram_running != Some(signature) {
                    return;
                }
                this.histogram_running = None;
                this._histogram = None;
                if this.histogram_for == Some(signature) {
                    this.histogram = (!counted.is_empty()).then(|| Arc::new(counted));
                    cx.notify();
                }
                this.recount(cx);
            });
        }));
    }

    /// The index of `source` for a background count: shared where the source
    /// is a file, a copy of the index (not of the bytes) where it grows.
    fn index_copy(&self, source: usize) -> Option<IndexCopy> {
        self.snapshot_of(source).map(IndexCopy)
    }

    /// The bar under `x`, or `None` before the first count.
    fn histogram_bucket(&self, x: Pixels) -> Option<usize> {
        let histogram = self.histogram.as_ref()?;
        let bounds = self.histogram_bounds.get();
        if bounds.size.width <= px(0.) || histogram.is_empty() {
            return None;
        }
        let fraction = ((x - bounds.left()) / bounds.size.width).clamp(0., 1.);
        Some(((fraction * histogram.len() as f32) as usize).min(histogram.len() - 1))
    }

    /// A press on the bars ended: a drag selects the time the bars it covers
    /// span, a click jumps to the first shown row of the bar under it.
    fn release_histogram(&mut self, x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let Some((start, _)) = self.histogram_press.take() else {
            return;
        };
        cx.notify();
        let (Some(a), Some(b), Some(histogram)) = (
            self.histogram_bucket(start),
            self.histogram_bucket(x),
            self.histogram.clone(),
        ) else {
            return;
        };
        if (x - start).abs() >= px(HISTOGRAM_DRAG_MIN) {
            if let Some((from, to)) = histogram.window(a, b) {
                self.update_filter(|filter| filter.time = TimeWindow::range(from, to), cx);
            }
        } else if let Some(bucket) = histogram.bucket(b) {
            self.jump_to_time(bucket.from, window, cx);
        }
    }

    /// Selects the first shown row whose own time is at or after `ts`.
    fn jump_to_time(&mut self, ts: i64, window: &mut Window, cx: &mut Context<Self>) {
        let single = match self.merge {
            Some(_) => None,
            None => self
                .enabled_sources()
                .first()
                .and_then(|&source| self.source(source))
                .map(LogData::index),
        };
        let target = (0..self.visible_len()).find_map(|row| {
            let id = self.entry_at(row)?;
            let time = match single {
                Some(index) => index.get(id)?.timestamp()?,
                None => {
                    let (source, entry) = self.locate(id)?;
                    self.source(source)?.index().get(entry)?.timestamp()?
                }
            };
            (time >= ts).then_some(id)
        });
        if let Some(id) = target {
            self.select(id, true, window, cx);
        }
    }

    fn render_histogram(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.recount(cx);
        let histogram = self.histogram.clone()?;
        let tallest = histogram.max_total().max(1);
        let order = [
            Level::Fatal,
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Debug,
            Level::Trace,
            Level::Unknown,
            Level::Missing,
            Level::Unparsed,
        ];
        let bounds = self.histogram_bounds.clone();
        let selection = self.histogram_press.map(|(a, b)| (a.min(b), (a - b).abs()));
        let origin = self.histogram_bounds.get().left();
        let range_label = match self.filter.time {
            TimeWindow::Range { from, to } => Some(format!(
                "{} – {}",
                timestamp::datetime_utc(from),
                timestamp::clock_utc(to)
            )),
            _ => None,
        };
        let untimed = histogram.untimed();
        let accent = brand::palette(cx).accent;
        let muted = cx.theme().muted_foreground;
        Some(
            v_flex()
                .flex_none()
                .px_3()
                .pt_1()
                .gap_1()
                .border_b_1()
                .border_color(brand::palette(cx).border_subtle)
                .child(
                    div()
                        .id("log-histogram")
                        .debug_selector(|| "log-histogram".into())
                        .relative()
                        .h(px(HISTOGRAM_HEIGHT))
                        .w_full()
                        .flex()
                        .items_end()
                        .gap(px(1.))
                        .cursor_crosshair()
                        .children(histogram.buckets().enumerate().map(|(at, bucket)| {
                            let total = bucket.total();
                            let height = HISTOGRAM_HEIGHT * total as f32 / tallest as f32;
                            v_flex()
                                .id(("log-bar", at))
                                .flex_1()
                                .h(px(height))
                                .children(order.iter().filter_map(|level| {
                                    let count = bucket.counts[*level as usize];
                                    (count > 0).then(|| {
                                        div()
                                            .w_full()
                                            .h(px(height * count as f32 / total.max(1) as f32))
                                            .bg(level_color(*level, cx))
                                    })
                                }))
                        }))
                        .child(
                            canvas(move |area, _, _| bounds.set(area), |_, _, _, _| {})
                                .absolute()
                                .size_full(),
                        )
                        .when_some(selection, |bars, (left, width)| {
                            bars.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left(left - origin)
                                    .w(width)
                                    .bg(accent.opacity(0.2)),
                            )
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                this.histogram_press = Some((event.position.x, event.position.x));
                                cx.notify();
                            }),
                        )
                        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                            if let Some((start, _)) = this.histogram_press {
                                if event.dragging() {
                                    this.histogram_press = Some((start, event.position.x));
                                    cx.notify();
                                }
                            }
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseUpEvent, window, cx| {
                                this.release_histogram(event.position.x, window, cx)
                            }),
                        ),
                )
                .child(
                    h_flex()
                        .h(px(18.))
                        .gap_2()
                        .text_size(px(11.))
                        .text_color(muted)
                        .child(
                            histogram
                                .start()
                                .map(timestamp::datetime_utc)
                                .unwrap_or_default(),
                        )
                        .child(div().flex_1())
                        .children(range_label.map(|label| {
                            h_flex()
                                .gap_1()
                                .debug_selector(|| "log-histogram-range".into())
                                .child(format!("Selected {label}"))
                                .child(
                                    Button::new("log-histogram-clear")
                                        .debug_selector(|| "log-histogram-clear".into())
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Close)
                                        .tooltip("Clear the time range")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.update_filter(
                                                |filter| filter.time = TimeWindow::All,
                                                cx,
                                            )
                                        })),
                                )
                        }))
                        .child(div().flex_1())
                        .when(untimed > 0, |row| {
                            row.child(format!("{untimed} without a time"))
                        })
                        .child(
                            histogram
                                .end()
                                .map(|end| timestamp::clock_utc(end - 1))
                                .unwrap_or_default(),
                        ),
                )
                .into_any_element(),
        )
    }

    pub fn following(&self) -> bool {
        self.follow
            .as_ref()
            .is_some_and(|follow| follow.poll.is_some())
    }

    /// Rows currently listed.
    pub fn visible_len(&self) -> usize {
        match &self.rows {
            Some(rows) => rows.len(),
            None => self.total(),
        }
    }

    /// The entry shown at list position `row`.
    pub fn entry_at(&self, row: usize) -> Option<usize> {
        match &self.rows {
            Some(rows) => rows.get(row).map(|entry| *entry as usize),
            None => (row < self.visible_len()).then_some(row),
        }
    }

    /// The list position of entry `index`, if it is listed.
    pub fn row_of(&self, index: usize) -> Option<usize> {
        match &self.rows {
            Some(rows) if self.investigation.request.is_some() => {
                rows.iter().position(|&row| row as usize == index)
            }
            Some(rows) => rows.binary_search(&(index as u32)).ok(),
            None => (index < self.visible_len()).then_some(index),
        }
    }

    pub fn select(
        &mut self,
        index: usize,
        reveal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some(index);
        self.anchor = None;
        if let Some(row) = self.row_of(index).filter(|_| reveal) {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Moves the end of the selected rows to `index`, keeping the other end
    /// where it was (shift-click, shift-arrows).
    pub fn extend_to(
        &mut self,
        index: usize,
        reveal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(anchor) = self.anchor.or(self.selected) else {
            return self.select(index, reveal, window, cx);
        };
        self.select(index, reveal, window, cx);
        self.anchor = (anchor != index).then_some(anchor);
    }

    /// The list positions of the selected rows: one, or the range between the
    /// anchor and the selected record.
    pub fn selection(&self) -> Option<Range<usize>> {
        let cursor = self.row_of(self.selected?)?;
        match self.anchor.and_then(|anchor| self.row_of(anchor)) {
            Some(anchor) => Some(anchor.min(cursor)..anchor.max(cursor) + 1),
            None => Some(cursor..cursor + 1),
        }
    }

    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.step_by(delta, false, window, cx)
    }

    /// The list positions of the marked rows the filters show, in order.
    pub fn marked_rows(&self) -> Vec<usize> {
        let mut rows: Vec<usize> = self
            .marks
            .iter()
            .filter_map(|&(source, entry)| self.row_of(self.id_of(source, entry)?))
            .collect();
        rows.sort_unstable();
        rows
    }

    /// Marks the selected rows, or removes their marks when all of them are
    /// marked already.
    pub fn toggle_mark(&mut self, cx: &mut Context<Self>) {
        let Some(rows) = self.selection() else {
            return;
        };
        let keys: Vec<(usize, usize)> = rows
            .filter_map(|row| self.locate(self.entry_at(row)?))
            .collect();
        if keys.iter().all(|key| self.marks.contains(key)) {
            for key in &keys {
                self.marks.remove(key);
            }
        } else {
            self.marks.extend(keys);
        }
        cx.notify();
    }

    /// The row menu's toggle: the selected rows when `id` is one of them,
    /// otherwise just row `id`.
    pub fn toggle_mark_at(&mut self, id: usize, cx: &mut Context<Self>) {
        let in_selection = self
            .row_of(id)
            .zip(self.selection())
            .is_some_and(|(row, rows)| rows.contains(&row));
        if !in_selection {
            self.selected = Some(id);
            self.anchor = None;
        }
        self.toggle_mark(cx);
    }

    /// The row menu's next and previous: counted from row `id`.
    fn step_mark_from(
        &mut self,
        id: usize,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some(id);
        self.anchor = None;
        self.step_mark(forward, window, cx);
    }

    fn clear_marks(&mut self, cx: &mut Context<Self>) {
        self.marks.clear();
        cx.notify();
    }

    /// The next (or previous) marked row the filters show, wrapping at the
    /// ends. Following, a mark above the newest row pauses follow, as
    /// scrolling up does.
    pub fn step_mark(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.marked_rows();
        if rows.is_empty() {
            let message = if self.marks.is_empty() {
                "No marked rows. Mark one with the row menu"
            } else {
                "No marked row is shown"
            };
            reader_toast::error(message, window, cx);
            return;
        }
        let current = self.selected.and_then(|id| self.row_of(id));
        let target = if forward {
            current
                .and_then(|row| rows.iter().find(|&&marked| marked > row))
                .or(rows.first())
        } else {
            current
                .and_then(|row| rows.iter().rev().find(|&&marked| marked < row))
                .or(rows.last())
        };
        let Some(&row) = target else {
            return;
        };
        if row + 1 < self.visible_len() {
            if let Some(follow) = self.follow.as_mut().filter(|follow| follow.pinned) {
                follow.pinned = false;
            }
        }
        if let Some(id) = self.entry_at(row) {
            self.select(id, true, window, cx);
        }
    }

    fn step_by(&mut self, delta: isize, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
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
            if extend {
                self.extend_to(index, true, window, cx);
            } else {
                self.select(index, true, window, cx);
            }
        }
    }

    pub fn update_filter(&mut self, change: impl FnOnce(&mut LogFilter), cx: &mut Context<Self>) {
        let before = self.filter.clone();
        change(&mut self.filter);
        if self.filter != before {
            self.refilter(cx);
        }
    }

    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.update_filter(|filter| filter.text = text, cx);
    }

    /// A query that parses replaces the filter's; one that does not leaves
    /// the last valid query (and its rows) in place and shows where it fails.
    pub fn set_query(&mut self, text: String, cx: &mut Context<Self>) {
        match Query::parse(&text) {
            Ok(query) => {
                self.query_error = None;
                let (mut chips, advanced) = chips::split_query(&query);
                for chip in &mut chips {
                    if let Some(old) = self
                        .filter_ui
                        .chips
                        .iter()
                        .find(|old| old.enabled && old.expr() == chip.expr())
                    {
                        chip.execution = old.execution;
                    }
                }
                chips.extend(
                    self.filter_ui
                        .chips
                        .iter()
                        .filter(|chip| !chip.enabled)
                        .cloned(),
                );
                self.filter_ui.chips = chips;
                self.filter_ui.advanced = advanced;
                let query = self.local_chip_query();
                self.emit_chip_change(cx);
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

    pub fn replace_query(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.query_input
            .update(cx, |input, cx| input.set_value(text.clone(), window, cx));
        self.set_query(text, cx);
    }

    /// A detail value becomes a typed chip in the shared query model.
    pub fn add_field_term(
        &mut self,
        key: &str,
        value: FilterValue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_chip(
            FilterChip::new(FieldRef::Key(key.into()), vec![value], FilterMode::Include),
            window,
            cx,
        );
    }

    /// True when the applied query already requires `term`.
    fn has_term(&self, term: &Expr) -> bool {
        match self.filter.query.as_ref().map(Query::expr) {
            Some(Expr::And(items)) => items.contains(term),
            Some(expr) => expr == term,
            None => false,
        }
    }

    pub fn history_key(&self) -> Option<String> {
        self.data()
            .map(|data| data.path().to_string_lossy().into_owned())
    }

    pub fn remember_query(&mut self, cx: &mut Context<Self>) {
        if self.query_error.is_some() {
            return;
        }
        if let (Some(key), Some(query)) = (self.history_key(), &self.filter.query) {
            reader_ui_state::remember_log_query(&key, &query.to_string(), cx);
        }
    }

    pub fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.text_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.query_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.query_text.clear();
        self.query_open = false;
        self.regex_editor = None;
        self.regex_error = None;
        self.filter_ui.chips.clear();
        self.filter_ui.advanced.clear();
        self.query_error = None;
        self.update_filter(|filter| *filter = LogFilter::default(), cx);
        if let Some(request) = self
            .investigation
            .request
            .clone()
            .filter(|_| !self.filter_ui.server)
        {
            self.replace_query(
                correlation::query_with_field(&request.value, Some(&request.key)).to_string(),
                window,
                cx,
            );
        }
        self.emit_chip_change(cx);
        cx.notify();
    }

    /// Starts a run for the current filter. Every earlier run is cancelled by
    /// the new generation, and a result that arrives late is dropped.
    fn refilter(&mut self, cx: &mut Context<Self>) {
        // One background filter at a time. Cancellation is cooperative; merely
        // dropping its UI task must not create a queue of retained snapshots.
        if self.filtering {
            self.generations.next();
            self.filter_again = true;
            return;
        }
        if self.merge.is_some() {
            return self.refilter_merged(cx);
        }
        let ticket = self.generations.next();
        let Some(snapshot) = self
            .enabled_sources()
            .first()
            .and_then(|&source| self.snapshot_of(source))
        else {
            self.rows = None;
            self.filtering = false;
            self._filtering = None;
            cx.notify();
            return;
        };
        self.filtered_upto = snapshot.parts().0.len();
        if !self.filter.is_active() && self.investigation.request.is_none() {
            self.rows = None;
            self.filtering = false;
            self._filtering = None;
            self.after_rows_changed();
            cx.notify();
            return;
        }
        self.filtering = true;
        self.filter_running = Some(ticket.generation());
        let filter = self.filter_for_source(self.enabled_sources().first().copied().unwrap_or(0));
        self._filtering = Some(cx.spawn(async move |this, cx| {
            let generation = ticket.generation();
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let (index, bytes) = snapshot.parts();
                    filter::evaluate(&filter, index, bytes, &ticket)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.filter_running != Some(generation) {
                    return;
                }
                this.filter_running = None;
                this._filtering = None;
                this.filtering = false;
                if this.filter_again {
                    this.filter_again = false;
                    this.refilter(cx);
                    return;
                }
                let Some(outcome) = outcome else {
                    return;
                };
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

    /// The run over several files (#1160): each file is filtered on its own,
    /// with the time window ending at the newest record of all of them, and
    /// its matches are placed at their rows of the merged view.
    fn refilter_merged(&mut self, cx: &mut Context<Self>) {
        let ticket = self.generations.next();
        if !self.filter.is_active() && self.investigation.request.is_none() {
            self.rows = None;
            self.filtering = false;
            self._filtering = None;
            self.after_rows_changed();
            cx.notify();
            return;
        }
        let Some(merge) = &self.merge else {
            return;
        };
        let source_names: Vec<_> = merge
            .sources
            .iter()
            .map(|&source| self.source_name(source))
            .collect();
        let snapshots: Vec<Snapshot> = merge
            .sources
            .iter()
            .filter_map(|&source| self.snapshot_of(source))
            .collect();
        let positions = merge.positions.clone();
        self.filtering = true;
        self.filter_running = Some(ticket.generation());
        let filter = self.investigation_filter();
        self._filtering = Some(cx.spawn(async move |this, cx| {
            let generation = ticket.generation();
            let rows = cx
                .background_executor()
                .spawn(async move {
                    let window_end = snapshots
                        .iter()
                        .filter_map(|snapshot| {
                            snapshot
                                .parts()
                                .0
                                .entries()
                                .iter()
                                .filter_map(|e| e.timestamp())
                                .max()
                        })
                        .max();
                    let filter = LogFilter {
                        window_end,
                        ..filter
                    };
                    let mut rows = Vec::new();
                    for (file, snapshot) in snapshots.iter().enumerate() {
                        let (index, bytes) = snapshot.parts();
                        let mut filter = filter.clone();
                        filter.query = filter
                            .query
                            .as_ref()
                            .map(|q| q.for_source(&source_names[file]));
                        let outcome = filter::evaluate(&filter, index, bytes, &ticket)?;
                        rows.extend(
                            outcome
                                .rows
                                .into_iter()
                                .map(|entry| positions[file][entry as usize]),
                        );
                    }
                    rows.sort_unstable();
                    Some(rows)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.filter_running != Some(generation) {
                    return;
                }
                this.filter_running = None;
                this._filtering = None;
                this.filtering = false;
                if this.filter_again {
                    this.filter_again = false;
                    this.refilter(cx);
                    return;
                }
                let Some(rows) = rows else {
                    return;
                };
                if !this.generations.is_current(generation) {
                    return;
                }
                this.rows = Some(Arc::new(rows));
                this.filtering = false;
                this.after_rows_changed();
                // Following and pinned: the newest match, not the top (#1282).
                this.keep_pinned();
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Follow on, off, or on again. Every file of the view is followed: the
    /// view's own file and the added ones, read afresh with `LogTail`.
    pub fn toggle_follow(&mut self, cx: &mut Context<Self>) {
        if self.source_locked {
            return;
        }
        match &mut self.follow {
            Some(follow) if follow.poll.is_some() => {
                follow.poll = None;
                cx.notify();
            }
            _ => self.open_tails(cx),
        }
    }

    /// Opens a tail for the view's own file (the first time) and for each
    /// added file not followed yet, off the UI thread, then polls them all.
    fn open_tails(&mut self, cx: &mut Context<Self>) {
        let primary = match &self.follow {
            Some(_) => None,
            None => match self.data() {
                Some(data) => Some((data.path().to_owned(), data.index().regex().cloned())),
                None => return,
            },
        };
        let added: Vec<_> = self
            .extra
            .iter()
            .enumerate()
            .filter_map(|(slot, extra)| match &extra.data {
                SourceData::File(file) => Some((
                    slot,
                    (file.path().to_owned(), file.index().regex().cloned()),
                )),
                _ => None,
            })
            .collect();
        self.follow_failed = None;
        self._follow_open = Some(cx.spawn(async move |this, cx| {
            let (primary, added) = cx
                .background_executor()
                .spawn(async move {
                    let open = |(path, regex): (
                        PathBuf,
                        Option<Arc<okilum_core::log::regex_text::RegexParser>>,
                    )| {
                        let interval = okilum_core::log::tail::poll_interval_for(&path);
                        LogTail::open(&path).and_then(|tail| {
                            let tail = tail.with_interval(interval);
                            match regex {
                                Some(regex) => tail.with_regex(regex),
                                None => Ok(tail),
                            }
                        })
                    };
                    let primary = primary.map(open);
                    let added: Vec<_> = added
                        .into_iter()
                        .map(|(slot, path)| (slot, open(path)))
                        .collect();
                    (primary, added)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let mut changed_sources = Vec::new();
                match primary {
                    Some(Ok(tail)) => {
                        if this.data().is_some_and(|old| {
                            old.window() != tail.window()
                                || old.index().len() != tail.index().len()
                                || old.bytes() != tail.bytes()
                        }) {
                            changed_sources.push(0);
                        }
                        if let State::Ready(document) = &this.state {
                            let rel = document.rel.clone();
                            this.state = State::Following { rel };
                        }
                        this.follow = Some(Follow {
                            tail,
                            poll: None,
                            pinned: true,
                            unseen: 0,
                            reset: None,
                            error: None,
                            last_full_run: None,
                        })
                    }
                    Some(Err(error)) => {
                        this.follow_failed = Some(format!("{error:#}"));
                        cx.notify();
                        return;
                    }
                    None => {}
                }
                for (slot, tail) in added {
                    match (tail, this.extra.get_mut(slot)) {
                        (Ok(tail), Some(extra)) if matches!(extra.data, SourceData::File(_)) => {
                            let old = extra.data.data();
                            if old.window() != tail.window()
                                || old.index().len() != tail.index().len()
                                || old.bytes() != tail.bytes()
                            {
                                changed_sources.push(slot + 1);
                            }
                            extra.data = SourceData::Tail(Box::new(tail))
                        }
                        (Err(error), _) => this.follow_failed = Some(format!("{error:#}")),
                        _ => {}
                    }
                }
                if let Some(follow) = this.follow.as_mut() {
                    follow.pinned = true;
                    follow.unseen = 0;
                    follow.poll = Some(Self::start_poll(cx));
                }
                // The file can grow/slide between static open and enabling
                // Follow. Only identical windows preserve entry identities.
                if !changed_sources.is_empty() {
                    this.selected = None;
                    this.anchor = None;
                    this.marks
                        .retain(|(source, _)| !changed_sources.contains(source));
                    this.rows = None;
                    this.filtered_upto = 0;
                    this.window_revision = this.window_revision.wrapping_add(1);
                    this.histogram_for = None;
                }
                this.rebuild_merge();
                this.refilter(cx);
                this.keep_pinned();
                cx.notify();
            });
        }));
    }

    /// Polls at the view's tail interval: every followed file is read in the
    /// background and indexed here. Dropping the task stops it.
    fn start_poll(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| loop {
            let Ok(Some(interval)) = this.update(cx, |this, _| {
                this.follow.as_ref().map(|follow| follow.tail.interval())
            }) else {
                return;
            };
            cx.background_executor().timer(interval).await;
            let Ok(requests) = this.update(cx, |this, cx| this.begin_polls(cx)) else {
                return;
            };
            if requests.is_empty() {
                return;
            }
            let reads = cx
                .background_executor()
                .spawn(async move {
                    requests
                        .into_iter()
                        .map(|(source, request)| (source, request.read()))
                        .collect::<Vec<_>>()
                })
                .await;
            if this
                .update(cx, |this, cx| this.apply_polls(reads, cx))
                .is_err()
            {
                return;
            }
        })
    }

    pub fn begin_polls(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Vec<(usize, okilum_core::log::tail::PollRequest)> {
        // A soft process-wide watermark can reflect unrelated views. Do not
        // discard this view's history on a successful/no-op poll. A failed
        // reservation requests reclamation for the next poll, before source
        // IDs are captured (never midway through applying a multi-file batch).
        let reclaim = std::mem::take(&mut self.history_reclaim_pending);
        if reclaim
            && self
                .extra
                .iter()
                .any(|extra| matches!(extra.data, SourceData::Segment(_)))
        {
            let old = std::mem::take(&mut self.extra);
            let mut mapping = vec![Some(0)];
            for extra in old {
                if matches!(extra.data, SourceData::Segment(_)) {
                    mapping.push(None);
                } else {
                    mapping.push(Some(self.extra.len() + 1));
                    self.extra.push(extra);
                }
            }
            for (slot, extra) in self.extra.iter_mut().enumerate() {
                extra.owner = slot + 1;
            }
            self.marks = std::mem::take(&mut self.marks)
                .into_iter()
                .filter_map(|(source, entry)| {
                    Some((mapping.get(source).copied().flatten()?, entry))
                })
                .collect();
            self.history_limited = true;
            self.invalidate_window(usize::MAX, cx);
        }
        let Some(follow) = self.follow.as_mut() else {
            return Vec::new();
        };
        let mut requests = vec![(0, follow.tail.begin_poll())];
        for (slot, extra) in self.extra.iter_mut().enumerate() {
            if let SourceData::Tail(tail) = &mut extra.data {
                requests.push((slot + 1, tail.begin_poll()));
            }
        }
        requests
    }

    fn tail_mut(&mut self, source: usize) -> Option<&mut LogTail> {
        match source {
            0 => self.follow.as_mut().map(|follow| &mut follow.tail),
            n => match &mut self.extra.get_mut(n - 1)?.data {
                SourceData::Tail(tail) => Some(tail.as_mut()),
                _ => None,
            },
        }
    }

    fn apply_polls(
        &mut self,
        reads: Vec<(usize, anyhow::Result<okilum_core::log::tail::PollRead>)>,
        cx: &mut Context<Self>,
    ) {
        self.investigation.last_update = Some(timestamp::clock_utc(unix_now()));
        for (source, read) in reads {
            self.apply_poll(source, read, cx);
        }
        self.sort_request_rows();
        self.keep_pinned();
        cx.notify();
    }

    pub fn apply_poll(
        &mut self,
        source: usize,
        read: anyhow::Result<okilum_core::log::tail::PollRead>,
        cx: &mut Context<Self>,
    ) {
        let event = match read {
            Ok(read) => {
                if source == 0 {
                    if let Some(follow) = self.follow.as_mut() {
                        follow.error = None;
                    }
                }
                match self.tail_mut(source) {
                    Some(tail) => tail.apply(read),
                    None => return,
                }
            }
            Err(error) => {
                self.history_reclaim_pending |=
                    error.is::<okilum_core::log::window_budget::BudgetBusy>();
                let name = self.source_name(source);
                if let Some(follow) = self.follow.as_mut() {
                    follow.error = Some(if source == 0 {
                        format!("{error:#}")
                    } else {
                        format!("{name}: {error:#}")
                    });
                }
                return;
            }
        };
        match event {
            TailEvent::Unchanged | TailEvent::Stale => {}
            TailEvent::Deferred(error) => {
                self.history_reclaim_pending |=
                    error.is::<okilum_core::log::window_budget::BudgetBusy>();
                if let Some(follow) = self.follow.as_mut() {
                    follow.error = Some(format!("{error:#}"));
                }
            }
            TailEvent::WindowChanged => {
                self.invalidate_window(source, cx);
            }
            TailEvent::Appended {
                entries,
                reindexed,
                grown,
            } => {
                if reindexed {
                    self.invalidate_window(source, cx);
                    return;
                }
                let time_moves = self.filter.time.follows_newest();
                let due = self.follow.as_ref().is_some_and(|follow| {
                    follow
                        .last_full_run
                        .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1))
                });
                let merged = self.merge.is_some();
                if reindexed || (time_moves && due) || (grown && merged) {
                    // Every row may change: earlier entries were indexed
                    // again, the newest record moved the time window, or a
                    // merged record took new lines.
                    if let Some(follow) = self.follow.as_mut() {
                        follow.last_full_run = Some(std::time::Instant::now());
                    }
                    if merged {
                        self.rebuild_merge();
                    }
                    self.refilter(cx);
                } else {
                    let added = if merged {
                        self.append_merged(source, entries, cx)
                    } else {
                        // An entry that took new lines (a stack trace written
                        // line by line, #1161) is filtered again with them; it
                        // is not a new row.
                        let mut entries = entries;
                        let mut regrown = 0;
                        if grown && entries.start > 0 {
                            entries.start -= 1;
                            regrown = usize::from(self.drop_last_row(entries.start));
                        }
                        self.extend_rows(entries).saturating_sub(regrown)
                    };
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
                // The earlier content stays in the view under its own name
                // (#1160), before the new content of the same file.
                let earlier = previous.index().len();
                if source == 0 {
                    if let Some(follow) = self.follow.as_mut() {
                        follow.reset = Some((reason, earlier));
                        follow.unseen = 0;
                    }
                }
                let keep_history = !previous.window().tail
                    && previous.bytes().len() <= 2 * 1024 * 1024
                    && previous.snapshot().reserved_bytes() <= 12 * 1024 * 1024;
                if earlier > 0 && !keep_history {
                    self.history_limited = true;
                }
                if earlier > 0 && keep_history {
                    let what = match reason {
                        ResetReason::Truncated => "before truncation",
                        ResetReason::Rotated => "before rotation",
                    };
                    let name = self.source_name(source);
                    let seq = self
                        .extra
                        .iter()
                        .filter(|extra| matches!(extra.data, SourceData::Segment(_)))
                        .map(|extra| extra.seq)
                        .max()
                        .map_or(0, |seq| seq + 1);
                    // Marks on the earlier content move with it (#1386): the
                    // segment keeps its entries, the file starts again at 0.
                    let retained: Vec<_> = self
                        .extra
                        .iter()
                        .enumerate()
                        .filter(|(_, extra)| matches!(extra.data, SourceData::Segment(_)))
                        .map(|(slot, _)| slot)
                        .collect();
                    let recycle = (retained.len() >= 6).then(|| {
                        *retained
                            .iter()
                            .min_by_key(|&&slot| self.extra[slot].seq)
                            .unwrap()
                    });
                    let segment = recycle.map_or(self.extra.len() + 1, |slot| slot + 1);
                    if recycle.is_some() {
                        self.history_limited = true;
                        self.marks.retain(|&(at, _)| at != segment);
                    }
                    self.marks = std::mem::take(&mut self.marks)
                        .into_iter()
                        .map(|(at, entry)| {
                            if at == source && entry < earlier {
                                (segment, entry)
                            } else {
                                (at, entry)
                            }
                        })
                        .collect();
                    let extra = ExtraSource {
                        data: SourceData::Segment(Arc::new(Segment {
                            path: PathBuf::from(format!("{name} ({what})")),
                            segment: previous,
                        })),
                        enabled: true,
                        owner: source,
                        seq,
                    };
                    if let Some(slot) = recycle {
                        self.extra[slot] = extra;
                    } else {
                        self.extra.push(extra);
                    }
                }
                // Nothing earlier to keep: marks on this file have no record.
                if earlier == 0 || !keep_history {
                    self.marks.retain(|&(at, _)| at != source);
                }
                self.invalidate_window(source, cx);
            }
        }
    }

    /// New entries `entries` of `source` in a merged view: placed at the end
    /// in time order when they are the newest, then filtered into the rows.
    /// Anything else rebuilds the merge and filters again.
    fn append_merged(
        &mut self,
        source: usize,
        entries: Range<usize>,
        cx: &mut Context<Self>,
    ) -> usize {
        let Some(mut merge) = self.merge.take() else {
            return 0;
        };
        let Some(file) = merge.sources.iter().position(|&s| s == source) else {
            self.merge = Some(merge);
            return 0;
        };
        let before = merge.log.len();
        let update = {
            let indexes: Vec<&LogIndex> = merge
                .sources
                .iter()
                .filter_map(|&source| self.source(source).map(LogData::index))
                .collect();
            merge
                .log
                .append(&indexes, okilum_core::log::merge::FileId::new(file))
        };
        match update {
            okilum_core::log::MergeUpdate::Appended(added)
                if added == entries.len() && merge.positions[file].len() == entries.start =>
            {
                Arc::make_mut(&mut merge.positions)[file]
                    .extend((before..before + added).map(|row| row as u32));
                let positions = merge.positions.clone();
                self.merge = Some(merge);
                if self.rows.is_none() {
                    return added;
                }
                let matched = match self.source(source) {
                    Some(data) => {
                        let filter = LogFilter {
                            window_end: self
                                .filter
                                .time
                                .follows_newest()
                                .then(|| self.newest())
                                .flatten(),
                            ..self.filter_for_source(source)
                        };
                        filter::evaluate_range(&filter, data.index(), data.bytes(), entries)
                    }
                    None => Vec::new(),
                };
                let rows: Vec<u32> = matched
                    .into_iter()
                    .map(|entry| positions[file][entry as usize])
                    .collect();
                let count = rows.len();
                if let Some(shown) = self.rows.as_mut() {
                    Arc::make_mut(shown).extend(rows);
                }
                count
            }
            _ => {
                self.merge = Some(merge);
                self.rebuild_merge();
                self.refilter(cx);
                0
            }
        }
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
            Some(data) => filter::evaluate_range(
                &self.filter_for_source(0),
                data.index(),
                data.bytes(),
                range,
            ),
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

    /// Session streams (#3010): follow the bottom or stay where the reader
    /// scrolled. Turning it on jumps to the newest row now.
    pub fn set_stream_bottom(&mut self, follow: bool, cx: &mut Context<Self>) {
        self.stream_bottom = Some(follow);
        self.keep_pinned();
        cx.notify();
    }
    pub fn stream_bottom(&self) -> Option<bool> {
        self.stream_bottom
    }

    /// The session stream shown, for evidence export (#3012).
    pub fn stream_snapshot(&self) -> Option<Arc<okilum_core::log::stream::Snapshot>> {
        match &self.state {
            State::Stream(snapshot) => Some(snapshot.clone()),
            _ => None,
        }
    }

    /// The filter the reader applied, exactly as chosen, and what it shows.
    pub fn filter_evidence(&self) -> serde_json::Value {
        serde_json::json!({
            "active": self.filter.is_active(),
            "query": self.query_text,
            "text": self.filter.text,
            "level": format!("{:?}", self.filter.level),
            "time": format!("{:?}", self.filter.time),
            "fields": self
                .filter
                .chips
                .iter()
                .map(|chip| format!("{}={}", chip.key, chip.value))
                .collect::<Vec<_>>(),
            "rows_shown": self.visible_len(),
            "rows_loaded": self.total(),
            "rows_marked": self.marks.len(),
            // Counts are of a filter run still in progress when true.
            "filtering_in_progress": self.filtering,
        })
    }

    /// Following and pinned: the newest row stays in view.
    fn keep_pinned(&mut self) {
        let pinned = (self.following() && self.follow.as_ref().is_some_and(|follow| follow.pinned))
            || self.stream_bottom == Some(true);
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
        if self.follow.is_none() && self.stream_bottom.is_some() {
            let handle = self.scroll.0.borrow();
            let offset = handle.base_handle.offset();
            let max = handle.base_handle.max_offset();
            let at_bottom = -offset.y >= max.y - px(2.);
            drop(handle);
            self.stream_bottom = Some(at_bottom);
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
        self.sort_request_rows();
        self.window_revision = self.window_revision.wrapping_add(1);
        self.histogram_for = None;
        if self
            .anchor
            .is_some_and(|anchor| self.row_of(anchor).is_none())
        {
            self.anchor = None;
        }
        match self.selected.and_then(|index| self.row_of(index)) {
            Some(row) => self.scroll.scroll_to_item(row, ScrollStrategy::Nearest),
            None => {
                self.selected = None;
                self.anchor = None;
                // A stream reader who scrolled away keeps the viewport while
                // rows arrive (#3010); pinned streams go to the bottom below.
                if self.visible_len() > 0 && self.stream_bottom.is_none() {
                    self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
            }
        }
        self.restore_viewport();
    }

    fn reveal_tail(&self) {
        if self.data().is_some_and(|data| data.window().tail) && self.visible_len() > 0 {
            self.scroll
                .scroll_to_item(self.visible_len() - 1, ScrollStrategy::Bottom);
        }
    }
    fn invalidate_window(&mut self, source: usize, cx: &mut Context<Self>) {
        self.selected = None;
        self.anchor = None;
        self.marks.retain(|&(at, _)| at != source);
        self.rows = None;
        self.filtered_upto = 0;
        self.window_revision = self.window_revision.wrapping_add(1);
        self.histogram_for = None;
        self.rebuild_merge();
        self.refilter(cx);
        self.after_rows_changed();
    }
    pub fn window_notice(&self) -> Option<String> {
        if matches!(&self.state, State::Stream(_)) {
            return Some("Session stream — only complete loaded lines are shown. Filters, counts and export cover these rows. Loading stops at 8 MiB / 20,000 lines; incomplete lines are retained until newline or Stop.".into());
        }
        if matches!(&self.state, State::Remote(_) | State::Stream(_)) {
            return Some("Remote history — only loaded events are shown. Search, local filters and histogram cover these rows, not the full server result. Use Export loaded JSONL for original events and identities.".into());
        }
        let windows: Vec<_> = self
            .enabled_sources()
            .into_iter()
            .filter_map(|source| self.source(source))
            .map(|data| data.window())
            .collect();
        if windows.iter().any(|w| w.tail) || self.history_limited {
            let mut text = "End of file only — search, filters, counts and export cover the loaded window. Older rows are discarded as Follow advances; their selections and marks are cleared.".to_owned();
            if windows.iter().any(|w| w.oversized) {
                text.push_str(" Oversized records were excluded (64 KiB source / 1 MiB decoded-field budget).");
            }
            if self.history_limited {
                text.push_str(" Earlier rotated data was discarded.");
            }
            Some(text)
        } else {
            None
        }
    }

    pub fn focus_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.text_input
            .update(cx, |input, cx| input.focus(window, cx));
    }

    /// Copies the selected record's raw lines; several records are copied as
    /// they are in the file, one after another.
    pub fn copy_raw(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = self.selection() else {
            return;
        };
        if range.len() == 1 {
            let Some(raw) = self.selected.and_then(|index| self.raw_at(index)) else {
                return;
            };
            let text = String::from_utf8_lossy(raw).into_owned();
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            reader_toast::transient("Copied raw line", window, cx);
            return;
        }
        if range.len() > COPY_ROWS_MAX {
            reader_toast::error(
                format!(
                    "Too many rows to copy ({} selected, at most {}): export them instead",
                    grouped(range.len() as u64),
                    grouped(COPY_ROWS_MAX as u64)
                ),
                window,
                cx,
            );
            return;
        }
        let mut bytes = Vec::new();
        let mut ending = 0;
        for row in range.clone() {
            let Some((file, entry)) = self
                .entry_at(row)
                .and_then(|id| self.locate(id))
                .and_then(|(source, local)| Some((self.source(source)?, local)))
                .and_then(|(file, local)| Some((file, file.index().get(local)?)))
            else {
                continue;
            };
            let (raw, line_ending) = export::raw_with_ending(file.bytes(), entry);
            if bytes.len() + raw.len() + line_ending.len() > 4 * 1024 * 1024 {
                reader_toast::error(
                    "Selection exceeds the 4 MiB copy budget; export it instead",
                    window,
                    cx,
                );
                return;
            }
            bytes.extend_from_slice(raw);
            bytes.extend_from_slice(line_ending);
            ending = line_ending.len();
        }
        // Lines, not a file: no ending after the last one.
        bytes.truncate(bytes.len().saturating_sub(ending));
        cx.write_to_clipboard(ClipboardItem::new_string(
            String::from_utf8_lossy(&bytes).into_owned(),
        ));
        reader_toast::transient(
            format!("Copied {} records", grouped(range.len() as u64)),
            window,
            cx,
        );
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let this = &*self;
        let merged = this.merge.is_some();
        let columns = this.columns(cx);
        let message_field = this.message_field(cx);
        let widths: Vec<f32> = columns.iter().map(|key| this.width_of(key, cx)).collect();
        let palette = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        let hover = brand::reader_palette(cx).hover;
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let chosen = this.selection();
        let mark_color = cx.theme().primary;
        range
            .map(|position| {
                let Some((index, source, file, local, entry)) =
                    this.entry_at(position).and_then(|index| {
                        let (source, local) = this.locate(index)?;
                        let file = this.source(source)?;
                        Some((index, source, file, local, file.index().get(local)?))
                    })
                else {
                    return div().into_any_element();
                };
                let level = entry.level();
                let record = file.record(local);
                let source_name = this.source_name(source);
                let cell_value = |field: &FieldRef| {
                    if let FieldRef::Key(key) = field {
                        return record
                            .as_ref()
                            .and_then(|record| record.fields.iter().find(|field| field.key == *key))
                            .map(FilterValue::from_field)
                            .unwrap_or(FilterValue::Missing);
                    }
                    chips::row_values(
                        field,
                        record.as_ref(),
                        file.raw(local).unwrap_or_default(),
                        &source_name,
                    )
                    .into_iter()
                    .next()
                    .unwrap_or(FilterValue::Missing)
                };
                let cell_field = |field: FieldRef| {
                    let column = match &field {
                        FieldRef::Key(key) => Some(key.clone()),
                        FieldRef::Source => None,
                        field => record.as_ref().and_then(|record| {
                            let role = match field {
                                FieldRef::Time => Role::Time,
                                FieldRef::Level => Role::Level,
                                FieldRef::Message => Role::Message,
                                FieldRef::Logger => Role::Logger,
                                _ => unreachable!(),
                            };
                            record.role(role).map(|field| field.key.clone())
                        }),
                    };
                    (field, column)
                };
                let cells: Vec<Option<String>> =
                    if columns.is_empty() {
                        Vec::new()
                    } else {
                        columns
                            .iter()
                            .map(|key| {
                                record.as_ref().and_then(|record| {
                                    record.fields.iter().find(|field| &field.key == key).map(
                                        |field| field.value.chars().take(ROW_TEXT_LIMIT).collect(),
                                    )
                                })
                            })
                            .collect()
                    };
                let row = row_text_message(file, local, record.as_ref(), message_field.as_deref());
                let lines = row.lines;
                let selected = chosen.as_ref().is_some_and(|rows| rows.contains(&position));
                let marked = this.marks.contains(&(source, local));
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
                    .relative()
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
                    // A marked row (#1386): a dot in the gutter, inside the
                    // left padding so nothing else moves.
                    .when(marked, |row| {
                        row.child(
                            div()
                                .debug_selector(move || format!("log-mark-{index}"))
                                .absolute()
                                .left(px(3.))
                                .top(px((ROW_HEIGHT - 6.) / 2.))
                                .size(px(6.))
                                .rounded_full()
                                .bg(mark_color),
                        )
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.modifiers().shift {
                            this.extend_to(index, false, window, cx)
                        } else {
                            this.select(index, false, window, cx)
                        }
                    }))
                    // Several files: a colour stripe and the file's name (#1160).
                    .when(merged, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .w(px(3.))
                                .h(px(ROW_HEIGHT - 6.))
                                .rounded(px(1.5))
                                .bg(source_color(source, cx)),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .w(px(92.))
                            .font_family(mono.clone())
                            .text_size(px(12.))
                            .text_color(muted)
                            .child(this.filterable_cell(
                                div().child(row.time).into_any_element(),
                                format!("log-time-cell-{index}"),
                                cell_field(FieldRef::Time),
                                cell_value(&FieldRef::Time),
                                index,
                                cx,
                            )),
                    )
                    .when(merged, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .w(px(SOURCE_NAME_WIDTH))
                                .truncate()
                                .text_size(px(12.))
                                .text_color(muted)
                                .child(this.filterable_cell(
                                    div().child(this.source_name(source)).into_any_element(),
                                    format!("log-source-cell-{index}"),
                                    cell_field(FieldRef::Source),
                                    FilterValue::string(&this.source_name(source)),
                                    index,
                                    cx,
                                )),
                        )
                    })
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
                            .child(this.filterable_cell(
                                div().child(level.badge()).into_any_element(),
                                format!("log-level-cell-{index}"),
                                cell_field(FieldRef::Level),
                                cell_value(&FieldRef::Level),
                                index,
                                cx,
                            )),
                    )
                    // Field columns (#1167), before the message.
                    .children(
                        columns
                            .iter()
                            .zip(cells)
                            .enumerate()
                            .map(|(at, (key, value))| {
                                let value = value.unwrap_or_default();
                                let numeric = !value.is_empty() && value.parse::<f64>().is_ok();
                                div()
                                    .id(("log-cell", at))
                                    .flex_none()
                                    .w(px(widths[at]))
                                    .truncate()
                                    .text_size(px(12.))
                                    .when(numeric, |cell| {
                                        cell.text_align(TextAlign::Right).font_family(mono.clone())
                                    })
                                    .text_color(match column_tone(key, &value) {
                                        Some(Tone::Danger) => palette.danger,
                                        Some(Tone::Warning) => palette.warning,
                                        None => palette.text,
                                    })
                                    .child(this.filterable_cell(
                                        div().child(value).into_any_element(),
                                        format!("log-field-cell-{index}-{at}"),
                                        cell_field(FieldRef::Key(key.clone())),
                                        cell_value(&FieldRef::Key(key.clone())),
                                        index,
                                        cx,
                                    ))
                            }),
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
                            .child(
                                this.filterable_cell(
                                    div()
                                        .child(
                                            StyledText::new(row.text).with_highlights(highlights),
                                        )
                                        .into_any_element(),
                                    format!("log-message-cell-{index}"),
                                    cell_field(
                                        message_field.as_ref().map_or(FieldRef::Message, |key| {
                                            FieldRef::Key(key.clone())
                                        }),
                                    ),
                                    cell_value(
                                        &message_field.as_ref().map_or(FieldRef::Message, |key| {
                                            FieldRef::Key(key.clone())
                                        }),
                                    ),
                                    index,
                                    cx,
                                ),
                            ),
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
                    .into_any_element()
            })
            .collect()
    }

    /// The time of row `id`: the parsed timestamp that also orders a merged
    /// view, with the precision its time field writes (#1385). A record whose
    /// precision cannot be read is shown in whole seconds, never finer.
    fn stamp_of(&self, id: usize) -> Option<Stamp> {
        let (source, entry) = self.locate(id)?;
        let data = self.source(source)?;
        let nanos = data.index().get(entry)?.timestamp()?;
        let digits = data
            .record(entry)
            .and_then(|record| record.stamp())
            .filter(|stamp| stamp.nanos == nanos)
            .map_or(0, |stamp| stamp.digits);
        Some(Stamp::new(nanos, digits))
    }

    /// Elapsed time for the selection (#1385), with its tooltip: between the
    /// first and the last of the selected rows, signed by their order in the
    /// view, or from the one selected record to `now` (Unix nanoseconds; while
    /// following, every poll draws it again). A record without a timestamp says
    /// so instead of showing a number.
    pub fn elapsed_label(&self, now: i64) -> Option<(String, String)> {
        let cursor = self.selected?;
        let Some(anchor) = self.anchor.filter(|&anchor| anchor != cursor) else {
            return Some(match self.stamp_of(cursor) {
                Some(stamp) => (
                    elapsed::since(stamp, now),
                    "Time since this record, by this computer's clock".into(),
                ),
                None => ("No timestamp".into(), "This record has no timestamp".into()),
            });
        };
        let (anchor_row, cursor_row) = (self.row_of(anchor)?, self.row_of(cursor)?);
        let ((first, first_row), (last, last_row)) = if anchor_row <= cursor_row {
            ((anchor, anchor_row), (cursor, cursor_row))
        } else {
            ((cursor, cursor_row), (anchor, anchor_row))
        };
        let shown = |row: usize| grouped(row as u64 + 1);
        Some(match (self.stamp_of(first), self.stamp_of(last)) {
            (Some(from), Some(to)) => (
                format!("\u{394} {}", elapsed::between(from, to)),
                format!(
                    "From row {} to row {}, by their timestamps",
                    shown(first_row),
                    shown(last_row)
                ),
            ),
            (from, _) => (
                format!(
                    "Row {}: no timestamp",
                    shown(if from.is_none() { first_row } else { last_row })
                ),
                "Elapsed time needs a timestamp on both rows".into(),
            ),
        })
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let columns = self.columns(cx);
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let pane = v_flex().size_full().min_h_0();
        let located = self.selected.and_then(|id| {
            let (source, entry) = self.locate(id)?;
            Some((id, self.source(source)?, entry))
        });
        let Some((id, file, index)) = located else {
            return pane
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child(if self.total() > 0 {
                    "Select a record to see its fields"
                } else {
                    ""
                })
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
                        "Record {} of {} · {}{}",
                        grouped(self.row_of(id).unwrap_or(id) as u64 + 1),
                        grouped(self.visible_len() as u64),
                        match self.merge.as_ref().and(file.path().file_name()) {
                            Some(name) => format!("{} ", name.to_string_lossy()),
                            None => String::new(),
                        },
                        match lines {
                            _ if file.window().offset > 0 => format!(
                                "window line {} · {}byte {}",
                                grouped(entry.line()),
                                if file.window().decoded {
                                    "decoded "
                                } else {
                                    ""
                                },
                                grouped(file.window().absolute_offset(entry))
                            ),
                            1 => format!("line {}", grouped(entry.line())),
                            n => format!(
                                "lines {}–{}",
                                grouped(entry.line()),
                                grouped(entry.line() + n as u64 - 1)
                            ),
                        }
                    )),
            )
            .children(self.elapsed_label(unix_now()).map(|(label, tip)| {
                div()
                    .id("log-elapsed")
                    .debug_selector(|| "log-elapsed".into())
                    .flex_none()
                    .text_sm()
                    .text_color(muted)
                    .font_family(mono.clone())
                    .child(label)
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    })
            }))
            .children(
                file.record(index)
                    .as_ref()
                    .and_then(|record| self.request_id(record, cx))
                    .map(|id| {
                        let value = id.value.clone();
                        Button::new("log-show-request")
                            .debug_selector(|| "log-show-request".into())
                            .ghost()
                            .small()
                            .label("Follow this request")
                            .tooltip(format!("Every record with {} {}", id.key, id.value))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.follow_request(
                                    correlation::RequestId {
                                        key: id.key.clone(),
                                        value: value.clone(),
                                    },
                                    window,
                                    cx,
                                )
                            }))
                    }),
            )
            .child(
                reader_icon_button(
                    "log-copy-raw",
                    IconName::Copy,
                    reader_shortcuts::hint("Copy raw line", &CopyRawLine, cx),
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
        let fields_limited = record
            .as_ref()
            .is_some_and(|record| record.fields.len() > 256);
        let links = record
            .as_ref()
            .map(source_link::in_record)
            .unwrap_or_default();
        let body = body.children(record.as_ref().and_then(|record| {
            let key = self.message_field(cx)?;
            let field = record.fields.iter().find(|field| field.key == key)?;
            Some(
                v_flex()
                    .py_2()
                    .gap_2()
                    .child(div().text_color(muted).child(format!("Message · {key}")))
                    .child(
                        div()
                            .debug_selector(|| "log-full-message".into())
                            .whitespace_normal()
                            .child(field.value.clone()),
                    ),
            )
        }));
        let body = match record {
            Some(record) => body.children(
                detail_rows(&record, entry.timestamp())
                    .into_iter()
                    .zip(&record.fields)
                    .take(256)
                    .enumerate()
                    .map(|(position, ((key, value, kind), field))| {
                        let group = SharedString::from(format!("log-field-{position}"));
                        let term = Expr::Typed {
                            field: FieldRef::Key(field.key.clone()),
                            value: FilterValue::from_field(field),
                        };
                        let present = self.has_term(&term);
                        let field_links: Vec<SourceLocation> = links
                            .iter()
                            .filter(|found| found.field == position && value == field.value)
                            .map(|found| found.location.clone())
                            .collect();
                        let (field_key, field_value) = (field.key.clone(), field.value.clone());
                        let value = bounded_detail(&value);
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
                                    .child(self.linked(
                                        ("log-field-value", position),
                                        value,
                                        field_links,
                                        cx,
                                    )),
                            )
                            // A fixed slot, shown on hover, so rows never shift.
                            .child(
                                h_flex()
                                    .flex_none()
                                    .opacity(if present || columns.contains(&field.key) {
                                        1.
                                    } else {
                                        0.
                                    })
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
                                                    FilterValue::Value {
                                                        kind,
                                                        text: field_value.clone(),
                                                    },
                                                    window,
                                                    cx,
                                                )
                                            })),
                                    )
                                    .child({
                                        use gpui_component::menu::DropdownMenu as _;
                                        let owner = cx.entity().downgrade();
                                        let key = field.key.clone();
                                        let value = FilterValue::from_field(field);
                                        let request = correlation::ids_with_field(
                                            &record,
                                            self.field_choices(cx).correlation.as_deref(),
                                        )
                                        .into_iter()
                                        .find(|id| id.key == key);
                                        Button::new(("log-detail-field-actions", position))
                                            .debug_selector(move || {
                                                format!("log-detail-field-actions-{position}")
                                            })
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::Ellipsis)
                                            .tooltip("Field actions")
                                            .dropdown_menu(move |menu, _, _| {
                                                Self::field_actions(
                                                    menu,
                                                    owner.clone(),
                                                    key.clone(),
                                                    value.clone(),
                                                    request.clone(),
                                                )
                                            })
                                    })
                                    .child({
                                        let key = field.key.clone();
                                        let shown = columns.contains(&field.key);
                                        Button::new(("log-toggle-column", position))
                                            .debug_selector(move || {
                                                format!("log-toggle-column-{position}")
                                            })
                                            .ghost()
                                            .xsmall()
                                            .icon(
                                                Icon::new(IconName::LayoutDashboard)
                                                    .text_color(faint),
                                            )
                                            .tooltip(if shown {
                                                "Hide this column"
                                            } else {
                                                "Show as a column"
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.toggle_column(&key, cx)
                                            }))
                                    }),
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
                .child({
                    let text = bounded_detail(&String::from_utf8_lossy(&ansi::strip(raw)));
                    let found = source_link::find(&text);
                    div()
                        .debug_selector(|| "log-detail-raw".into())
                        .font_family(mono.clone())
                        .text_size(px(12.))
                        .child(self.linked("log-detail-raw", text, found, cx))
                }),
        };
        let body =
            body.when(fields_limited, |body| {
                body.child(div().text_sm().text_color(muted).child(
                    "Showing the first 256 fields; copy or export the record for all fields.",
                ))
            });
        // A record's continuation lines, frame by frame as written; an entry
        // that is no record shows all its lines above.
        let body = body.when_some(
            block::continuation(raw).filter(|_| is_record),
            |body, rest| {
                let text = bounded_detail(&String::from_utf8_lossy(&ansi::strip(rest)));
                let found = source_link::find(&text);
                body.child(
                    div()
                        .id("log-detail-continuation")
                        .debug_selector(|| "log-detail-frames".into())
                        .pt_2()
                        .font_family(mono)
                        .text_size(px(12.))
                        .child(self.linked("log-detail-frames", text, found, cx)),
                )
            },
        );
        pane.child(header).child(body).into_any_element()
    }

    pub fn begin_regex(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Regex name"));
        let pattern = cx.new(|cx| {
            InputState::new(window, cx).placeholder("(?P<ts>...) (?P<level>...) (?P<msg>...)")
        });
        let events = cx.subscribe_in(&pattern, window, |this, input, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                let definition = okilum_core::log::regex_text::Definition {
                    name: String::new(),
                    pattern: input.read(cx).value().to_string(),
                };
                if let Some(editor) = this.regex_editor.as_mut() {
                    editor.error = definition.compile().err().map(|error| error.to_string());
                }
                cx.notify();
            }
        });
        name.update(cx, |input, cx| input.focus(window, cx));
        self.regex_editor = Some(RegexEditor {
            name,
            pattern,
            error: None,
            _events: events,
        });
        cx.notify();
    }

    pub fn save_regex(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.regex_editor else {
            return;
        };
        let definition = okilum_core::log::regex_text::Definition {
            name: editor.name.read(cx).value().trim().to_owned(),
            pattern: editor.pattern.read(cx).value().to_string(),
        };
        let result = definition.compile();
        let error = if definition.name.is_empty() {
            Some("Enter a name".to_owned())
        } else {
            result.as_ref().err().map(ToString::to_string)
        };
        if let Some(error) = error {
            self.regex_editor.as_mut().unwrap().error = Some(error);
        } else if !reader_ui_state::save_log_regex(definition, cx) {
            self.regex_editor.as_mut().unwrap().error =
                Some("Cannot save regex in the app's settings file".into());
        } else {
            self.regex_editor = None;
            self.focus.focus(window, cx);
            self.apply_regex(result.ok(), cx);
        }
        cx.notify();
    }

    /// Reopens the primary file off the UI thread; a newer choice cancels the job.
    pub fn apply_regex(
        &mut self,
        regex: Option<Arc<okilum_core::log::regex_text::RegexParser>>,
        cx: &mut Context<Self>,
    ) {
        if self.source_locked {
            return;
        }
        let Some(data) = self.data() else {
            return;
        };
        let path = data.path().to_owned();
        let rel = match &self.state {
            State::Ready(document) => document.rel.clone(),
            State::Held(source) => source.path.to_string_lossy().into_owned(),
            State::Following { rel } => rel.clone(),
            _ => return,
        };
        self.regex_error = None;
        self._indexing = Some(cx.spawn(async move |this, cx| {
            let started = std::time::Instant::now();
            let result = cx
                .background_executor()
                .spawn(async move { open_with_regex(&path, regex, false) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(file) => {
                        this._follow_open = None;
                        this.follow = None;
                        this.generations.next();
                        this._filtering = None;
                        this.filter_running = None;
                        this.filter_again = false;
                        this._histogram = None;
                        this.histogram_running = None;
                        this.state = State::Ready(LogDocument {
                            rel,
                            file: Arc::new(file),
                            elapsed: started.elapsed(),
                        });
                        this.selected = None;
                        this.anchor = None;
                        this.marks.retain(|(source, _)| *source != 0);
                        this.rows = None;
                        this.filtering = false;
                        this.rebuild_merge();
                        this.after_rows_changed();
                        this.refilter(cx);
                        this.reveal_tail();
                    }
                    Err(error) => this.regex_error = Some(format!("Cannot apply regex: {error:#}")),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn regex_button(&self, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let definitions = reader_ui_state::log_regex_definitions(cx);
        let view = cx.entity().downgrade();
        bar_button(
            "log-regex",
            self.data()
                .is_some_and(|data| data.index().format() == Format::Regex),
        )
        .secondary()
        .outline()
        .label("Regex")
        .dropdown_caret(true)
        .debug_selector(|| "log-regex".into())
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
            let v = view.clone();
            let mut menu = menu.item(PopupMenuItem::new("New regex…").on_click(
                move |_, window, cx| {
                    let _ = v.update(cx, |this, cx| this.begin_regex(window, cx));
                },
            ));
            let v = view.clone();
            menu = menu.item(PopupMenuItem::new("Use detected format").on_click(
                move |_, _, cx| {
                    let _ = v.update(cx, |this, cx| this.apply_regex(None, cx));
                },
            ));
            for definition in &definitions {
                let v = view.clone();
                let definition = definition.clone();
                menu = menu.item(
                    PopupMenuItem::new(format!("Apply “{}” to this file", definition.name))
                        .on_click(move |_, _, cx| {
                            let _ = v.update(cx, |this, cx| match definition.compile() {
                                Ok(regex) => this.apply_regex(Some(regex), cx),
                                Err(error) => {
                                    this.regex_error = Some(error.to_string());
                                    cx.notify();
                                }
                            });
                        }),
                );
            }
            menu
        })
        .into_any_element()
    }

    fn render_regex_editor(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let editor = self.regex_editor.as_ref()?;
        let pattern =
            Input::new(&editor.pattern)
                .small()
                .when_some(editor.error.clone(), |input, error| {
                    input.suffix(
                        div()
                            .id("log-regex-pattern-error")
                            .debug_selector(|| "log-regex-error".into())
                            .max_w(px(200.))
                            .truncate()
                            .text_size(px(12.))
                            .line_height(px(16.))
                            .text_color(brand::palette(cx).danger)
                            .child(regex_error_summary(&error))
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(error.clone())
                                    .build(window, cx)
                            }),
                    )
                });
        Some(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .w(px(120.))
                        .debug_selector(|| "log-regex-name".into())
                        .child(Input::new(&editor.name).small()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(80.))
                        .debug_selector(|| "log-regex-pattern".into())
                        .child(pattern),
                )
                .child(self.regex_help())
                .child(
                    Button::new("log-regex-save")
                        .small()
                        .label("Save and apply")
                        .on_click(cx.listener(|this, _, window, cx| this.save_regex(window, cx))),
                )
                .child(
                    Button::new("log-regex-cancel")
                        .small()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.regex_editor = None;
                            this.focus.focus(window, cx);
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// Starts naming the shown filter to save it (#1169).
    pub fn begin_saving(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Name this view"));
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.commit_saving(window, cx),
                InputEvent::Change => {
                    if let Some(saving) = this.saving.as_mut() {
                        saving.error = None;
                        cx.notify();
                    }
                }
                _ => {}
            });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.saving = Some(SavingFilter {
            input,
            error: None,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Escape closes the name row; `false` when it was not open.
    pub fn dismiss_saving(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let open = self.saving.is_some();
        self.cancel_saving(window, cx);
        open
    }

    fn cancel_saving(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving.take().is_some() {
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Saves the shown filter under the typed name, for every log. A time
    /// range from the histogram is not saved, and the toast says so.
    pub fn commit_saving(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(saving) = self.saving.as_ref() else {
            return;
        };
        let name = saving.input.read(cx).value().to_string();
        let fail = |this: &mut Self, message: String, cx: &mut Context<Self>| {
            if let Some(saving) = this.saving.as_mut() {
                saving.error = Some(message);
            }
            cx.notify();
        };
        let mut query = match SavedQuery::capture(&name, &self.filter) {
            Ok(query) => query,
            Err(error) => return fail(self, error.to_string(), cx),
        };
        query.filter_chips = self.filter_ui.chips.clone();
        query.columns = Some(self.columns(cx));
        self.capture_investigation(&mut query, cx);
        if !reader_ui_state::installed(cx) {
            return fail(
                self,
                "Saved filters need the app's settings file".into(),
                cx,
            );
        }
        let replaced = reader_ui_state::saved_log_filters(cx)
            .get(&query.name)
            .is_some();
        let name = query.name.clone();
        if !reader_ui_state::save_log_filter(query, cx) {
            return fail(
                self,
                format!(
                    "At most {} filters can be saved; delete one first",
                    okilum_core::log::saved_query::MAX_QUERIES
                ),
                cx,
            );
        }
        let mut note = format!("Saved “{name}”");
        if replaced {
            note.push_str(", replacing the one of that name");
        }
        if matches!(self.filter.time, TimeWindow::Range { .. }) {
            note.push_str("; the selected time range is not saved");
        }
        if self.query_error.is_some() {
            note.push_str("; the query field has an error, the last valid query was saved");
        }
        self.saving = None;
        self.focus.focus(window, cx);
        reader_toast::transient(note, window, cx);
        cx.notify();
    }

    /// Makes a saved filter the shown one: its text, query, levels, time and
    /// chips replace the current ones. A query this version cannot read is
    /// reported, not guessed at.
    pub fn apply_saved(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(saved) = reader_ui_state::saved_log_filters(cx).get(name).cloned() else {
            return;
        };
        let mut filter = match saved.to_filter() {
            Ok(filter) => filter,
            Err(error) => {
                reader_toast::error(
                    format!(
                        "“{}” cannot be applied: its query does not parse ({})",
                        saved.name, error.message
                    ),
                    window,
                    cx,
                );
                return;
            }
        };
        if !self.restore_saved_source(&saved, window, cx) {
            return;
        }
        self.filter_ui.navigating = saved.remote_source.is_some();
        self.restore_investigation(&saved, cx);
        let (parsed_chips, advanced) = filter
            .query
            .as_ref()
            .map(chips::split_query)
            .unwrap_or_default();
        self.filter_ui.chips = if saved.filter_chips.is_empty() {
            parsed_chips
        } else {
            saved.filter_chips.clone()
        };
        self.filter_ui.advanced = advanced;
        if let (Some(key), Some(columns)) = (self.history_key(), saved.columns.clone()) {
            reader_ui_state::set_log_columns(&key, columns, cx);
        }
        let query_text = filter
            .query
            .as_ref()
            .map(|query| query.to_string())
            .unwrap_or_default();
        self.text_input.update(cx, |input, cx| {
            input.set_value(filter.text.clone(), window, cx)
        });
        self.query_input.update(cx, |input, cx| {
            input.set_value(query_text.clone(), window, cx)
        });
        self.query_text = query_text;
        self.query_error = None;
        if self.filter_ui.server {
            let local = self.local_chip_query();
            filter.query = (!local.is_empty()).then_some(local);
            self.emit_chip_change(cx);
        }
        self.update_filter(|current| *current = filter, cx);
        self.filter_ui.navigating = false;
        self.remember_query(cx);
        cx.notify();
    }

    fn render_saving(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let saving = self.saving.as_ref()?;
        let palette = brand::palette(cx);
        let muted = cx.theme().muted_foreground;
        Some(
            h_flex()
                .id("log-saving")
                .debug_selector(|| "log-saving".into())
                .flex_none()
                .h(px(28.))
                .px_3()
                .gap_2()
                .border_t_1()
                .border_color(palette.border_subtle)
                .key_context("LogSaving")
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.))
                        .text_color(muted)
                        .child("Save view"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(80.))
                        .max_w(px(260.))
                        .h(px(BAR_CONTROL_HEIGHT))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .bg(palette.surface_raised)
                        .debug_selector(|| "log-saving-name".into())
                        .child(Input::new(&saving.input).appearance(false).small()),
                )
                .child(
                    Button::new("log-saving-save")
                        .primary()
                        .small()
                        .label("Save")
                        .debug_selector(|| "log-saving-save".into())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.commit_saving(window, cx)),
                        ),
                )
                .child(
                    Button::new("log-saving-cancel")
                        .ghost()
                        .small()
                        .label("Cancel")
                        .debug_selector(|| "log-saving-cancel".into())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.cancel_saving(window, cx)),
                        ),
                )
                .children(saving.error.clone().map(|message| {
                    div()
                        .id("log-saving-error")
                        .debug_selector(|| "log-saving-error".into())
                        .truncate()
                        .text_size(px(12.))
                        .text_color(palette.danger)
                        .child(message)
                }))
                .into_any_element(),
        )
    }

    /// The files and rows an export would write now, or `None` when a file
    /// of the view cannot be read.
    fn export_job(&self, scope: ExportScope) -> Option<ExportJob> {
        let sources: Vec<usize> = match &self.merge {
            Some(merge) => merge.sources.clone(),
            None => self.enabled_sources().into_iter().take(1).collect(),
        };
        let files: Vec<(Snapshot, String)> = sources
            .iter()
            .map(|&source| Some((self.snapshot_of(source)?, self.source_name(source))))
            .collect::<Option<_>>()?;
        let ids = match scope {
            ExportScope::Shown => match &self.rows {
                Some(rows) => ExportIds::List(rows.clone()),
                None => ExportIds::All(self.total()),
            },
            ExportScope::Selected => ExportIds::Picked(
                self.selection()?
                    .filter_map(|row| self.entry_at(row).map(|id| id as u32))
                    .collect(),
            ),
            ExportScope::Marked => ExportIds::Picked(
                self.marked_rows()
                    .into_iter()
                    .filter_map(|row| self.entry_at(row).map(|id| id as u32))
                    .collect(),
            ),
        };
        Some(ExportJob {
            files,
            merged: self.merge.as_ref().map(|merge| merge.log.clone()),
            ids,
        })
    }

    /// Asks where to save, then writes the rows as they were when this was
    /// asked: raw lines exactly as in the file, or one JSON object per row.
    /// The file appears whole or not at all, and never replaces a log being
    /// read.
    pub fn export_rows(
        &mut self,
        scope: ExportScope,
        format: ExportFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.exporting {
            reader_toast::error("An export is still running", window, cx);
            return;
        }
        let Some(job) = self.export_job(scope) else {
            reader_toast::error(
                "Cannot export: a file of this view cannot be read",
                window,
                cx,
            );
            return;
        };
        if job.count() == 0 {
            reader_toast::error("Nothing to export: no row is shown", window, cx);
            return;
        }
        let logs: Vec<PathBuf> = self
            .enabled_sources()
            .into_iter()
            .filter_map(|source| Some(self.source(source)?.path().to_path_buf()))
            .collect();
        let first = logs.first().cloned().unwrap_or_default();
        let directory = first.parent().map(Path::to_path_buf).unwrap_or_default();
        let stem = first
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "log".into());
        let what = match scope {
            ExportScope::Shown => "filtered",
            ExportScope::Selected => "selected",
            ExportScope::Marked => "marked",
        };
        let suggested = match &format {
            ExportFormat::Raw => format!(
                "{stem}-{what}.{}",
                first
                    .extension()
                    .map_or("log".into(), |ext| ext.to_string_lossy())
            ),
            ExportFormat::JsonLines => format!("{stem}-{what}.jsonl"),
            ExportFormat::Csv(_) => format!("{stem}-{what}.csv"),
        };
        let picker = cx.prompt_for_new_path(&directory, Some(&suggested));
        self.exporting = true;
        cx.spawn_in(window, async move |this, cx| {
            let chosen = picker.await;
            let outcome = match chosen {
                Ok(Ok(Some(path))) => {
                    let counted = job.count();
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            write_export(&job, &logs, &path, &format).map(|n| (n, path))
                        })
                        .await;
                    Some((counted, result))
                }
                _ => None,
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.exporting = false;
                match outcome {
                    Some((_, Ok((written, path)))) => {
                        let name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        reader_toast::transient(
                            format!("Exported {} rows to {name}", grouped(written as u64)),
                            window,
                            cx,
                        );
                    }
                    Some((_, Err(error))) => {
                        reader_toast::error(format!("Cannot export: {error:#}"), window, cx)
                    }
                    None => {}
                }
            });
        })
        .detach();
    }

    /// The truncation or rotation follow mode saw last, above the rows.
    fn reset_notice(&self) -> Option<String> {
        let (reason, earlier) = self.follow.as_ref()?.reset?;
        let what = match reason {
            ResetReason::Truncated => "The file was truncated",
            ResetReason::Rotated => "The file was rotated",
        };
        // The earlier records stay listed as their own file (#1160).
        let name = self.source_name(0);
        let label = match reason {
            ResetReason::Truncated => "before truncation",
            ResetReason::Rotated => "before rotation",
        };
        let earlier = match earlier {
            0 => String::new(),
            1 => format!(" · 1 earlier record is listed as “{name} ({label})”"),
            n => format!(
                " · {} earlier records are listed as “{name} ({label})”",
                grouped(n as u64)
            ),
        };
        Some(format!("{what}{earlier}; following the new content"))
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

    pub fn status(&self) -> String {
        let mut text = match (&self.state, &self.follow) {
            (State::Stream(data), _) => format!(
                "{} loaded stream records · Local filters affect loaded rows only",
                grouped(data.index().len() as u64)
            ),
            (State::Remote(data), _) => format!(
                "{} loaded remote events · Local filters affect loaded rows only",
                grouped(data.events.len() as u64)
            ),
            (State::Held(source), _) => index_status(source.index()),
            (State::Indexing, _) => "Indexing…".to_owned(),
            (State::Failed(error), _) => format!("Cannot show this log: {error}"),
            (State::Ready(_) | State::Following { .. }, Some(follow)) => {
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
            (State::Ready(_), None) if !self.extra.is_empty() => {
                let records: usize = (0..self.source_count())
                    .filter_map(|source| self.source(source))
                    .map(|data| data.index().len())
                    .sum();
                let shown = self.enabled_sources().len();
                let mut text = format!(
                    "{} files, {} shown · {} records",
                    self.source_count(),
                    shown,
                    grouped(records as u64)
                );
                if let Some(error) = &self.follow_failed {
                    text.push_str(&format!(" · Cannot open {error}"));
                }
                text
            }
            (State::Ready(document), None) => match &self.follow_failed {
                Some(error) => format!("{} · Cannot follow: {error}", status_text(document)),
                None => status_text(document),
            },
            (State::Following { .. }, None) => "Preparing Follow…".to_owned(),
        };
        if !self.extra.is_empty() {
            // Follow already reports the primary file above. A merged view
            // must also keep each regex's counts, including a zero match.
            let first = usize::from(self.follow.is_some());
            for source in first..self.source_count() {
                if let Some(data) = self.source(source) {
                    if data.index().regex().is_some() {
                        text.push_str(&format!(
                            " · {}: {}",
                            self.source_name(source),
                            index_status(data.index())
                        ));
                    }
                }
            }
        }
        text
    }

    fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(saving) = self.render_saving(cx) {
            return saving;
        }
        let mut text = self.status();
        if self.following() || self.investigation.live {
            if let Some(last) = &self.investigation.last_update {
                text.push_str(&format!(" · Live · last update {last}"));
            }
        }
        let counter = match_counter(
            self.rows.as_ref().map(|rows| rows.len()),
            self.total(),
            self.filtering,
        );
        let source = self
            .history_key()
            .filter(|_| !matches!(self.state, State::Remote(_) | State::Stream(_)))
            .map(|path| format!("Log source: {path}"));
        let notice = self.window_notice();
        let reset = self.reset_notice();
        let mut full = text.clone();
        for part in [
            counter.as_ref(),
            source.as_ref(),
            notice.as_ref(),
            reset.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            full.push_str(" · ");
            full.push_str(part);
        }
        let (leading, remainder, count_label) =
            status_parts(&text, counter, self.total(), self.filtering);
        let trailing = h_flex()
            .id("log-status-content")
            .debug_selector(|| "log-status".into())
            .flex_1()
            .min_w_0()
            .gap_2()
            .overflow_hidden()
            .child(div().truncate().child(leading))
            .children(count_label.map(|count| {
                div()
                    .flex_none()
                    .debug_selector(|| "log-match-counter".into())
                    .child(count)
            }))
            .when(!remainder.is_empty(), |row| {
                row.child(div().truncate().child(remainder))
            })
            .children(source.map(|source| {
                div()
                    .min_w_0()
                    .truncate()
                    .debug_selector(|| "log-view-source".into())
                    .child(source)
            }))
            .children(notice.map(|notice| {
                div()
                    .min_w_0()
                    .truncate()
                    .debug_selector(|| "log-window-scope".into())
                    .child(notice)
            }))
            .children(reset.map(|reset| {
                div()
                    .min_w_0()
                    .truncate()
                    .debug_selector(|| "log-follow-reset".into())
                    .child(reset)
            }))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(full.clone()).build(window, cx)
            })
            .into_any_element();
        reader_status_line::render_with_content(
            &reader_status_line::StatusLine::default(),
            "log-status",
            Some(trailing),
            None,
            cx,
        )
    }
}

impl Render for LogView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_pin();
        // A request to scroll to the newest row is placed with the row height
        // the list knew at that moment, which is not yet measured when the
        // first rows appear, and the view stayed a few rows short of the end
        // until the next poll (#1342). Asking again while pinned lets the next
        // layout, which has measured the rows, place it.
        self.keep_pinned();
        let count = self.visible_len();
        let has_entries = (0..self.source_count()).any(|source| {
            self.source(source)
                .is_some_and(|data| !data.index().is_empty())
        });
        let list = div().relative().flex_1().min_h_0().w_full();
        let columns = self.columns(cx);
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
                if this.follow.is_some() || this.stream_bottom.is_some() {
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
                State::Ready(_) | State::Held(_) | State::Following { .. } => "This log is empty",
                State::Remote(_) => "No remote events loaded",
                State::Stream(_) => "No complete stream lines loaded",
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
        };
        // Column titles above the rows, on the rows' grid (#1167).
        let list = if !has_entries {
            list.into_any_element()
        } else {
            let merged = self.merge.is_some();
            let muted = cx.theme().muted_foreground;
            let faint = brand::reader_palette(cx).text_faint;
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    h_flex()
                        .debug_selector(|| "log-columns".into())
                        .flex_none()
                        .h(px(ROW_HEIGHT))
                        .w_full()
                        .pl_3()
                        .pr_4()
                        .gap_3()
                        .border_l_2()
                        .border_color(transparent_black())
                        .border_b_1()
                        .text_size(px(12.))
                        .text_color(muted)
                        .when(merged, |row| row.child(div().flex_none().w(px(3.))))
                        .child(div().flex_none().w(px(92.)).child(self.filter_popover(
                            "log-time-filter".into(),
                            "Time".into(),
                            FieldRef::Time,
                            None,
                            cx,
                        )))
                        .when(merged, |row| {
                            row.child(div().flex_none().w(px(SOURCE_NAME_WIDTH)).child(
                                self.filter_popover(
                                    "log-source-filter".into(),
                                    "File".into(),
                                    FieldRef::Source,
                                    None,
                                    cx,
                                ),
                            ))
                        })
                        .child(div().flex_none().w(px(28.)).child(self.filter_popover(
                            "log-level-filter".into(),
                            "Lv".into(),
                            FieldRef::Level,
                            None,
                            cx,
                        )))
                        .children(columns.iter().enumerate().map(|(at, key)| {
                            let column = key.clone();
                            h_flex()
                                .debug_selector(move || format!("log-column-{at}"))
                                .relative()
                                .flex_none()
                                .w(px(self.width_of(key, cx)))
                                .gap_1()
                                .child(div().min_w_0().overflow_hidden().child(
                                    self.filter_popover(
                                        format!("log-header-filter-{at}").into(),
                                        key.clone(),
                                        FieldRef::Key(key.clone()),
                                        None,
                                        cx,
                                    ),
                                ))
                                .child(
                                    Button::new(("log-remove-column", at))
                                        .ghost()
                                        .xsmall()
                                        .icon(Icon::new(IconName::Close).text_color(faint))
                                        .tooltip("Hide this column")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.toggle_column(&column, cx)
                                        })),
                                )
                                // Drag the right edge to resize (#1167).
                                .child(
                                    div()
                                        .id(("log-column-resize", at))
                                        .debug_selector(move || format!("log-column-resize-{at}"))
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        // Centred on the column edge, so the edge follows the pointer.
                                        .right(px(-3.))
                                        .w(px(6.))
                                        .cursor_col_resize()
                                        .on_drag(ColumnDrag(at), |drag, _, _, cx| {
                                            cx.new(|_| drag.clone())
                                        }),
                                )
                        }))
                        .child(
                            div().flex_1().child(
                                self.filter_popover(
                                    "log-message-filter".into(),
                                    "Message".into(),
                                    self.message_field(cx)
                                        .map_or(FieldRef::Message, FieldRef::Key),
                                    None,
                                    cx,
                                ),
                            ),
                        )
                        .on_drag_move(cx.listener(
                            |this, event: &DragMoveEvent<ColumnDrag>, _, cx| {
                                let at = event.drag(cx).0;
                                let start = event.bounds.left() + px(this.column_start(at, cx));
                                let width = f32::from(event.event.position.x - start);
                                this.resize_column(at, width, cx);
                            },
                        )),
                )
                .child(list)
                .into_any_element()
        };
        let bounds = self.pane_bounds.clone();
        let owner = cx.entity().downgrade();
        v_flex()
            .id("reader-log")
            .debug_selector(|| "reader-log".into())
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .min_h_0()
            .on_action(
                cx.listener(|this, _: &SelectNextRecord, window, cx| this.step(1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectPreviousRecord, window, cx| this.step(-1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ExtendNextRecord, window, cx| {
                this.step_by(1, true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ExtendPreviousRecord, window, cx| {
                this.step_by(-1, true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyRawLine, window, cx| this.copy_raw(window, cx)))
            .on_action(
                cx.listener(|this, _: &FocusLogFilter, window, cx| this.focus_filter(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &InvestigationBack, window, cx| {
                    this.request_back(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &FindInNote, window, cx| this.focus_filter(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleLogQuery, window, cx| this.toggle_query(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleLogFollow, _, cx| this.toggle_follow(cx)))
            .on_action(
                cx.listener(|this, _: &ClearLogFilters, window, cx| this.clear_filters(window, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenLogLevels, window, cx| {
                this.show_menu(chrome::MenuKind::Level, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenLogTimeRange, window, cx| {
                this.show_menu(chrome::MenuKind::Time, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenLogSavedViews, window, cx| {
                this.show_menu(chrome::MenuKind::Saved, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenLogColumns, window, cx| {
                this.show_menu(chrome::MenuKind::Columns, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenLogExport, window, cx| {
                this.show_menu(chrome::MenuKind::Export, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenLogFiles, window, cx| {
                this.show_menu(chrome::MenuKind::Files, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleMark, _, cx| this.toggle_mark(cx)))
            .on_action(
                cx.listener(|this, _: &NextMark, window, cx| this.step_mark(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PreviousMark, window, cx| this.step_mark(false, window, cx)),
            )
            // Log files dropped from the system join the view (#1160).
            .drag_over::<ExternalPaths>(|style, _, _, cx| {
                style.bg(brand::palette(cx).selected.opacity(0.35))
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_files(paths, window, cx)
            }))
            .when(!self.hosted_header, |view| {
                view.child(self.render_own_header(window, cx))
            })
            .children(self.render_secondary(window, cx))
            .children(self.render_histogram(cx))
            .child(reader_split::vertical(
                &self.detail_split,
                |view: &mut Self| &mut view.detail_split,
                list,
                self.render_detail(cx),
                cx,
            ))
            .child(self.render_status(cx))
            .children(self.render_cell_menu())
            .children(self.render_command_menu())
            .child(
                canvas(
                    move |area, _, cx| {
                        let previous = bounds.replace(area);
                        if previous.size.width != area.size.width {
                            // Geometry changes once after layout, never per input or poll.
                            cx.defer(move |cx| {
                                let _ = owner.update(cx, |_, cx| cx.notify());
                            });
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
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

/// Regex diagnostics include a multi-line source excerpt. Keep the cause on
/// one line in the 40 px row; the tooltip retains the complete diagnostic.
pub fn regex_error_summary(error: &str) -> String {
    error
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(error)
        .trim()
        .trim_start_matches("error: ")
        .to_owned()
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

/// Writes `job` to `path` through a temporary file next to it, so the file
/// appears whole or not at all; returns how many rows. A log being read is
/// never the destination.
fn write_export(
    job: &ExportJob,
    logs: &[PathBuf],
    path: &Path,
    format: &ExportFormat,
) -> anyhow::Result<usize> {
    use anyhow::Context as _;
    use std::io::Write as _;
    let target = path.canonicalize().ok();
    anyhow::ensure!(
        !logs
            .iter()
            .any(|log| log == path || (target.is_some() && log.canonicalize().ok() == target)),
        "choose a file other than the log itself"
    );
    let directory = path
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .context("the destination has no folder")?;
    let _admission = okilum_write_authority::operation_for_path(path)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    let written = {
        let mut out = std::io::BufWriter::new(file.as_file_mut());
        let written = job.write(&mut out, format)?;
        out.flush()?;
        written
    };
    file.persist(path)?;
    Ok(written)
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
/// Column width by key (#1167): paths, URLs and messages get room.
fn column_width(key: &str) -> f32 {
    let key = key.to_ascii_lowercase();
    if [
        "path",
        "url",
        "uri",
        "msg",
        "message",
        "ua",
        "user_agent",
        "referer",
    ]
    .iter()
    .any(|wide| {
        key == *wide || key.ends_with(&format!(".{wide}")) || key.ends_with(&format!("_{wide}"))
    }) {
        WIDE_COLUMN_WIDTH
    } else {
        COLUMN_WIDTH
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Warning,
    Danger,
}

/// A column value that wants attention (#1167): an HTTP status of 4xx or
/// 5xx, or a duration over a second (the unit from the key: `_ns`, `_us`,
/// `_ms`, seconds otherwise).
fn column_tone(key: &str, value: &str) -> Option<Tone> {
    let key = key.to_ascii_lowercase();
    let number: f64 = value.trim().parse().ok()?;
    let last = key.rsplit(['.', '_']).next().unwrap_or(&key);
    if key.ends_with("status")
        || key.ends_with("status_code")
        || last == "code" && key.contains("status")
    {
        return match number as i64 {
            500..=599 => Some(Tone::Danger),
            400..=499 => Some(Tone::Warning),
            _ => None,
        };
    }
    if [
        "duration",
        "latency",
        "elapsed",
        "took",
        "time_taken",
        "response_time",
    ]
    .iter()
    .any(|word| key.contains(word))
    {
        let second = match last {
            "ns" => 1e9,
            "us" | "micros" => 1e6,
            "ms" | "millis" => 1e3,
            _ => 1.,
        };
        return (number > second).then_some(Tone::Warning);
    }
    None
}

/// "N of M" once a filter has produced rows. Nothing while no filter runs,
/// and nothing for an empty result: the list says that itself.
pub fn match_counter(matched: Option<usize>, total: usize, filtering: bool) -> Option<String> {
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

/// Keep a progress label distinct from a completed "N of M records" count.
pub fn status_parts(
    text: &str,
    counter: Option<String>,
    total: usize,
    filtering: bool,
) -> (String, String, Option<String>) {
    let total_label = format!(
        "{} {}",
        grouped(total as u64),
        if total == 1 { "record" } else { "records" }
    );
    match (&counter, text.split_once(" · ")) {
        (Some(counter), Some((format, rest))) if !filtering && rest.starts_with(&total_label) => (
            format.to_owned(),
            rest[total_label.len()..]
                .trim_start_matches(" · ")
                .to_owned(),
            Some(format!("{counter} records")),
        ),
        _ => (text.to_owned(), String::new(), counter),
    }
}

/// One row's text and the byte ranges drawn muted (logger, other fields).
#[derive(Debug, PartialEq, Eq)]
pub struct RowText {
    pub time: String,
    pub text: String,
    pub muted: Vec<Range<usize>>,
    /// Lines the entry spans; more than one is shown folded.
    pub lines: usize,
}

#[cfg(any(test, feature = "test-support"))]
pub fn row_text(file: &dyn LogData, index: usize) -> RowText {
    row_text_record(file, index, file.record(index).as_ref())
}

fn row_text_record(file: &dyn LogData, index: usize, record: Option<&Record>) -> RowText {
    let raw = file.raw(index).unwrap_or_default();
    let time = file
        .index()
        .get(index)
        .and_then(|entry| entry.timestamp())
        .map(timestamp::clock_utc)
        .unwrap_or_default();
    let mut row = RowBuilder::default();
    match record {
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
            &String::from_utf8_lossy(&ansi::strip(block::head(raw))),
            false,
        ),
    }
    RowText {
        time,
        text: row.text,
        muted: row.muted,
        lines: block::line_count(raw),
    }
}

pub fn row_text_message(
    file: &dyn LogData,
    index: usize,
    record: Option<&Record>,
    message: Option<&str>,
) -> RowText {
    let mut row = row_text_record(file, index, record);
    if let Some(key) = message {
        let mut text = RowBuilder::default();
        if let Some(field) =
            record.and_then(|record| record.fields.iter().find(|field| field.key == key))
        {
            text.push(&field.value, false);
        }
        row.text = text.text;
        row.muted.clear();
    }
    row
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
fn bounded_detail(text: &str) -> String {
    const MAX_CHARS: usize = 8192;
    let mut chars = text.chars();
    let mut shown: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        shown.push_str("… (preview limited; copy or export for full text)");
    }
    shown
}

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
    if let (Some(regex), Some((matched, unmatched))) = (index.regex(), index.regex_counts()) {
        let mut text = format!(
            "Regex “{}” matched {} of {} {}",
            regex.definition().name,
            grouped(matched),
            grouped(matched + unmatched),
            if matched + unmatched == 1 {
                "line"
            } else {
                "lines"
            }
        );
        if unmatched > 0 {
            text.push_str(&format!(
                "; {} unmatched {} shown as text",
                grouped(unmatched),
                if unmatched == 1 { "line" } else { "lines" }
            ));
        }
        return text;
    }
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

/// This computer's clock as Unix nanoseconds, for "time since" (#1385).
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        })
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
        for (key, value) in [
            ("sample.path", "synthetic-85b4"),
            ("sample", "500"),
            ("level", "NOTICE"),
            ("msg", "a \"b\"\nc"),
            ("@time", "space : \\ λ"),
        ] {
            let term = FilterChip::new(
                FieldRef::Key(key.into()),
                vec![FilterValue::string(value)],
                FilterMode::Include,
            )
            .expr();
            let text = term.to_string();
            assert_eq!(Query::parse(&text).unwrap().expr(), &term, "{text}");
        }
    }

    #[test]
    fn column_tones_and_widths() {
        assert_eq!(column_tone("status", "502"), Some(Tone::Danger));
        assert_eq!(
            column_tone("http.response.status_code", "404"),
            Some(Tone::Warning)
        );
        assert_eq!(column_tone("status", "200"), None);
        assert_eq!(column_tone("duration_us", "1500000"), Some(Tone::Warning));
        assert_eq!(column_tone("duration_us", "900000"), None);
        assert_eq!(column_tone("latency_ms", "1200"), Some(Tone::Warning));
        assert_eq!(column_tone("duration", "0.4"), None);
        assert_eq!(column_tone("duration", "2.5"), Some(Tone::Warning));
        assert_eq!(column_tone("path", "/v1/orders"), None);
        assert_eq!(column_width("path"), WIDE_COLUMN_WIDTH);
        assert_eq!(column_width("http.request.url"), WIDE_COLUMN_WIDTH);
        assert_eq!(column_width("status"), COLUMN_WIDTH);
    }

    #[test]
    fn epoch_time_shows_its_utc_date_beside_the_raw_value() {
        let record = Record::parse(br#"{"time":1759276800,"x":"1"}"#, Format::JsonLines).unwrap();
        let rows = detail_rows(&record, Some(1_759_276_800_000_000_000));
        assert_eq!(rows[0].1, "1759276800 · 2025-10-01 00:00:00.000 UTC");
        assert_eq!(rows[1].1, "1");
    }
}
