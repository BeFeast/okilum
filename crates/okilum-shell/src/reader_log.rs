//! Read-only structured log view (#602): virtualized one-line rows over a
//! `okilum_core::log` index, a filter bar (text, level, time window and an
//! hl-compatible query, #1158) evaluated off the UI thread, a detail pane for
//! the selected record, a raw-line copy, and follow mode (#1159): the file is
//! polled with `okilum_core::log::tail`, appended records are filtered as they
//! arrive, and a truncation or rotation is announced, not hidden. More files
//! can join the view (#1160): their records interleave in time order through
//! `okilum_core::log::merge`, each with its colour and name, and any file can
//! be hidden without indexing anything again.
use super::*;
use gpui_component::scroll::ScrollableElement as _;
use okilum_core::log::correlation;
use okilum_core::log::export::{self, ExportFormat};
use okilum_core::log::histogram::Histogram;
use okilum_core::log::merge::MergedLog;
use okilum_core::log::query::{Expr, FieldRef, Literal, Test};
use okilum_core::log::saved_query::SavedQuery;
use okilum_core::log::source_link::{self, Open, SourceLocation};
use okilum_core::log::tail::{ResetReason, TailEvent};
use okilum_core::log::{
    ansi, block, filter, timestamp, FilterGenerations, Format, Level, LogFile, LogFilter, LogIndex,
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
/// The file name column of a view over several files (#1160).
const SOURCE_NAME_WIDTH: f32 = 140.;
/// Field columns (#1167): narrow for numbers and short values, wide for
/// paths and URLs.
const COLUMN_WIDTH: f32 = 96.;
const WIDE_COLUMN_WIDTH: f32 = 240.;
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
        ExtendPreviousRecord,
        ExtendNextRecord,
        CopyRawLine,
        FocusLogFilter
    ]
);

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPreviousRecord, Some(CONTEXT)),
        KeyBinding::new("down", SelectNextRecord, Some(CONTEXT)),
        KeyBinding::new("shift-up", ExtendPreviousRecord, Some(CONTEXT)),
        KeyBinding::new("shift-down", ExtendNextRecord, Some(CONTEXT)),
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
    Segment(Arc<Segment>),
    Owned { index: LogIndex, bytes: Vec<u8> },
}

/// The name being typed to save the shown filter (#1169).
struct SavingFilter {
    input: Entity<InputState>,
    error: Option<String>,
    _subscription: Subscription,
}

/// Which rows an export takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportScope {
    /// Every row the filters show.
    Shown,
    /// The selected rows.
    Selected,
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
    fn locate(&self, id: usize) -> Option<(usize, usize)> {
        match &self.merged {
            Some(log) => log.get(id).map(|row| (row.file().index(), row.entry())),
            None => Some((0, id)),
        }
    }

    /// Writes every row in order; returns how many.
    fn write(&self, out: &mut dyn std::io::Write, format: ExportFormat) -> std::io::Result<usize> {
        let several = self.merged.is_some();
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
                data,
                entry,
                format: index.format(),
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
enum IndexCopy {
    Held(Snapshot),
    Copied(LogIndex),
}

impl IndexCopy {
    fn get(&self) -> &LogIndex {
        match self {
            IndexCopy::Held(snapshot) => snapshot.parts().0,
            IndexCopy::Copied(index) => index,
        }
    }
}

impl Snapshot {
    fn parts(&self) -> (&LogIndex, &[u8]) {
        match self {
            Snapshot::File(file) => (file.index(), file.bytes()),
            Snapshot::Segment(segment) => (segment.index(), segment.bytes()),
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

/// A file added to the view (#1160).
struct ExtraSource {
    data: SourceData,
    enabled: bool,
    /// The source a segment came from (its own index for an added file),
    /// and the segment's place among that source's segments: a file's
    /// earlier content comes before it when nothing else orders them.
    owner: usize,
    seq: usize,
}

enum SourceData {
    File(Arc<LogFile>),
    /// A file's content before follow mode saw it rotated or truncated
    /// (#1159, #1160): kept, listed under its own name, never polled.
    Segment(Arc<Segment>),
    /// An added file while follow mode is on.
    Tail(Box<LogTail>),
}

impl SourceData {
    fn data(&self) -> &dyn LogData {
        match self {
            SourceData::File(file) => file.as_ref(),
            SourceData::Segment(segment) => segment.as_ref(),
            SourceData::Tail(tail) => tail.as_ref(),
        }
    }
}

/// The finished content of a followed file, under a label of its own.
struct Segment {
    path: PathBuf,
    segment: okilum_core::log::tail::LogSegment,
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
}

/// Several shown sources in time order. Rows of the view are rows of `log`.
struct Merge {
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

pub(crate) struct LogView {
    state: State,
    /// Entry index (into the log index) of the selected record.
    selected: Option<usize>,
    /// Where a range of selected rows started (#1169); `selected` is its other end.
    anchor: Option<usize>,
    /// An export is being chosen or written; one at a time.
    exporting: bool,
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
    /// Files added to this view (#1160); the view's own file is source 0.
    extra: Vec<ExtraSource>,
    /// Source 0 is shown.
    primary_enabled: bool,
    /// The time order of the shown sources, when there is more than one.
    merge: Option<Merge>,
    /// Showing one request (#1166): the query text to go back to.
    request_back: Option<String>,
    /// Records per level over time for the shown rows (#1168), the rows it
    /// was counted for, and where the bars were last drawn.
    histogram: Option<Arc<Histogram>>,
    histogram_for: Option<(usize, usize)>,
    histogram_at: Option<std::time::Instant>,
    histogram_bounds: Rc<std::cell::Cell<Bounds<Pixels>>>,
    /// A press on the bars: where it started and where the pointer is.
    histogram_press: Option<(Pixels, Pixels)>,
    _histogram: Option<Task<()>>,
    /// A count that waits for the next second of a followed log.
    _histogram_wait: Option<Task<()>>,
    /// Naming the shown filter to save it (#1169).
    saving: Option<SavingFilter>,
    /// What the last source-link click asked for, instead of launching (#1165).
    #[cfg(test)]
    last_open: Option<Open>,
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
            anchor: None,
            exporting: false,
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
            extra: Vec::new(),
            primary_enabled: true,
            merge: None,
            request_back: None,
            histogram: None,
            histogram_for: None,
            histogram_at: None,
            histogram_bounds: Rc::default(),
            histogram_press: None,
            _histogram: None,
            _histogram_wait: None,
            saving: None,
            #[cfg(test)]
            last_open: None,
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

    fn compressed(&self) -> bool {
        self.file().is_some_and(|file| file.is_compressed())
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

    fn snapshot_of(&self, source: usize) -> Option<Snapshot> {
        match (source, &self.follow) {
            (0, Some(follow)) => Some(Snapshot::Owned {
                index: follow.tail.index().clone(),
                bytes: follow.tail.bytes()[..follow.tail.index().bytes() as usize].to_vec(),
            }),
            (0, None) => self.file().cloned().map(Snapshot::File),
            (n, _) => self.extra.get(n - 1).map(|extra| match &extra.data {
                SourceData::File(file) => Snapshot::File(file.clone()),
                SourceData::Segment(segment) => Snapshot::Segment(segment.clone()),
                SourceData::Tail(tail) => Snapshot::Owned {
                    index: tail.index().clone(),
                    bytes: tail.bytes()[..tail.index().bytes() as usize].to_vec(),
                },
            }),
        }
    }

    /// Source `source`: 0 is the view's own file (or its followed copy).
    fn source(&self, source: usize) -> Option<&dyn LogData> {
        match source {
            0 => self.data(),
            n => self.extra.get(n - 1).map(|extra| extra.data.data()),
        }
    }

    fn source_count(&self) -> usize {
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

    fn source_name(&self, source: usize) -> String {
        self.source(source)
            .and_then(|data| data.path().file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Rows of the view before any filter: every record of the shown sources.
    fn total(&self) -> usize {
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
    fn locate(&self, id: usize) -> Option<(usize, usize)> {
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
    fn id_of(&self, source: usize, entry: usize) -> Option<usize> {
        match &self.merge {
            Some(merge) => {
                let file = merge.sources.iter().position(|&s| s == source)?;
                merge.positions[file].get(entry).map(|row| *row as usize)
            }
            None => (self.enabled_sources().first() == Some(&source)).then_some(entry),
        }
    }

    fn raw_at(&self, id: usize) -> Option<&[u8]> {
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

    fn set_source_enabled(&mut self, source: usize, enabled: bool, cx: &mut Context<Self>) {
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
    fn add_files(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let opened = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .map(|path| LogFile::open(&path).map_err(|error| (path, error)))
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
    pub(crate) fn has_file(&self, path: &Path) -> bool {
        (0..self.source_count()).any(|source| self.source(source).map(LogData::path) == Some(path))
    }

    pub(crate) fn add_file_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.add_files(paths, cx);
    }

    fn prompt_add_files(&mut self, cx: &mut Context<Self>) {
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
    fn open_location(
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

    #[cfg(test)]
    fn launch(&mut self, open: Open, _: &mut Window, _: &mut Context<Self>) {
        self.last_open = Some(open);
    }

    #[cfg(not(test))]
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

    /// Every record of the request `id`, across all files of the view
    /// (#1166). The previous query comes back with `request_back`.
    fn show_request(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.request_back.is_none() {
            self.request_back = Some(self.query_text.clone());
        }
        self.replace_query(correlation::query(id).to_string(), window, cx);
    }

    fn request_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(previous) = self.request_back.take() {
            self.replace_query(previous, window, cx);
        }
    }

    /// Field columns of this view (#1167), remembered per file.
    fn columns(&self, cx: &App) -> Vec<String> {
        self.history_key()
            .map(|key| reader_ui_state::log_columns(&key, cx))
            .unwrap_or_default()
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
    fn field_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let total = self.total().min(COLUMN_MENU_SAMPLE);
        for id in 0..total {
            let Some((source, entry)) = self.locate(id) else {
                continue;
            };
            let Some(record) = self.source(source).and_then(|data| data.record(entry)) else {
                continue;
            };
            for field in record.fields {
                if !names.contains(&field.key) {
                    names.push(field.key);
                }
            }
        }
        names
    }

    fn toggle_column(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(file) = self.history_key() else {
            return;
        };
        let mut columns = reader_ui_state::log_columns(&file, cx);
        match columns.iter().position(|column| column == key) {
            Some(at) => {
                columns.remove(at);
            }
            None => columns.push(key.to_owned()),
        }
        reader_ui_state::set_log_columns(&file, columns, cx);
        cx.notify();
    }

    /// Counts the shown rows per level over time off the UI thread when they
    /// changed since the last count (#1168). While a log is followed a count
    /// starts at most once a second, like a time window that moves with it.
    fn recount(&mut self, cx: &mut Context<Self>) {
        let signature = (self.visible_len(), self.total());
        if self.histogram_for == Some(signature) || self._histogram_wait.is_some() {
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
                if this.histogram_for == Some(signature) {
                    this.histogram = (!counted.is_empty()).then(|| Arc::new(counted));
                    cx.notify();
                }
            });
        }));
    }

    /// The index of `source` for a background count: shared where the source
    /// is a file, a copy of the index (not of the bytes) where it grows.
    fn index_copy(&self, source: usize) -> Option<IndexCopy> {
        let tail = match (source, &self.follow) {
            (0, Some(follow)) => Some(&follow.tail),
            (0, None) => None,
            (n, _) => match self.extra.get(n - 1).map(|extra| &extra.data) {
                Some(SourceData::Tail(tail)) => Some(tail.as_ref()),
                _ => None,
            },
        };
        match tail {
            Some(tail) => Some(IndexCopy::Copied(tail.index().clone())),
            None => self.snapshot_of(source).map(IndexCopy::Held),
        }
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

    fn following(&self) -> bool {
        self.follow
            .as_ref()
            .is_some_and(|follow| follow.poll.is_some())
    }

    /// Rows currently listed.
    fn visible_len(&self) -> usize {
        match &self.rows {
            Some(rows) => rows.len(),
            None => self.total(),
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
        self.anchor = None;
        if let Some(row) = self.row_of(index).filter(|_| reveal) {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Moves the end of the selected rows to `index`, keeping the other end
    /// where it was (shift-click, shift-arrows).
    fn extend_to(
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
    fn selection(&self) -> Option<Range<usize>> {
        let cursor = self.row_of(self.selected?)?;
        match self.anchor.and_then(|anchor| self.row_of(anchor)) {
            Some(anchor) => Some(anchor.min(cursor)..anchor.max(cursor) + 1),
            None => Some(cursor..cursor + 1),
        }
    }

    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.step_by(delta, false, window, cx)
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
        self.request_back = None;
        self.update_filter(|filter| *filter = LogFilter::default(), cx);
    }

    /// Starts a run for the current filter. Every earlier run is cancelled by
    /// the new generation, and a result that arrives late is dropped.
    fn refilter(&mut self, cx: &mut Context<Self>) {
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

    /// The run over several files (#1160): each file is filtered on its own,
    /// with the time window ending at the newest record of all of them, and
    /// its matches are placed at their rows of the merged view.
    fn refilter_merged(&mut self, cx: &mut Context<Self>) {
        let ticket = self.generations.next();
        if !self.filter.is_active() {
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
        let snapshots: Vec<Snapshot> = merge
            .sources
            .iter()
            .filter_map(|&source| self.snapshot_of(source))
            .collect();
        let positions = merge.positions.clone();
        self.filtering = true;
        let filter = self.filter.clone();
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
            let Some(rows) = rows else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
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
    fn toggle_follow(&mut self, cx: &mut Context<Self>) {
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
                Some(data) => Some(data.path().to_owned()),
                None => return,
            },
        };
        let added: Vec<(usize, PathBuf)> = self
            .extra
            .iter()
            .enumerate()
            .filter_map(|(slot, extra)| match &extra.data {
                SourceData::File(file) => Some((slot, file.path().to_owned())),
                _ => None,
            })
            .collect();
        self.follow_failed = None;
        self._follow_open = Some(cx.spawn(async move |this, cx| {
            let (primary, added) = cx
                .background_executor()
                .spawn(async move {
                    let open = |path: &Path| {
                        let interval = okilum_core::log::tail::poll_interval_for(path);
                        LogTail::open(path).map(|tail| tail.with_interval(interval))
                    };
                    let primary = primary.map(|path| open(&path));
                    let added: Vec<_> = added
                        .into_iter()
                        .map(|(slot, path)| (slot, open(&path)))
                        .collect();
                    (primary, added)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match primary {
                    Some(Ok(tail)) => {
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
                // Same bytes from the start: the selection keeps its record.
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
            let Ok(requests) = this.update(cx, |this, _| this.begin_polls()) else {
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

    fn begin_polls(&mut self) -> Vec<(usize, okilum_core::log::tail::PollRequest)> {
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
        for (source, read) in reads {
            self.apply_poll(source, read, cx);
        }
        self.keep_pinned();
        cx.notify();
    }

    fn apply_poll(
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
            TailEvent::Appended {
                entries,
                reindexed,
                grown,
            } => {
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
                if earlier > 0 {
                    let what = match reason {
                        ResetReason::Truncated => "before truncation",
                        ResetReason::Rotated => "before rotation",
                    };
                    let name = self.source_name(source);
                    let seq = self
                        .extra
                        .iter()
                        .filter(|extra| extra.owner == source)
                        .filter(|extra| matches!(extra.data, SourceData::Segment(_)))
                        .count();
                    self.extra.push(ExtraSource {
                        data: SourceData::Segment(Arc::new(Segment {
                            path: PathBuf::from(format!("{name} ({what})")),
                            segment: previous,
                        })),
                        enabled: true,
                        owner: source,
                        seq,
                    });
                }
                self.selected = None;
                self.anchor = None;
                self.rebuild_merge();
                self.refilter(cx);
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
                            ..self.filter.clone()
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

    /// Copies the selected record's raw lines; several records are copied as
    /// they are in the file, one after another.
    fn copy_raw(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            bytes.extend_from_slice(raw);
            bytes.extend_from_slice(line_ending);
            ending = line_ending.len();
        }
        // Lines, not a file: no ending after the last one.
        bytes.truncate(bytes.len() - ending);
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
        let widths: Vec<f32> = columns.iter().map(|key| this.width_of(key, cx)).collect();
        let palette = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        let hover = brand::reader_palette(cx).hover;
        let muted = cx.theme().muted_foreground;
        let mono = cx.theme().mono_font_family.clone();
        let chosen = this.selection();
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
                let cells: Vec<Option<String>> = if columns.is_empty() {
                    Vec::new()
                } else {
                    let record = file.record(local);
                    columns
                        .iter()
                        .map(|key| {
                            record.as_ref().and_then(|record| {
                                record
                                    .fields
                                    .iter()
                                    .find(|field| &field.key == key)
                                    .map(|field| field.value.clone())
                            })
                        })
                        .collect()
                };
                let row = row_text(file, local);
                let lines = row.lines;
                let selected = chosen.as_ref().is_some_and(|rows| rows.contains(&position));
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
                            .child(row.time),
                    )
                    .when(merged, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .w(px(SOURCE_NAME_WIDTH))
                                .truncate()
                                .text_size(px(12.))
                                .text_color(muted)
                                .child(this.source_name(source)),
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
                            .child(level.badge()),
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
                                    .child(value)
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
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.modifiers().shift {
                            this.extend_to(index, false, window, cx)
                        } else {
                            this.select(index, false, window, cx)
                        }
                    }))
                    .into_any_element()
            })
            .collect()
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
                            1 => format!("line {}", grouped(entry.line())),
                            n => format!(
                                "lines {}–{}",
                                grouped(entry.line()),
                                grouped(entry.line() + n as u64 - 1)
                            ),
                        }
                    )),
            )
            .children(
                file.record(index)
                    .as_ref()
                    .and_then(|record| correlation::ids(record).into_iter().next())
                    .map(|id| {
                        let value = id.value.clone();
                        Button::new("log-show-request")
                            .debug_selector(|| "log-show-request".into())
                            .ghost()
                            .small()
                            .label("Show this request")
                            .tooltip(format!("Every record with {} {}", id.key, id.value))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show_request(&value, window, cx)
                            }))
                    }),
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
        let links = record
            .as_ref()
            .map(source_link::in_record)
            .unwrap_or_default();
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
                        let field_links: Vec<SourceLocation> = links
                            .iter()
                            .filter(|found| found.field == position && value == field.value)
                            .map(|found| found.location.clone())
                            .collect();
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
                                                    &field_value,
                                                    window,
                                                    cx,
                                                )
                                            })),
                                    )
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
                    let text = String::from_utf8_lossy(&ansi::strip(raw)).into_owned();
                    let found = source_link::find(&text);
                    div()
                        .debug_selector(|| "log-detail-raw".into())
                        .font_family(mono.clone())
                        .text_size(px(12.))
                        .child(self.linked("log-detail-raw", text, found, cx))
                }),
        };
        // A record's continuation lines, frame by frame as written; an entry
        // that is no record shows all its lines above.
        let body = body.when_some(
            block::continuation(raw).filter(|_| is_record),
            |body, rest| {
                let text = String::from_utf8_lossy(&ansi::strip(rest)).into_owned();
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
            .is_some_and(|file| file.index().stats().timestamped > 0)
            || self
                .extra
                .iter()
                .any(|extra| extra.data.data().index().stats().timestamped > 0);
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
            self.total(),
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
            .children(self.request_back.as_ref().map(|_| {
                bar_button("log-request-back", true)
                    .label("Back")
                    .icon(IconName::ArrowLeft)
                    .tooltip("Back to the previous filter")
                    .debug_selector(|| "log-request-back".into())
                    .on_click(cx.listener(|this, _, window, cx| this.request_back(window, cx)))
            }))
            .children(history)
            .child(self.saved_button(cx))
            .child(self.export_button(cx))
            .child(self.columns_button(cx))
            .child(self.files_button(cx))
            // A compressed log does not grow (#1164).
            .when(!self.compressed(), |bar| bar.child(self.follow_button(cx)))
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

    /// Starts naming the shown filter to save it (#1169).
    fn begin_saving(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Name this filter"));
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
    pub(crate) fn dismiss_saving(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
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
    fn commit_saving(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let query = match SavedQuery::capture(&name, &self.filter) {
            Ok(query) => query,
            Err(error) => return fail(self, error.to_string(), cx),
        };
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
    fn apply_saved(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(saved) = reader_ui_state::saved_log_filters(cx).get(name).cloned() else {
            return;
        };
        let filter = match saved.to_filter() {
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
        self.request_back = None;
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
        self.update_filter(|current| *current = filter, cx);
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
                .h(px(BAR_HEIGHT))
                .px_3()
                .gap_2()
                .border_b_1()
                .border_color(palette.border_subtle)
                .key_context("LogSaving")
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.))
                        .text_color(muted)
                        .child("Save the shown filter for every log as"),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(260.))
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

    /// The saved filters: apply one, save the shown filter, delete one.
    fn saved_button(&self, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let view = cx.entity().downgrade();
        let names: Vec<String> = reader_ui_state::saved_log_filters(cx)
            .sorted()
            .iter()
            .map(|query| query.name.clone())
            .collect();
        let can_save = self.filter.is_active();
        bar_button("log-saved", false)
            .icon(IconName::Star)
            .tooltip("Saved filters")
            .debug_selector(|| "log-saved".into())
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
                let mut menu = menu;
                for name in &names {
                    let (view, name) = (view.clone(), name.clone());
                    menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                        move |_, window, cx| {
                            let _ = view.update(cx, |this, cx| this.apply_saved(&name, window, cx));
                        },
                    ));
                }
                if names.is_empty() {
                    menu = menu.item(PopupMenuItem::new("No saved filters yet").disabled(true));
                }
                let save_view = view.clone();
                menu = menu.separator().item(
                    PopupMenuItem::new("Save the shown filter…")
                        .disabled(!can_save)
                        .on_click(move |_, window, cx| {
                            let _ = save_view.update(cx, |this, cx| this.begin_saving(window, cx));
                        }),
                );
                if !names.is_empty() {
                    let (view, names) = (view.clone(), names.clone());
                    menu = menu.submenu("Delete", window, cx, move |menu, _, _| {
                        let mut menu = menu;
                        for name in &names {
                            let (view, name) = (view.clone(), name.clone());
                            menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                                move |_, window, cx| {
                                    reader_ui_state::delete_log_filter(&name, cx);
                                    reader_toast::transient(
                                        format!("Deleted “{name}”"),
                                        window,
                                        cx,
                                    );
                                    let _ = view.update(cx, |_, cx| cx.notify());
                                },
                            ));
                        }
                        menu
                    });
                }
                menu
            })
            .into_any_element()
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
    fn export_rows(
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
        let suggested = match format {
            ExportFormat::Raw => format!(
                "{stem}-{}.{}",
                if scope == ExportScope::Shown {
                    "filtered"
                } else {
                    "selected"
                },
                first
                    .extension()
                    .map_or("log".into(), |ext| ext.to_string_lossy())
            ),
            ExportFormat::JsonLines => format!(
                "{stem}-{}.jsonl",
                if scope == ExportScope::Shown {
                    "filtered"
                } else {
                    "selected"
                }
            ),
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
                            write_export(&job, &logs, &path, format).map(|n| (n, path))
                        })
                        .await;
                    Some((counted, result))
                }
                _ => None,
            };
            let _ = this.update_in_own_window(cx, |this, window, cx| {
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

    /// Export of the shown rows, or of a selection of several.
    fn export_button(&self, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let view = cx.entity().downgrade();
        let shown = self.visible_len();
        let selected = self.selection().map_or(0, |rows| rows.len());
        bar_button("log-export", false)
            .icon(IconName::ArrowDown)
            .tooltip("Export rows")
            .debug_selector(|| "log-export".into())
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let mut menu = menu;
                let mut scopes = vec![(ExportScope::Shown, shown, "shown")];
                if selected > 1 {
                    scopes.push((ExportScope::Selected, selected, "selected"));
                }
                for (at, (scope, count, what)) in scopes.into_iter().enumerate() {
                    if at > 0 {
                        menu = menu.separator();
                    }
                    for (format, how) in [
                        (ExportFormat::Raw, "raw lines"),
                        (ExportFormat::JsonLines, "JSON lines"),
                    ] {
                        let view = view.clone();
                        menu = menu.item(
                            PopupMenuItem::new(format!(
                                "Export {} {what} as {how}…",
                                grouped(count as u64)
                            ))
                            .disabled(count == 0)
                            .on_click(move |_, window, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.export_rows(scope, format, window, cx)
                                });
                            }),
                        );
                    }
                }
                menu
            })
            .into_any_element()
    }

    /// The files of this view: show or hide each, or add more (#1160).
    fn files_button(&self, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let view = cx.entity().downgrade();
        let sources: Vec<(usize, String, bool)> = (0..self.source_count())
            .map(|source| {
                (
                    source,
                    self.source_name(source),
                    self.source_enabled(source),
                )
            })
            .collect();
        let several = sources.len() > 1;
        let hidden = sources.iter().any(|(_, _, enabled)| !enabled);
        bar_button("log-files", several && hidden)
            .label(if several {
                format!("{} files", sources.len())
            } else {
                "Files".to_owned()
            })
            .dropdown_caret(true)
            .tooltip("Files in this view")
            .debug_selector(|| "log-files".into())
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let mut menu = menu;
                for (source, name, enabled) in &sources {
                    let (view, source, enabled) = (view.clone(), *source, *enabled);
                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(enabled).on_click(
                        move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.set_source_enabled(source, !enabled, cx)
                            });
                        },
                    ));
                }
                let view = view.clone();
                menu.separator()
                    .item(PopupMenuItem::new("Add file…").on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| this.prompt_add_files(cx));
                    }))
            })
            .into_any_element()
    }

    /// Fields of the first records as columns, with the shown ones checked.
    fn columns_button(&self, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let view = cx.entity().downgrade();
        let shown = self.columns(cx);
        bar_button("log-columns-menu", !shown.is_empty())
            .icon(IconName::LayoutDashboard)
            .tooltip("Columns")
            .debug_selector(|| "log-columns-menu".into())
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
                let Some(this) = view.upgrade() else {
                    return menu;
                };
                let names = this.read(cx).field_names();
                let mut menu = menu;
                for name in names {
                    let (view, checked) = (view.clone(), shown.contains(&name));
                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(checked).on_click(
                        move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| this.toggle_column(&name, cx));
                        },
                    ));
                }
                menu
            })
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
        // Any file with records keeps the bar, even with every file hidden.
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
                    matches!(self.state, State::Ready(_))
                        && self.follow.is_none()
                        && !self.compressed(),
                    |list| list.child(self.follow_button(cx)),
                )
        };
        // Column titles above the rows, on the rows' grid (#1167).
        let list = if columns.is_empty() || count == 0 {
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
                        .child(div().flex_none().w(px(92.)).child("Time"))
                        .when(merged, |row| {
                            row.child(div().flex_none().w(px(SOURCE_NAME_WIDTH)).child("File"))
                        })
                        .child(div().flex_none().w(px(28.)))
                        .children(columns.iter().enumerate().map(|(at, key)| {
                            let column = key.clone();
                            h_flex()
                                .debug_selector(move || format!("log-column-{at}"))
                                .relative()
                                .flex_none()
                                .w(px(self.width_of(key, cx)))
                                .gap_1()
                                .child(div().min_w_0().truncate().child(key.clone()))
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
                        .child(div().flex_1().child("Message"))
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
            // Log files dropped from the system join the view (#1160).
            .drag_over::<ExternalPaths>(|style, _, _, cx| {
                style.bg(brand::palette(cx).selected.opacity(0.35))
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_files(paths, window, cx)
            }))
            .when(has_entries || self.follow.is_some(), |view| {
                view.child(self.render_filter_bar(cx))
            })
            .children(self.render_saving(cx))
            .children(self.render_histogram(cx))
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

/// Writes `job` to `path` through a temporary file next to it, so the file
/// appears whole or not at all; returns how many rows. A log being read is
/// never the destination.
fn write_export(
    job: &ExportJob,
    logs: &[PathBuf],
    path: &Path,
    format: ExportFormat,
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
        open_named("app.jsonl", text.as_bytes(), cx)
    }

    /// Opens a file named `name` holding `bytes`, as a path given on launch.
    fn open_named<'a>(
        name: &str,
        bytes: &[u8],
        cx: &'a mut TestAppContext,
    ) -> (tempfile::TempDir, Entity<Reader>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join(name), bytes).unwrap();
        let state = root.join(".state");
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(root.join(name)),
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

    #[gpui::test]
    fn a_rotated_gzip_log_opens_decoded_and_without_follow(cx: &mut TestAppContext) {
        // Packed by the system gzip; the text is the pack's 649-line access log.
        let packed = include_bytes!("../../okilum-core/tests/fixtures/log/names/access.log.1.gz");
        let (_directory, reader, visual) = open_named("app.log.1.gz", packed, cx);
        let view = log_view(&reader, visual);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(visual, |view, _| {
            let file = view.file().unwrap();
            assert!(file.is_compressed());
            assert_eq!(file.index().format(), Format::AccessLog);
            assert_eq!(view.visible_len(), 649);
        });
        assert!(
            visual.debug_bounds("log-follow").is_none(),
            "a compressed log is not followed"
        );
    }

    #[gpui::test]
    fn a_glog_file_name_opens_in_the_log_view(cx: &mut TestAppContext) {
        let name = "ordgw.trade-gw-01.god.log.INFO.20261010-060400.4411";
        let text = include_bytes!(
            "../../okilum-core/tests/fixtures/log/names/ordgw.trade-gw-01.god.log.INFO.20261010-060400.4411"
        );
        let (_directory, reader, visual) = open_named(name, text, cx);
        let view = log_view(&reader, visual);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(visual, |view, _| {
            assert_eq!(view.file().unwrap().index().format(), Format::Glog);
            assert_eq!(view.visible_len(), 26);
        });
        // Positive control for the test above: an uncompressed log has Follow.
        assert!(visual.debug_bounds("log-follow").is_some());
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
        for poll in 0..600usize {
            if poll == 300 {
                std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
                std::fs::write(&path, "").unwrap();
            }
            // 2–3 records per 250 ms: 10 records a second.
            let burst = if poll.is_multiple_of(2) { 3 } else { 2 };
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            for _ in 0..burst {
                written += 1;
                file.write_all(line(written).as_bytes()).unwrap();
            }
            drop(file);
            visual.executor().advance_clock(Duration::from_millis(250));
            visual.run_until_parked();
            if poll % 50 == 49 || poll == 299 || poll == 300 || poll == 599 {
                // The rows are every warn record written so far, in order:
                // after the rotation the earlier file is listed first under
                // its own name (#1160), so record n is still row n - 1.
                let expected: Vec<usize> = (1..=written)
                    .filter(|n| n.is_multiple_of(10))
                    .map(|n| n - 1)
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
        // earlier records stay listed under their own name (#1160).
        std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
        std::fs::write(&path, line("info", "fresh 1") + &line("warn", "fresh 2")).unwrap();
        tick(visual);
        tick(visual);
        assert_eq!(len(visual), 69, "67 earlier records and the new file's 2");
        view.read_with(visual, |view, _| {
            assert_eq!(view.source_count(), 2);
            assert_eq!(view.source_name(1), "app.jsonl (before rotation)");
            // The earlier records come first, then the new file's.
            assert_eq!(view.locate(0).unwrap().0, 1);
            assert_eq!(view.locate(68).unwrap(), (0, 1));
        });
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

        // Truncation of the followed file keeps its two records as well.
        std::fs::write(&path, line("info", "after truncate")).unwrap();
        tick(visual);
        assert_eq!(len(visual), 70);
        view.read_with(visual, |view, _| {
            assert_eq!(view.source_name(2), "app.jsonl (before truncation)");
        });
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
        assert_eq!(len(visual), 70);
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        tick(visual);
        assert_eq!(len(visual), 71);
    }

    /// #1282: a merged view with a query stays on its newest match while
    /// following, from an empty result through appends and a rotation.
    #[gpui::test]
    fn a_followed_merged_view_with_a_query_stays_at_the_newest_match(cx: &mut TestAppContext) {
        use std::io::Write as _;
        let line = |n: u32, level: &str, msg: &str| {
            format!(
                "{{\"ts\":\"2025-10-01T{:02}:{:02}:{:02}Z\",\"level\":\"{level}\",\"msg\":\"{msg}\"}}\n",
                n / 3600,
                n / 60 % 60,
                n % 60
            )
        };
        let lines = |from: u32, step: u32, count: u32, level: &str| -> String {
            (0..count)
                .map(|i| line(from + i * step, level, &format!("r{}", from + i * step)))
                .collect()
        };
        // Nothing matches level>=warn at first.
        let (directory, reader, visual) = open_log(&line(1, "info", "a1"), cx);
        let root = directory.path().canonicalize().unwrap();
        let a = root.join("app.jsonl");
        let b = root.join("b.jsonl");
        std::fs::write(&b, line(2, "info", "b2")).unwrap();
        let append = |path: &Path, text: &str| {
            let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            file.write_all(text.as_bytes()).unwrap();
        };
        let tick = |visual: &mut VisualTestContext| {
            visual.executor().advance_clock(Duration::from_millis(600));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        // Exactly one poll: the one that reads the new records. A writer that
        // never stops leaves no quiet poll after it to pin the view again.
        let poll = |visual: &mut VisualTestContext| {
            visual
                .executor()
                .advance_clock(okilum_core::log::tail::LOCAL_POLL_INTERVAL);
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        let view = log_view(&reader, visual);
        // (the list overflows, it is scrolled to the end)
        let place = |visual: &mut VisualTestContext| {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.update(|window, cx| window.draw(cx).clear(cx));
            view.read_with(visual, |view, _| {
                let handle = view.scroll.0.borrow();
                let offset = handle.base_handle.offset();
                let max = handle.base_handle.max_offset();
                (max.y > px(0.), -offset.y >= max.y - px(2.))
            })
        };
        view.update(visual, |view, cx| view.add_files(vec![b.clone()], cx));
        visual.run_until_parked();
        view.update(visual, |view, cx| view.set_query("level>=warn".into(), cx));
        visual.run_until_parked();
        assert_eq!(view.read_with(visual, |view, _| view.visible_len()), 0);
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        tick(visual);

        // Appends to both files, interleaved in time so the merge is rebuilt
        // and refiltered: far more matches than fit.
        append(&a, &lines(10, 2, 60, "warn"));
        append(&b, &lines(11, 2, 60, "error"));
        poll(visual);
        assert_eq!(view.read_with(visual, |view, _| view.visible_len()), 120);
        assert_eq!(
            place(visual),
            (true, true),
            "newest match in view after appends"
        );

        // Rotation of the added file, then more records in both files.
        std::fs::rename(&b, b.with_extension("jsonl.1")).unwrap();
        std::fs::write(&b, lines(200, 2, 5, "warn")).unwrap();
        tick(visual);
        tick(visual);
        append(&a, &lines(211, 2, 5, "warn"));
        append(&b, &lines(210, 2, 5, "error"));
        poll(visual);
        assert_eq!(view.read_with(visual, |view, _| view.visible_len()), 135);
        view.read_with(visual, |view, _| {
            assert!(view.following() && view.follow.as_ref().unwrap().pinned)
        });
        assert_eq!(
            place(visual),
            (true, true),
            "newest match in view after rotation"
        );
    }

    /// #1160: follow covers every file of a merged view. A newer record
    /// lands at the end in place, an older one rebuilds the time order, the
    /// query applies to both, and a rotated added file keeps its records.
    #[gpui::test]
    fn follow_covers_every_file_of_a_merged_view(cx: &mut TestAppContext) {
        use std::io::Write as _;
        let line = |second: u32, level: &str, msg: &str| {
            format!(
                "{{\"ts\":\"2025-10-01T00:00:{second:02}Z\",\"level\":\"{level}\",\"msg\":\"{msg}\"}}\n"
            )
        };
        let (directory, reader, visual) =
            open_log(&(line(1, "info", "a1") + &line(3, "warn", "a3")), cx);
        let root = directory.path().canonicalize().unwrap();
        let a = root.join("app.jsonl");
        let b = root.join("b.jsonl");
        std::fs::write(&b, line(2, "info", "b2") + &line(4, "error", "b4")).unwrap();
        let append = |path: &Path, text: &str| {
            let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            file.write_all(text.as_bytes()).unwrap();
        };
        let tick = |visual: &mut VisualTestContext| {
            visual.executor().advance_clock(Duration::from_millis(600));
            visual.run_until_parked();
        };
        let messages = |visual: &mut VisualTestContext, view: &Entity<LogView>| {
            view.read_with(visual, |view, _| {
                (0..view.visible_len())
                    .map(|row| {
                        let id = view.entry_at(row).unwrap();
                        let record = String::from_utf8_lossy(view.raw_at(id).unwrap()).into_owned();
                        record
                            .split("\"msg\":\"")
                            .nth(1)
                            .unwrap()
                            .trim_end_matches("\"}")
                            .to_owned()
                    })
                    .collect::<Vec<_>>()
            })
        };
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.add_files(vec![b.clone()], cx));
        visual.run_until_parked();
        assert_eq!(messages(visual, &view), ["a1", "b2", "a3", "b4"]);
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert!(view.following());
            assert!(
                matches!(view.extra[0].data, SourceData::Tail(_)),
                "b is followed too"
            );
        });

        // Newer records in either file land at the end.
        append(&b, &line(5, "warn", "b5"));
        tick(visual);
        append(&a, &line(6, "info", "a6"));
        tick(visual);
        assert_eq!(
            messages(visual, &view),
            ["a1", "b2", "a3", "b4", "b5", "a6"]
        );

        // The query applies to appended records of every file.
        view.update(visual, |view, cx| view.set_query("level>=warn".into(), cx));
        visual.run_until_parked();
        assert_eq!(messages(visual, &view), ["a3", "b4", "b5"]);
        append(&b, &line(7, "error", "b7"));
        tick(visual);
        assert_eq!(messages(visual, &view), ["a3", "b4", "b5", "b7"]);
        // An older record is placed in time order, not at the end.
        append(&a, &line(0, "warn", "a0 late"));
        tick(visual);
        assert_eq!(messages(visual, &view), ["a0 late", "a3", "b4", "b5", "b7"]);

        // Rotation of the added file keeps its records under their own name.
        view.update(visual, |view, cx| view.set_query(String::new(), cx));
        visual.run_until_parked();
        std::fs::rename(&b, b.with_extension("jsonl.1")).unwrap();
        std::fs::write(&b, line(8, "info", "b8 new file")).unwrap();
        tick(visual);
        tick(visual);
        view.read_with(visual, |view, _| {
            let names: Vec<String> = (0..view.source_count())
                .map(|s| view.source_name(s))
                .collect();
            assert!(
                names.contains(&"b.jsonl (before rotation)".to_owned()),
                "{names:?}"
            );
        });
        let shown = messages(visual, &view);
        for msg in ["b2", "b4", "b5", "b7", "b8 new file"] {
            assert!(shown.contains(&msg.to_owned()), "{msg} in {shown:?}");
        }
        assert_eq!(shown.last().map(String::as_str), Some("b8 new file"));
    }

    /// #1160 acceptance over the sample pack: the Kubernetes stream plus the
    /// kubelet and Apache logs read in one time order, a query covers every
    /// file, hiding a file keeps the selection when its record stays shown,
    /// and a time window ends at the newest record of all files.
    #[gpui::test]
    fn several_files_read_in_time_order_and_filter_together(cx: &mut TestAppContext) {
        let samples =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../okilum-core/tests/fixtures/log/samples");
        let names = [
            "kubernetes/kubernetes-fluentbit.ndjson",
            "kubernetes/kubelet.jsonl",
            "apache/apache-access.logfmt",
            "apache/apache-error.jsonl",
        ];
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let paths: Vec<PathBuf> = names
            .iter()
            .map(|name| {
                let target = root.join(Path::new(name).file_name().unwrap());
                std::fs::copy(samples.join(name), &target).unwrap();
                target
            })
            .collect();
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::bind_keys(cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(paths[0].clone()),
                        session_directory: Some(root.join(".state")),
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
        let view = log_view(&reader, visual);
        let alone = view.read_with(visual, |view, _| view.total());
        view.update(visual, |view, cx| view.add_files(paths[1..].to_vec(), cx));
        visual.run_until_parked();
        let files: Vec<LogFile> = paths
            .iter()
            .map(|path| LogFile::open(path).unwrap())
            .collect();
        let records: usize = files.iter().map(|file| file.index().len()).sum();
        view.read_with(visual, |view, _| {
            assert_eq!(view.source_count(), 4);
            assert_eq!(view.total(), records);
            assert!(records > alone, "positive control: the files joined");
            // Time order across files: no timestamped row is older than one above it.
            let mut last = i64::MIN;
            let mut from_each = [false; 4];
            for id in 0..view.total() {
                let (source, entry) = view.locate(id).unwrap();
                from_each[source] = true;
                if let Some(ts) = view
                    .source(source)
                    .unwrap()
                    .index()
                    .get(entry)
                    .unwrap()
                    .timestamp()
                {
                    assert!(ts >= last, "row {id} goes back in time");
                    last = ts;
                }
            }
            assert_eq!(from_each, [true; 4], "rows of every file");
        });

        // A query over every file: the union of each file's own matches.
        let per_file = |filter: &LogFilter| -> usize {
            files
                .iter()
                .map(|file| {
                    filter::evaluate_range(
                        filter,
                        file.index(),
                        file.bytes(),
                        0..file.index().len(),
                    )
                    .len()
                })
                .sum()
        };
        view.update(visual, |view, cx| view.set_query("level>=error".into(), cx));
        visual.run_until_parked();
        let errors = per_file(&LogFilter {
            query: Some(Query::parse("level>=error").unwrap()),
            ..Default::default()
        });
        assert!(errors > 0);
        assert_eq!(view.read_with(visual, |view, _| view.visible_len()), errors);
        let shown = shown(&view, visual);
        assert!(
            shown.windows(2).all(|pair| pair[0] < pair[1]),
            "rows stay in time order"
        );

        // Hiding a file keeps the selection when its record stays shown.
        let kubelet_row = view.read_with(visual, |view, _| {
            (0..view.total())
                .find(|&id| view.locate(id).unwrap().0 == 1)
                .unwrap()
        });
        view.update(visual, |view, cx| view.set_query(String::new(), cx));
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| {
            view.select(kubelet_row, true, window, cx)
        });
        let access = files[2].index().len();
        view.update(visual, |view, cx| view.set_source_enabled(2, false, cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(view.total(), records - access);
            let selected = view.selected.expect("the kubelet record stays selected");
            assert_eq!(view.locate(selected).unwrap().0, 1);
        });
        // Hiding the selected record's file drops the selection.
        view.update(visual, |view, cx| view.set_source_enabled(1, false, cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| assert_eq!(view.selected, None));
        view.update(visual, |view, cx| {
            view.set_source_enabled(1, true, cx);
            view.set_source_enabled(2, true, cx);
        });
        visual.run_until_parked();

        // The time window ends at the newest record of all files.
        let newest = files
            .iter()
            .filter_map(|file| {
                file.index()
                    .entries()
                    .iter()
                    .filter_map(|e| e.timestamp())
                    .max()
            })
            .max();
        view.update(visual, |view, cx| {
            view.update_filter(|filter| filter.time = TimeWindow::Last5Minutes, cx)
        });
        visual.run_until_parked();
        let windowed = per_file(&LogFilter {
            time: TimeWindow::Last5Minutes,
            window_end: newest,
            ..Default::default()
        });
        assert_eq!(
            view.read_with(visual, |view, _| view.visible_len()),
            windowed
        );
        assert!(
            windowed < records,
            "positive control: the window hides older rows"
        );

        // One stripe and file name per row while several files are shown.
        visual.update(|window, cx| window.draw(cx).clear(cx));
        view.read_with(visual, |view, _| {
            assert!(view.merge.is_some());
            assert!(!view.following() && view.extra.len() == 3);
        });
    }

    /// #1160: log files dropped on the view join it and other files are
    /// named, not opened; extra log paths from the command line open with
    /// the first.
    #[gpui::test]
    fn dropped_and_command_line_logs_join_the_view(cx: &mut TestAppContext) {
        let line = |msg: &str| {
            format!(
                "{{\"ts\":\"2025-10-01T00:00:0{}Z\",\"msg\":\"{msg}\"}}\n",
                msg.len() % 10
            )
        };
        let (directory, reader, visual) = open_log(&line("first"), cx);
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("second.jsonl"), line("second") + &line("again")).unwrap();
        std::fs::write(root.join("notes.md"), "# Notes\n").unwrap();
        let view = log_view(&reader, visual);
        assert_eq!(view.read_with(visual, |view, _| view.source_count()), 1);
        let center = visual.debug_bounds("reader-log").unwrap().center();
        let paths = ExternalPaths(
            [root.join("second.jsonl"), root.join("notes.md")]
                .into_iter()
                .collect(),
        );
        visual.simulate_event(FileDropEvent::Entered {
            position: center,
            paths,
        });
        visual.simulate_event(FileDropEvent::Submit { position: center });
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(view.source_count(), 2, "the log joined, the note did not");
            assert!(view.has_file(&root.join("second.jsonl")));
            assert_eq!(view.total(), 3);
        });
        // Dropping it again adds nothing.
        visual.simulate_event(FileDropEvent::Entered {
            position: center,
            paths: ExternalPaths([root.join("second.jsonl")].into_iter().collect()),
        });
        visual.simulate_event(FileDropEvent::Submit { position: center });
        visual.run_until_parked();
        assert_eq!(view.read_with(visual, |view, _| view.source_count()), 2);
        drop(directory);

        // Command line: `okilum a.jsonl b.jsonl` opens both in one view.
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("a.jsonl"), line("a")).unwrap();
        std::fs::write(root.join("b.jsonl"), line("bb") + &line("bbb")).unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(root.join("a.jsonl")),
                        extra_logs: vec![root.join("b.jsonl")],
                        session_directory: Some(root.join(".state")),
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
        let view = log_view(&entity.unwrap(), visual);
        view.read_with(visual, |view, _| {
            assert_eq!(view.source_count(), 2);
            assert_eq!(view.total(), 3);
            assert!(view.merge.is_some());
        });
    }

    #[gpui::test]
    fn a_followed_stack_trace_grows_its_row_and_is_filtered_again(cx: &mut TestAppContext) {
        use std::io::Write as _;
        let line =
            |level: &str, msg: &str| format!("{{\"level\":\"{level}\",\"msg\":\"{msg}\"}}\n");
        let (directory, reader, visual) = open_log(&line("info", "start"), cx);
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
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        tick(visual);
        // The word is only in a frame that has not been written yet.
        view.update(visual, |view, cx| view.set_text("Pool.java".into(), cx));
        visual.run_until_parked();
        append(
            &(line("error", "request failed") + "\tat com.example.Handler.run(Handler.java:12)\n"),
        );
        tick(visual);
        assert!(
            shown(&view, visual).is_empty(),
            "positive control: no match yet"
        );
        append("\tat com.example.Pool.take(Pool.java:88)\n");
        tick(visual);
        assert_eq!(shown(&view, visual), [1], "the grown record matches now");
        view.update(visual, |view, cx| view.set_text(String::new(), cx));
        visual.run_until_parked();
        tick(visual);
        assert_eq!(shown(&view, visual), [0, 1], "still two entries, not four");
        view.read_with(visual, |view, _| {
            assert_eq!(row_text(view.data().unwrap(), 1).lines, 3)
        });
    }

    /// #1165 acceptance on the sample pack's AddressSanitizer log: the
    /// frame `order_book.cpp:212:23` is a link in the selected entry, and
    /// opening it asks the chosen editor for that file, line and column.
    #[gpui::test]
    fn asan_frames_open_in_the_chosen_editor(cx: &mut TestAppContext) {
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../okilum-core/tests/fixtures/log/samples/cpp/ordgw_tests-asan.log");
        let text = std::fs::read_to_string(sample).unwrap();
        let state = tempfile::tempdir().unwrap();
        cx.update(|cx| reader_ui_state::install(state.path(), cx));
        let (_directory, reader, visual) = open_log(&text, cx);
        let view = log_view(&reader, visual);
        // The entry holding the frame, whether a record's continuation or a
        // raw block (#1161).
        let (id, location) = view.read_with(visual, |view, _| {
            (0..view.total())
                .find_map(|id| {
                    let raw = String::from_utf8_lossy(view.raw_at(id)?).into_owned();
                    let text = String::from_utf8_lossy(&ansi::strip(raw.as_bytes())).into_owned();
                    source_link::find(&text)
                        .into_iter()
                        .find(|l| l.path.ends_with("order_book.cpp") && l.line == 212)
                        .map(|location| (id, location))
                })
                .expect("the ASan frame is found")
        });
        assert_eq!(location.column, Some(23));
        assert_eq!(location.path, "/src/ordgw/src/md/order_book.cpp");
        view.update_in(visual, |view, window, cx| view.select(id, true, window, cx));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        // The detail pane draws the frames as links.
        assert!(
            visual.debug_bounds("log-detail-frames").is_some()
                || visual.debug_bounds("log-detail-raw").is_some(),
            "the frames are shown"
        );
        // Positive control: nothing was opened yet.
        assert_eq!(
            view.read_with(visual, |view, _| view.last_open.clone()),
            None
        );
        view.update_in(visual, |view, window, cx| {
            view.open_location(&location, window, cx)
        });
        assert_eq!(
            view.read_with(visual, |view, _| view.last_open.clone()),
            Some(Open::Url(
                "vscode://file/src/ordgw/src/md/order_book.cpp:212:23".into()
            ))
        );
        // A custom command gets the same place, word by word.
        visual.update(|_, cx| {
            reader_ui_state::set_log_editor(
                &source_link::Editor::Custom("myedit --goto {file}:{line}:{col}".into()),
                cx,
            )
        });
        view.update_in(visual, |view, window, cx| {
            view.open_location(&location, window, cx)
        });
        assert_eq!(
            view.read_with(visual, |view, _| view.last_open.clone()),
            Some(Open::Command(
                [
                    "myedit",
                    "--goto",
                    "/src/ordgw/src/md/order_book.cpp:212:23"
                ]
                .map(String::from)
                .to_vec()
            ))
        );
        // A build made elsewhere: the vault maps /src/ordgw to a local
        // checkout, and the frame opens there (#1165).
        let checkout = _directory.path().canonicalize().unwrap().join("checkout");
        let root = _directory.path().canonicalize().unwrap();
        visual.update(|_, cx| {
            reader_ui_state::set_log_editor(&source_link::Editor::VsCode, cx);
            reader_ui_state::set_source_maps(
                &root,
                vec![source_link::PathMap {
                    from: "/src/ordgw".into(),
                    to: checkout.clone(),
                }],
                cx,
            );
        });
        view.update_in(visual, |view, window, cx| {
            view.open_location(&location, window, cx)
        });
        let expected = checkout.join("src/md/order_book.cpp");
        assert_eq!(
            view.read_with(visual, |view, _| view.last_open.clone()),
            Some(Open::Url(format!(
                "vscode://file{}:212:23",
                expected.to_string_lossy()
            )))
        );
        // Kept for the next launch.
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.path().join("reader-ui.json")).unwrap())
                .unwrap();
        assert_eq!(
            saved["source_maps"][root.to_string_lossy().as_ref()][0]["from"],
            "/src/ordgw"
        );
    }

    /// #1166 acceptance: with ingress and app logs merged, choosing a
    /// record's request id shows only that request's rows from both files,
    /// and Back returns to the previous filter.
    #[gpui::test]
    fn show_this_request_filters_every_file_and_back_restores(cx: &mut TestAppContext) {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../okilum-core/tests/fixtures/log/correlation");
        let ingress = std::fs::read_to_string(fixtures.join("ingress-nginx.jsonl")).unwrap();
        let (directory, reader, visual) = open_log(&ingress, cx);
        let root = directory.path().canonicalize().unwrap();
        let app = root.join("checkout-api.jsonl");
        std::fs::copy(fixtures.join("checkout-api.jsonl"), &app).unwrap();
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.add_files(vec![app.clone()], cx));
        visual.run_until_parked();
        let messages = |visual: &mut VisualTestContext| {
            view.read_with(visual, |view, _| {
                (0..view.visible_len())
                    .map(|row| {
                        let id = view.entry_at(row).unwrap();
                        let raw = String::from_utf8_lossy(view.raw_at(id).unwrap()).into_owned();
                        let msg = raw.split("\"msg\": \"").nth(1).unwrap();
                        msg[..msg.find('"').unwrap()].to_owned()
                    })
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            messages(visual).len(),
            3 + 7,
            "positive control: both files"
        );
        // A query first, so Back has something to return to.
        view.update(visual, |view, cx| view.set_query("level>=info".into(), cx));
        visual.run_until_parked();
        let before = messages(visual);
        // Select the ingress record of the failed POST and show its request.
        let id = view.read_with(visual, |view, _| {
            (0..view.total())
                .find(|&id| {
                    String::from_utf8_lossy(view.raw_at(id).unwrap()).contains("POST /checkout 502")
                })
                .unwrap()
        });
        view.update_in(visual, |view, window, cx| view.select(id, true, window, cx));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let button = visual
            .debug_bounds("log-show-request")
            .expect("the record offers its request");
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            messages(visual),
            [
                "POST /checkout 502",
                "handling POST /checkout",
                "payment backend timeout"
            ],
            "only that request, from both files, in time order"
        );
        view.read_with(visual, |view, _| {
            assert_eq!(view.request_back.as_deref(), Some("level>=info"));
        });
        // Back returns to the previous filter.
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let back = visual
            .debug_bounds("log-request-back")
            .expect("Back is offered");
        visual.simulate_click(back.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(messages(visual), before);
        view.read_with(visual, |view, cx| {
            assert!(view.request_back.is_none());
            assert_eq!(view.query_input.read(cx).value().as_ref(), "level>=info");
        });
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

    /// #1167 acceptance: the Apache access sample with columns status,
    /// duration_us, path and client is readable at 1440 px, without the
    /// detail pane, and the columns are kept for the file.
    #[gpui::test]
    fn apache_access_reads_in_columns_at_1440(cx: &mut TestAppContext) {
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../okilum-core/tests/fixtures/log/samples/apache/apache-access.logfmt");
        let text = std::fs::read_to_string(sample).unwrap();
        let state = tempfile::tempdir().unwrap();
        cx.update(|cx| reader_ui_state::install(state.path(), cx));
        let (_directory, reader, visual) = open_log(&text, cx);
        visual.simulate_resize(gpui::size(px(1440.), px(900.)));
        visual.run_until_parked();
        let view = log_view(&reader, visual);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("log-columns").is_none(),
            "positive control: no columns yet"
        );
        // One column from the detail pane with a real click, the rest by key.
        view.update_in(visual, |view, window, cx| view.select(0, true, window, cx));
        visual.run_until_parked();
        let status_field = view.read_with(visual, |view, _| {
            let record = view.data().unwrap().record(0).unwrap();
            record
                .fields
                .iter()
                .position(|f| f.key == "status")
                .unwrap()
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let toggle = visual
            .debug_bounds(Box::leak(
                format!("log-toggle-column-{status_field}").into_boxed_str(),
            ))
            .expect("the field offers a column");
        visual.simulate_click(toggle.center(), Modifiers::default());
        visual.run_until_parked();
        view.update(visual, |view, cx| {
            for key in ["duration_us", "path", "client"] {
                view.toggle_column(key, cx);
            }
        });
        // Without the detail pane.
        view.update(visual, |view, cx| {
            view.detail_split.toggle_collapsed();
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let titles: Vec<_> = (0..4)
            .map(|at| {
                visual
                    .debug_bounds(Box::leak(format!("log-column-{at}").into_boxed_str()))
                    .unwrap()
            })
            .collect();
        let log = visual.debug_bounds("reader-log").unwrap();
        // In order, inside the view, with room left for the message.
        assert!(titles
            .windows(2)
            .all(|pair| pair[0].right() <= pair[1].left()));
        assert!(
            titles[3].right() + px(200.) < log.right(),
            "the message keeps room: {:?} in {log:?}",
            titles[3]
        );
        view.read_with(visual, |view, cx| {
            assert_eq!(
                view.columns(cx),
                ["status", "duration_us", "path", "client"]
            );
        });
        // Kept for the file, for the next launch.
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        let key = view.read_with(visual, |view, _| view.history_key().unwrap());
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.path().join("reader-ui.json")).unwrap())
                .unwrap();
        assert_eq!(saved["log_columns"][&key][2], "path");
        // Hiding from the header.
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let remove = visual.debug_bounds("log-column-0").unwrap();
        view.update(visual, |view, cx| view.toggle_column("status", cx));
        visual.run_until_parked();
        let _ = remove;
        view.read_with(visual, |view, cx| {
            assert_eq!(view.columns(cx), ["duration_us", "path", "client"]);
        });
    }

    /// #1167: a column is resized by dragging its header edge, rows follow,
    /// and the width is kept; the Columns menu offers the records' fields.
    #[gpui::test]
    fn columns_resize_by_drag_and_the_menu_lists_fields(cx: &mut TestAppContext) {
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../okilum-core/tests/fixtures/log/samples/apache/apache-access.logfmt");
        let text = std::fs::read_to_string(sample).unwrap();
        let state = tempfile::tempdir().unwrap();
        cx.update(|cx| reader_ui_state::install(state.path(), cx));
        let (_directory, reader, visual) = open_log(&text, cx);
        visual.simulate_resize(gpui::size(px(1440.), px(900.)));
        visual.run_until_parked();
        let view = log_view(&reader, visual);
        let names = view.read_with(visual, |view, _| view.field_names());
        for key in ["status", "duration_us", "path", "client", "req_id"] {
            assert!(names.iter().any(|name| name == key), "{key} in {names:?}");
        }
        view.update(visual, |view, cx| {
            for key in ["status", "path", "client"] {
                view.toggle_column(key, cx);
            }
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let before = visual.debug_bounds("log-column-1").unwrap();
        assert!((before.size.width - px(WIDE_COLUMN_WIDTH)).abs() < px(1.));
        let handle = visual.debug_bounds("log-column-resize-1").unwrap();
        let from = handle.center();
        let to = from + point(px(100.), px(0.));
        visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(
            from + point(px(10.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let after = visual.debug_bounds("log-column-1").unwrap();
        assert!(
            (after.size.width - before.size.width - px(100.)).abs() < px(6.),
            "the column follows the drag: {before:?} -> {after:?}"
        );
        // The next column moved with it: the grid is shared with the rows.
        let client = visual.debug_bounds("log-column-2").unwrap();
        assert!(client.left() >= after.right());
        // Kept for the file.
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        let key = view.read_with(visual, |view, _| view.history_key().unwrap());
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(state.path().join("reader-ui.json")).unwrap())
                .unwrap();
        let width = saved["log_column_widths"][&key]["path"].as_f64().unwrap() as f32;
        assert!((width - f32::from(after.size.width)).abs() < 1., "{width}");
    }

    /// #1168 acceptance on the Kibana sample: the Elasticsearch outage
    /// (05:57:35-38, seven errors) is a red cluster on the bars apart from the
    /// startup FATAL at 05:57:00; dragging over it filters the rows to that
    /// window, Clear brings every row back, and a click on a bar jumps to it.
    #[gpui::test]
    fn kibana_outage_is_a_red_cluster_and_dragging_selects_it(cx: &mut TestAppContext) {
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../okilum-core/tests/fixtures/log/samples/kibana/kibana.ndjson");
        let text = std::fs::read_to_string(sample).unwrap();
        let (_directory, reader, visual) = open_log(&text, cx);
        visual.simulate_resize(gpui::size(px(1440.), px(900.)));
        visual.run_until_parked();
        let view = log_view(&reader, visual);
        let settle = |visual: &mut VisualTestContext| {
            for _ in 0..2 {
                visual.update(|window, cx| window.draw(cx).clear(cx));
                visual.run_until_parked();
            }
        };
        settle(visual);
        let histogram = view
            .read_with(visual, |view, _| view.histogram.clone())
            .expect("the bars are counted");
        let total = view.read_with(visual, |view, _| view.total());
        let red = |counts: &[u32; Level::COUNT]| {
            counts[Level::Error as usize] + counts[Level::Fatal as usize]
        };
        let errors: u32 = histogram.buckets().map(|bucket| red(bucket.counts)).sum();
        assert_eq!(errors, 8, "the sample's level>=error records");
        let red_buckets: Vec<usize> = (0..histogram.len())
            .filter(|&at| red(histogram.bucket(at).unwrap().counts) > 0)
            .collect();
        // The startup FATAL stands alone in the first bucket; the outage is
        // one cluster of seven errors after it.
        assert_eq!(red_buckets.first(), Some(&0), "{red_buckets:?}");
        let cluster = &red_buckets[1..];
        let (first, last) = (cluster[0], *cluster.last().unwrap());
        assert!(
            last - first < histogram.len() / 4,
            "a cluster, not spread out: buckets {red_buckets:?}"
        );
        let in_cluster: u32 = (first..=last)
            .map(|at| red(histogram.bucket(at).unwrap().counts))
            .sum();
        assert_eq!(in_cluster, 7, "the outage's errors");
        // Drag over the cluster on the bars, from the middle of its first
        // bar to the middle of its last.
        let bars = visual
            .debug_bounds("log-histogram")
            .expect("the bars are drawn");
        let n = histogram.len() as f32;
        let x = |bucket: f32| bars.left() + bars.size.width * (bucket / n);
        let y = bars.center().y;
        let from = point(x(first as f32 + 0.5), y);
        let to = point(x(last as f32 + 0.5), y);
        visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(
            from + point(px(5.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        let (time, shown) = view.read_with(visual, |view, _| {
            let shown: Vec<(Option<i64>, Level)> = (0..view.visible_len())
                .map(|row| {
                    let id = view.entry_at(row).unwrap();
                    let (source, entry) = view.locate(id).unwrap();
                    let entry = *view.source(source).unwrap().index().get(entry).unwrap();
                    (entry.timestamp(), entry.level())
                })
                .collect();
            (view.filter.time, shown)
        });
        let (start, end) = histogram.window(first, last).unwrap();
        assert_eq!(
            time,
            TimeWindow::Range {
                from: start,
                to: end
            }
        );
        assert!(shown.len() < total, "the window hides other rows");
        assert!(shown
            .iter()
            .all(|(ts, _)| ts.is_none_or(|ts| (start..=end).contains(&ts))));
        let shown_red = shown
            .iter()
            .filter(|(_, level)| matches!(level, Level::Error | Level::Fatal))
            .count();
        assert_eq!(
            shown_red, 7,
            "the outage is inside the window, the startup FATAL is not"
        );
        settle(visual);
        assert!(visual.debug_bounds("log-histogram-range").is_some());
        let clear = visual.debug_bounds("log-histogram-clear").unwrap();
        visual.simulate_click(clear.center(), Modifiers::default());
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(view.filter.time, TimeWindow::All);
            assert_eq!(view.visible_len(), total);
        });
        settle(visual);
        let again = view
            .read_with(visual, |view, _| view.histogram.clone())
            .expect("the bars are counted again");
        assert_eq!(*again, *histogram, "Clear brings the same bars back");
        // A click on a bar selects the first row at or after its start.
        let bucket = histogram.bucket(first).unwrap();
        visual.simulate_click(point(x(first as f32 + 0.5), y), Modifiers::default());
        visual.run_until_parked();
        let selected = view.read_with(visual, |view, _| {
            let (source, entry) = view.locate(view.selected?)?;
            view.source(source)?.index().get(entry)?.timestamp()
        });
        assert!(
            selected.is_some_and(|ts| (bucket.from..bucket.to).contains(&ts)),
            "{selected:?} not in {}..{}",
            bucket.from,
            bucket.to
        );
    }

    /// #1168: in a view of several files the bars count every file, and with a
    /// query only the rows it shows.
    #[gpui::test]
    fn the_bars_count_every_file_of_a_merged_view(cx: &mut TestAppContext) {
        let samples =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../okilum-core/tests/fixtures/log/samples");
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let paths: Vec<PathBuf> = ["kubernetes/kubelet.jsonl", "apache/apache-error.jsonl"]
            .iter()
            .map(|name| {
                let target = root.join(Path::new(name).file_name().unwrap());
                std::fs::copy(samples.join(name), &target).unwrap();
                target
            })
            .collect();
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::bind_keys(cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(paths[0].clone()),
                        session_directory: Some(root.join(".state")),
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
        let view = log_view(&reader, visual);
        let settle = |visual: &mut VisualTestContext| {
            for _ in 0..2 {
                visual.update(|window, cx| window.draw(cx).clear(cx));
                visual.run_until_parked();
            }
        };
        let counted = |visual: &mut VisualTestContext| {
            view.read_with(visual, |view, _| {
                view.histogram
                    .as_ref()
                    .map(|histogram| (histogram.timed() + histogram.untimed()) as usize)
            })
        };
        settle(visual);
        let alone = view.read_with(visual, |view, _| view.total());
        assert_eq!(counted(visual), Some(alone), "one file");
        view.update(visual, |view, cx| view.add_files(paths[1..].to_vec(), cx));
        visual.run_until_parked();
        settle(visual);
        let total = view.read_with(visual, |view, _| view.total());
        assert!(total > alone, "positive control: the files joined");
        assert_eq!(counted(visual), Some(total), "every record of both files");
        view.update(visual, |view, cx| view.set_query("level>=error".into(), cx));
        visual.run_until_parked();
        settle(visual);
        let shown = view.read_with(visual, |view, _| view.visible_len());
        assert!(shown > 0 && shown < total, "{shown} of {total}");
        assert_eq!(
            counted(visual),
            Some(shown),
            "only the rows the query shows"
        );
    }

    /// #1168: while a log is followed the bars count what arrives, and a
    /// chosen time range stays put as newer records come in.
    #[gpui::test]
    fn the_bars_follow_appends_and_a_range_stays_put(cx: &mut TestAppContext) {
        use std::io::Write as _;
        let line = |second: u32, level: &str| {
            format!(
                "{{\"ts\":\"2025-10-01T00:{:02}:{:02}Z\",\"level\":\"{level}\"}}\n",
                second / 60,
                second % 60
            )
        };
        let at = |second: i64| (1_759_276_800 + second) * 1_000_000_000;
        let text: String = (0..30).map(|second| line(second, "info")).collect();
        let (directory, reader, visual) = open_log(&text, cx);
        let path = directory.path().canonicalize().unwrap().join("app.jsonl");
        let append = |text: &str| {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(text.as_bytes()).unwrap();
        };
        // A count started by a draw waits for the next second of a followed
        // log, so every step lets more than a second pass.
        let tick = |visual: &mut VisualTestContext| {
            visual.executor().advance_clock(Duration::from_millis(1200));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        };
        let view = log_view(&reader, visual);
        let len = |visual: &mut VisualTestContext| view.read_with(visual, |v, _| v.visible_len());
        let counted = |visual: &mut VisualTestContext| {
            view.read_with(visual, |view, _| {
                view.histogram
                    .as_ref()
                    .map(|histogram| (histogram.timed() + histogram.untimed()) as usize)
            })
        };
        tick(visual);
        assert_eq!(counted(visual), Some(30));
        view.update(visual, |view, cx| view.toggle_follow(cx));
        visual.run_until_parked();
        tick(visual);
        append(&(line(30, "error") + &line(31, "info")));
        tick(visual);
        tick(visual);
        assert_eq!(len(visual), 32, "follow shows the appended records");
        assert_eq!(counted(visual), Some(32), "and the bars count them");

        // A range is not a window that ends at the newest record.
        view.update(visual, |view, cx| {
            view.update_filter(|f| f.time = TimeWindow::range(at(10), at(20)), cx)
        });
        visual.run_until_parked();
        tick(visual);
        assert_eq!(len(visual), 11, "seconds 10 to 20, both ends included");
        append(&line(50, "info"));
        tick(visual);
        tick(visual);
        assert_eq!(len(visual), 11, "a newer record is outside a fixed range");
        append(&line(15, "error"));
        tick(visual);
        tick(visual);
        assert_eq!(len(visual), 12, "one inside the range is shown");
        assert_eq!(counted(visual), Some(12), "the bars count what is shown");
        view.read_with(visual, |view, _| {
            assert!(
                view.follow.as_ref().unwrap().last_full_run.is_none(),
                "a range never asks for a full run when records arrive"
            );
        });
    }

    /// #1169 acceptance: a filter saved on one log (name, query, levels) is
    /// listed for every log and applies to another file; a name is required,
    /// Enter saves and Escape cancels, and a saved query this version cannot
    /// read is reported without changing the rows.
    #[gpui::test]
    fn a_saved_filter_applies_to_another_log(cx: &mut TestAppContext) {
        let first = concat!(
            r#"{"level":"info","msg":"boot","status":200}"#,
            "\n",
            r#"{"level":"warn","msg":"slow","status":200}"#,
            "\n",
            r#"{"level":"error","msg":"upstream","status":503}"#,
            "\n",
            r#"{"level":"error","msg":"bad gateway","status":502}"#,
            "\n",
        );
        let second = concat!(
            r#"{"level":"info","msg":"ok","status":500}"#,
            "\n",
            r#"{"level":"warn","msg":"slow","status":504}"#,
            "\n",
            r#"{"level":"error","msg":"missing","status":404}"#,
            "\n",
            r#"{"level":"error","msg":"down","status":500}"#,
            "\n",
            r#"{"level":"fatal","msg":"no status"}"#,
            "\n",
        );
        let state = tempfile::tempdir().unwrap();
        cx.update(|cx| reader_ui_state::install(state.path(), cx));
        let query = "level>=warn and status>=500";
        {
            let (_directory, reader, visual) = open_log(first, cx);
            let view = log_view(&reader, visual);
            visual.update(|window, cx| window.draw(cx).clear(cx));
            assert!(
                visual.debug_bounds("log-saved").is_some(),
                "the Saved button"
            );
            view.update_in(visual, |view, window, cx| {
                view.query_input
                    .update(cx, |input, cx| input.set_value(query, window, cx));
                view.set_query(query.into(), cx);
            });
            visual.run_until_parked();
            assert_eq!(shown(&view, visual), [2, 3], "positive control");
            // The level menu's choice is part of what is saved, too.
            view.update(visual, |view, cx| {
                view.update_filter(|filter| filter.level.min = Some(Level::Warn), cx)
            });
            visual.run_until_parked();
            assert_eq!(shown(&view, visual), [2, 3]);

            // A name is required; the row stays open and says why.
            view.update_in(visual, |view, window, cx| view.begin_saving(window, cx));
            visual.update(|window, cx| window.draw(cx).clear(cx));
            assert!(visual.debug_bounds("log-saving-name").is_some());
            view.update_in(visual, |view, window, cx| view.commit_saving(window, cx));
            visual.update(|window, cx| window.draw(cx).clear(cx));
            assert!(visual.debug_bounds("log-saving-error").is_some());
            assert!(visual.update(|_, cx| reader_ui_state::saved_log_filters(cx).is_empty()));

            // Escape cancels.
            visual.simulate_keystrokes("escape");
            visual.run_until_parked();
            view.read_with(visual, |view, _| {
                assert!(view.saving.is_none(), "cancelled")
            });

            // Enter saves under the typed name.
            view.update_in(visual, |view, window, cx| {
                view.begin_saving(window, cx);
                let input = view.saving.as_ref().unwrap().input.clone();
                input.update(cx, |input, cx| input.set_value("Server errors", window, cx));
            });
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.simulate_keystrokes("enter");
            visual.run_until_parked();
            view.read_with(visual, |view, _| assert!(view.saving.is_none(), "saved"));
            let saved = visual.update(|_, cx| reader_ui_state::saved_log_filters(cx));
            let saved = saved
                .get("server errors")
                .expect("saved under the typed name");
            assert_eq!(saved.min_level, "warn");
            assert_eq!(saved.name, "Server errors");
        }
        // Another log: the filter is listed there and applies to its rows.
        let (_directory, reader, visual) = open_named("other.jsonl", second.as_bytes(), cx);
        let view = log_view(&reader, visual);
        assert_eq!(shown(&view, visual), [0, 1, 2, 3, 4], "positive control");
        view.update_in(visual, |view, window, cx| {
            view.apply_saved("Server errors", window, cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [1, 3]);
        assert_eq!(counter(&view, visual).as_deref(), Some("2 of 5"));
        // The query shows in its canonical spelling, in its field too.
        let canonical = Query::parse(query).unwrap().to_string();
        view.read_with(visual, |view, cx| {
            assert_eq!(view.query_text, canonical);
            assert_eq!(view.query_input.read(cx).value().as_ref(), canonical);
            assert_eq!(view.filter.level.min, Some(Level::Warn));
        });

        // A saved query this version cannot read: reported, rows unchanged.
        visual.update(|_, cx| {
            let broken = SavedQuery {
                name: "Broken".into(),
                query: "status >=".into(),
                ..Default::default()
            };
            assert!(reader_ui_state::save_log_filter(broken, cx));
        });
        view.update_in(visual, |view, window, cx| {
            view.apply_saved("Broken", window, cx)
        });
        visual.run_until_parked();
        assert_eq!(shown(&view, visual), [1, 3], "unchanged");

        // Deleted filters are gone for every log.
        visual.update(|_, cx| reader_ui_state::delete_log_filter("server  ERRORS", cx));
        let left: Vec<String> = visual.update(|_, cx| {
            reader_ui_state::saved_log_filters(cx)
                .sorted()
                .iter()
                .map(|query| query.name.clone())
                .collect()
        });
        assert_eq!(left, ["Broken"]);
    }

    /// #1169 acceptance: the 21 shown rows of the Apache sample export as
    /// exactly those raw lines; JSON lines carry the decoded records in file
    /// order; nothing is written over the log itself, and with no rows shown
    /// there is nothing to ask for.
    #[gpui::test]
    fn export_writes_exactly_the_shown_rows_and_never_over_the_log(cx: &mut TestAppContext) {
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../okilum-core/tests/fixtures/log/samples/apache/apache-access.logfmt");
        let text = std::fs::read_to_string(sample).unwrap();
        let (directory, reader, visual) = open_named("apache-access.logfmt", text.as_bytes(), cx);
        let root = directory.path().canonicalize().unwrap();
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.set_query("status>=500".into(), cx));
        visual.run_until_parked();
        assert_eq!(view.read_with(visual, |view, _| view.visible_len()), 21);
        let status = |line: &str| {
            line.split(' ')
                .find_map(|token| token.strip_prefix("status=")?.parse::<u32>().ok())
        };
        let expected: String = text
            .lines()
            .filter(|line| status(line).is_some_and(|status| status >= 500))
            .map(|line| format!("{line}\n"))
            .collect();
        assert_eq!(expected.lines().count(), 21, "positive control");
        let export = |visual: &mut VisualTestContext, format: ExportFormat, out: PathBuf| {
            view.update_in(visual, |view, window, cx| {
                view.export_rows(ExportScope::Shown, format, window, cx)
            });
            assert!(visual.did_prompt_for_new_path(), "asks where to save");
            visual.simulate_new_path_selection(move |_| Some(out));
            visual.run_until_parked();
        };
        let raw = root.join("errors.log");
        export(visual, ExportFormat::Raw, raw.clone());
        assert_eq!(std::fs::read_to_string(&raw).unwrap(), expected);

        let json = root.join("errors.jsonl");
        export(visual, ExportFormat::JsonLines, json.clone());
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(&json)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 21);
        let status_of = |row: &serde_json::Value| {
            let field = &row["fields"]["status"];
            field.as_i64().or_else(|| field.as_str()?.parse().ok())
        };
        assert!(
            rows.iter()
                .all(|row| status_of(row).is_some_and(|s| s >= 500)),
            "{}",
            rows[0]
        );
        assert!(
            rows.windows(2)
                .all(|pair| pair[0]["line"].as_u64() < pair[1]["line"].as_u64()),
            "in file order"
        );
        assert!(rows[0].get("source").is_none(), "one file has no source");
        assert!(std::fs::read_dir(&root).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp")));

        // Never over the log being read.
        let log = root.join("apache-access.logfmt");
        let before = std::fs::read(&log).unwrap();
        export(visual, ExportFormat::Raw, log.clone());
        assert_eq!(std::fs::read(&log).unwrap(), before, "the log is untouched");

        // No rows shown: nothing to ask for.
        view.update(visual, |view, cx| view.set_query("status>=999".into(), cx));
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| {
            view.export_rows(ExportScope::Shown, ExportFormat::Raw, window, cx)
        });
        assert!(!visual.did_prompt_for_new_path());
    }

    /// #1169: shift-click and shift-arrows select a run of records, which
    /// copy as their lines and export on their own.
    #[gpui::test]
    fn shift_selection_copies_several_records_and_exports_them(cx: &mut TestAppContext) {
        let lines: Vec<String> = (0..6)
            .map(|i| format!(r#"{{"level":"info","msg":"row {i}"}}"#))
            .collect();
        let text = lines.join("\n") + "\n";
        let (directory, reader, visual) = open_log(&text, cx);
        let root = directory.path().canonicalize().unwrap();
        let view = log_view(&reader, visual);
        let selection =
            |visual: &mut VisualTestContext| view.read_with(visual, |v, _| v.selection());
        let copied = |visual: &mut VisualTestContext| {
            view.update_in(visual, |view, window, cx| view.copy_raw(window, cx));
            visual.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap())
        };
        assert_eq!(selection(visual), None, "positive control");
        view.update_in(visual, |view, window, cx| view.select(1, true, window, cx));
        assert_eq!(selection(visual), Some(1..2));
        assert_eq!(copied(visual), lines[1], "one record: its line, as before");

        // Shift-click on a row further down.
        view.update_in(visual, |view, window, cx| {
            view.extend_to(3, false, window, cx)
        });
        assert_eq!(selection(visual), Some(1..4));
        assert_eq!(copied(visual), lines[1..4].join("\n"), "lines, not a file");
        visual.update(|window, cx| window.draw(cx).clear(cx));

        // Shift-arrows move the end that is not the anchor, past it too.
        visual.simulate_keystrokes("shift-up");
        assert_eq!(selection(visual), Some(1..3));
        visual.simulate_keystrokes("shift-up");
        assert_eq!(
            selection(visual),
            Some(1..2),
            "back to the anchor: one record"
        );
        visual.simulate_keystrokes("shift-up");
        assert_eq!(selection(visual), Some(0..2), "and beyond it");
        visual.simulate_keystrokes("shift-down shift-down shift-down");
        assert_eq!(selection(visual), Some(1..4));

        // A plain arrow or click goes back to one record.
        visual.simulate_keystrokes("down");
        assert_eq!(selection(visual), Some(4..5));
        view.update_in(visual, |view, window, cx| view.select(2, false, window, cx));
        assert_eq!(selection(visual), Some(2..3));

        // The selected records export on their own.
        view.update_in(visual, |view, window, cx| {
            view.extend_to(4, false, window, cx);
            view.export_rows(ExportScope::Selected, ExportFormat::Raw, window, cx)
        });
        let out = root.join("selected.log");
        let target = out.clone();
        visual.simulate_new_path_selection(move |_| Some(target));
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            lines[2..5]
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>()
        );
    }

    /// #1169: exporting a view of several files writes its rows in the order
    /// shown, each JSON line saying which file it came from.
    #[gpui::test]
    fn export_of_a_merged_view_names_each_source(cx: &mut TestAppContext) {
        let samples =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../okilum-core/tests/fixtures/log/samples");
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let names = ["kubernetes/kubelet.jsonl", "apache/apache-error.jsonl"];
        let paths: Vec<PathBuf> = names
            .iter()
            .map(|name| {
                let target = root.join(Path::new(name).file_name().unwrap());
                std::fs::copy(samples.join(name), &target).unwrap();
                target
            })
            .collect();
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::bind_keys(cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(paths[0].clone()),
                        session_directory: Some(root.join(".state")),
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
        let view = log_view(&reader, visual);
        view.update(visual, |view, cx| view.add_files(paths[1..].to_vec(), cx));
        visual.run_until_parked();
        let total = view.read_with(visual, |view, _| view.total());
        let expected: Vec<(String, u64)> = view.read_with(visual, |view, _| {
            (0..view.visible_len())
                .map(|row| {
                    let (source, entry) = view.locate(view.entry_at(row).unwrap()).unwrap();
                    let line = view
                        .source(source)
                        .unwrap()
                        .index()
                        .get(entry)
                        .unwrap()
                        .line();
                    (view.source_name(source), line)
                })
                .collect()
        });
        assert_eq!(expected.len(), total);
        view.update_in(visual, |view, window, cx| {
            view.export_rows(ExportScope::Shown, ExportFormat::JsonLines, window, cx)
        });
        let out = root.join("both.jsonl");
        let target = out.clone();
        visual.simulate_new_path_selection(move |_| Some(target));
        visual.run_until_parked();
        let written: Vec<(String, u64)> = std::fs::read_to_string(&out)
            .unwrap()
            .lines()
            .map(|line| {
                let row: serde_json::Value = serde_json::from_str(line).unwrap();
                (
                    row["source"].as_str().expect("a source").to_owned(),
                    row["line"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(written, expected, "every row, in the order shown");
        let sources: std::collections::BTreeSet<_> =
            written.iter().map(|(s, _)| s.clone()).collect();
        assert_eq!(sources.len(), 2, "both files are named");
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
