//! HTTP Archive files (#2836): the requests of a `.har` capture as a sortable,
//! filterable list with the selected request's details below it, and the
//! existing JSON viewer as Source. Read-only.
//!
//! The file is indexed once on a worker (`okilum_core::har`); requests appear
//! while it is read. Details and bodies are read again from the request's
//! byte range when it is selected. Filtering and sorting run on workers. A
//! file switch drops this view, which cancels the index and any detail read.
//!
//! HAR files hold cookies, tokens and passwords: nothing here writes their
//! content to diagnostics, logs or toasts, links are never opened and bodies
//! are never rendered as markup or loaded from the network.
use super::*;
use okilum_core::har::{self, Entry, Outcome, Problem};
use reader_viewer_chrome::{ViewerAction, ViewerChrome, ViewerView};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) mod details;
pub(crate) mod model;
#[cfg(test)]
mod tests;

use details::{DetailRows, Row, Section};
use model::{Column, Entries, Query, Rows, Stats};

const ROW_HEIGHT: f32 = 26.;
const DETAIL_SHARE: f32 = 0.55;
const LIST_MIN_ROWS: f32 = 4.;
const DETAIL_MIN_HEIGHT: f32 = 64.;
const METHOD_WIDTH: f32 = 70.;
const STATUS_WIDTH: f32 = 60.;
const TYPE_WIDTH: f32 = 96.;
const SIZE_WIDTH: f32 = 84.;
const TIME_WIDTH: f32 = 76.;
const STARTED_WIDTH: f32 = 104.;
const NAME_MIN: f32 = 160.;
/// Pane widths at which optional columns appear (measured on the pane).
const TYPE_FROM: f32 = 600.;
const STARTED_FROM: f32 = 800.;

pub(crate) fn eligible(rel: &str) -> bool {
    Path::new(rel)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("har"))
}

enum Index {
    Reading { offset: u64 },
    Done(Outcome),
}

enum Detail {
    Loading,
    Ready(Arc<DetailRows>),
    Failed(SharedString),
}

pub(crate) struct HarView {
    root: PathBuf,
    rel: String,
    path: PathBuf,
    bytes: u64,
    reader: WeakEntity<Reader>,
    index: Index,
    entries: Entries,
    stats: Stats,
    filter: Entity<InputState>,
    sort: Option<(Column, bool)>,
    rows: Rows,
    /// The query the current `rows` answer, and the newest one asked.
    shown_query: Query,
    projection: u64,
    projecting: Option<(Arc<AtomicBool>, Task<()>)>,
    selected: Option<usize>,
    detail: Option<Detail>,
    detail_generation: u64,
    detail_cancel: Arc<AtomicBool>,
    _detail_task: Option<Task<()>>,
    collapsed: std::collections::HashSet<Section>,
    detail_visible: Vec<usize>,
    detail_list: ListState,
    scroll: UniformListScrollHandle,
    split: reader_split::Split,
    list_focus: FocusHandle,
    detail_focus: FocusHandle,
    /// The list pane's measured width; columns follow the pane, not the window.
    list_width: f32,
    source: Option<Entity<reader_code_file::CodePreview>>,
    cancel: Arc<AtomicBool>,
    /// The file revision the byte ranges belong to, from the index.
    revision: Option<har::Revision>,
    /// Batches of an older index (before a reindex) are ignored.
    index_generation: u64,
    /// Said above the list after the file changed and was read again.
    notice: Option<SharedString>,
    /// Detail reads that finished their parse and formatting.
    #[cfg(test)]
    finished_reads: Arc<std::sync::atomic::AtomicUsize>,
    _index: Task<()>,
    _filter: Subscription,
}

impl Drop for HarView {
    fn drop(&mut self) {
        // Workers stop at their next read; nothing they produce is applied.
        self.cancel.store(true, Ordering::Relaxed);
        self.detail_cancel.store(true, Ordering::Relaxed);
        if let Some((cancel, _)) = &self.projecting {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

enum Message {
    Batch(Vec<Entry>, u64, Option<har::Revision>),
    Done(Result<Outcome, har::Cancelled>),
}

impl HarView {
    pub(crate) fn new(
        root: PathBuf,
        rel: String,
        path: PathBuf,
        reader: WeakEntity<Reader>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Find in requests"));
        let subscription = cx.subscribe_in(
            &filter,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.refresh(cx);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    if this.selected.is_none() {
                        if let Some(first) = this.rows.get(0) {
                            this.select(first, cx);
                        }
                    }
                    this.list_focus.focus(window, cx);
                    cx.notify();
                }
                _ => {}
            },
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let index = Self::start_index(path.clone(), cancel.clone(), 0, cx);
        Self {
            root,
            rel,
            path,
            bytes,
            reader,
            index: Index::Reading { offset: 0 },
            entries: Entries::default(),
            stats: Stats::default(),
            filter,
            sort: None,
            rows: Rows::default(),
            shown_query: Query::default(),
            projection: 0,
            projecting: None,
            selected: None,
            detail: None,
            detail_generation: 0,
            detail_cancel: Arc::new(AtomicBool::new(false)),
            _detail_task: None,
            collapsed: Default::default(),
            detail_visible: Vec::new(),
            detail_list: ListState::new(0, ListAlignment::Top, px(400.)),
            scroll: UniformListScrollHandle::new(),
            split: reader_split::Split::new(
                "har-details",
                reader_split::Initial::Share(DETAIL_SHARE),
                DETAIL_MIN_HEIGHT,
                ROW_HEIGHT * LIST_MIN_ROWS,
            ),
            list_focus: cx.focus_handle(),
            detail_focus: cx.focus_handle(),
            list_width: 0.,
            source: None,
            cancel,
            revision: None,
            index_generation: 0,
            notice: None,
            #[cfg(test)]
            finished_reads: Default::default(),
            _index: index,
            _filter: subscription,
        }
    }

    fn start_index(
        path: PathBuf,
        cancel: Arc<AtomicBool>,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        // Unbounded: the index never waits for the UI (the requests it holds
        // are bounded by `har::MAX_ENTRIES` either way).
        let (send, receive) = async_channel::unbounded::<Message>();
        cx.background_executor()
            .spawn(async move {
                let result = har::index_file(&path, &cancel, &mut |batch| {
                    let _ = send.send_blocking(Message::Batch(
                        batch.entries,
                        batch.offset,
                        batch.revision,
                    ));
                });
                let _ = send.send_blocking(Message::Done(result));
            })
            .detach();
        cx.spawn(async move |this, cx| {
            while let Ok(first) = receive.recv().await {
                // Apply everything already delivered in one update.
                let mut messages = vec![first];
                while let Ok(next) = receive.try_recv() {
                    messages.push(next);
                }
                let applied = this.update(cx, |this, cx| {
                    if this.index_generation != generation {
                        return Err(());
                    }
                    for message in messages {
                        this.apply(message);
                    }
                    this.refresh(cx);
                    cx.notify();
                    Ok(())
                });
                if !matches!(applied, Ok(Ok(()))) {
                    break;
                }
            }
        })
    }

    fn apply(&mut self, message: Message) {
        match message {
            Message::Batch(entries, offset, revision) => {
                if let Some(revision) = revision {
                    self.revision = Some(revision);
                    self.bytes = revision.len;
                }
                for entry in &entries {
                    self.stats.add(entry);
                }
                self.entries.push(entries);
                if let Index::Reading { offset: read } = &mut self.index {
                    *read = offset;
                }
            }
            Message::Done(Ok(outcome)) => {
                if outcome.revision.is_some() {
                    self.revision = outcome.revision;
                }
                self.index = Index::Done(outcome);
            }
            // Only a dropped view cancels; nothing is left to update.
            Message::Done(Err(har::Cancelled)) => {}
        }
    }

    /// Reads the file again after it changed on disk: earlier byte ranges
    /// belong to the old file, so the list, selection and reads are dropped.
    fn reindex(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.index_generation = self.index_generation.wrapping_add(1);
        self.cancel_projection();
        self.selected = None;
        self.detail = None;
        self.detail_cancel.store(true, Ordering::Relaxed);
        self.detail_generation = self.detail_generation.wrapping_add(1);
        self.reset_detail_rows();
        self.entries = Entries::default();
        self.stats = Stats::default();
        self.rows = Rows::default();
        self.revision = None;
        self.index = Index::Reading { offset: 0 };
        self.notice = Some("The file changed on disk, so its requests were read again.".into());
        self._index = Self::start_index(
            self.path.clone(),
            self.cancel.clone(),
            self.index_generation,
            cx,
        );
        self.refresh(cx);
        cx.notify();
    }

    fn reading(&self) -> bool {
        matches!(self.index, Index::Reading { .. })
    }

    fn outcome(&self) -> Option<&Outcome> {
        match &self.index {
            Index::Done(outcome) => Some(outcome),
            Index::Reading { .. } => None,
        }
    }

    fn query(&self, cx: &App) -> Query {
        Query {
            text: self.filter.read(cx).value().to_string(),
            sort: self.sort,
        }
    }

    /// Recomputes the displayed rows for the current query. Source order with
    /// no filter is immediate; anything else runs on a worker, and a newer
    /// query or batch supersedes a running one.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        if query.identity() {
            self.cancel_projection();
            self.rows = Rows::All(self.entries.len());
            self.shown_query = query;
            return;
        }
        self.cancel_projection();
        self.projection = self.projection.wrapping_add(1);
        let generation = self.projection;
        let cancel = Arc::new(AtomicBool::new(false));
        let entries = self.entries.clone();
        let worker_cancel = cancel.clone();
        let worker_query = query.clone();
        let work = cx
            .background_executor()
            .spawn(async move { model::project(&entries, &worker_query, &worker_cancel) });
        let task = cx.spawn(async move |this, cx| {
            let Some(rows) = work.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.projection != generation {
                    return;
                }
                this.projecting = None;
                this.rows = Rows::Projected(Arc::new(rows));
                this.shown_query = query;
                cx.notify();
            });
        });
        self.projecting = Some((cancel, task));
    }

    fn cancel_projection(&mut self) {
        if let Some((cancel, _)) = self.projecting.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.projection = self.projection.wrapping_add(1);
    }

    fn sort_by(&mut self, column: Column, cx: &mut Context<Self>) {
        // Ascending, descending, then file order.
        self.sort = match self.sort {
            Some((current, true)) if current == column => Some((column, false)),
            Some((current, false)) if current == column => None,
            _ => Some((column, true)),
        };
        self.refresh(cx);
        cx.notify();
    }

    fn select(&mut self, entry: usize, cx: &mut Context<Self>) {
        if self.selected == Some(entry) {
            return;
        }
        let Some((span, hash)) = self.entries.get(entry).map(|e| (e.span.clone(), e.hash)) else {
            return;
        };
        self.selected = Some(entry);
        self.detail_cancel.store(true, Ordering::Relaxed);
        self.detail_cancel = Arc::new(AtomicBool::new(false));
        self.detail_generation = self.detail_generation.wrapping_add(1);
        let generation = self.detail_generation;
        self.detail = Some(Detail::Loading);
        self.reset_detail_rows();
        let (path, cancel, start, revision) = (
            self.path.clone(),
            self.detail_cancel.clone(),
            self.stats.first_ms,
            self.revision,
        );
        #[cfg(test)]
        let finished = self.finished_reads.clone();
        let work = cx.background_executor().spawn(async move {
            let result = har::details(&path, span, revision, Some(hash), &cancel)
                .and_then(|details| details::build(&path, &details, start, revision, &cancel));
            #[cfg(test)]
            if result.is_ok() {
                finished.fetch_add(1, Ordering::Relaxed);
            }
            result
        });
        self._detail_task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                // A newer selection or a closed pane owns the detail slot.
                if this.detail_generation != generation || this.selected != Some(entry) {
                    return;
                }
                this.detail = Some(match result {
                    Ok(rows) => Detail::Ready(Arc::new(rows)),
                    Err(har::DetailError::Changed) => return this.reindex(cx),
                    Err(error) => Detail::Failed(error.to_string().into()),
                });
                this.reset_detail_rows();
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn close_details(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.detail = None;
        self.detail_cancel.store(true, Ordering::Relaxed);
        self.detail_generation = self.detail_generation.wrapping_add(1);
        self.reset_detail_rows();
        cx.notify();
    }

    fn reset_detail_rows(&mut self) {
        self.detail_visible = match &self.detail {
            Some(Detail::Ready(rows)) => {
                let collapsed = &self.collapsed;
                rows.visible(&|section| collapsed.contains(&section))
            }
            _ => Vec::new(),
        };
        self.detail_list.reset(self.detail_visible.len());
    }

    fn toggle_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&section) {
            self.collapsed.insert(section);
        }
        let top = self.detail_list.logical_scroll_top();
        self.reset_detail_rows();
        self.detail_list.scroll_to(top);
        cx.notify();
    }

    /// Moves the selection by `delta` rows in display order.
    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.rows.len();
        if count == 0 {
            return;
        }
        let current = self.selected.and_then(|entry| self.rows.position(entry));
        let next = match current {
            Some(position) => position.saturating_add_signed(delta).min(count - 1),
            None if delta < 0 => count - 1,
            None => 0,
        };
        if let Some(entry) = self.rows.get(next) {
            self.select(entry, cx);
            self.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        }
    }

    fn focus_find(&self, window: &mut Window, cx: &mut App) {
        self.filter.update(cx, |input, cx| input.focus(window, cx));
    }

    /// The JSON viewer of this file, created once and kept for switching back.
    fn source_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<reader_code_file::CodePreview> {
        if let Some(source) = &self.source {
            return source.clone();
        }
        let (root, rel, path) = (self.root.clone(), self.rel.clone(), self.path.clone());
        let source = cx.new(|cx| {
            reader_code_file::CodePreview::new(root, rel, path, window, cx).without_header()
        });
        self.source = Some(source.clone());
        source
    }

    fn summary(&self) -> String {
        let total = self.stats.requests;
        let mut parts = Vec::new();
        let shown = self.rows.len();
        let filtered = !self.shown_query.text.trim().is_empty();
        parts.push(if filtered {
            format!(
                "{} of {} requests",
                model::count(shown),
                model::count(total)
            )
        } else if total == 1 {
            "1 request".to_owned()
        } else {
            format!("{} requests", model::count(total))
        });
        parts.push(format!(
            "{} transferred",
            model::bytes(self.stats.transferred)
        ));
        if let Some(span) = self.stats.span_ms() {
            parts.push(model::millis(span));
        }
        parts.push(match self.stats.errors {
            0 => "no errors".to_owned(),
            1 => "1 error".to_owned(),
            n => format!("{} errors", model::count(n)),
        });
        parts.join(" · ")
    }

    fn status_line(&self) -> reader_status_line::StatusLine {
        let mut status = reader_status_line::StatusLine::default();
        match &self.index {
            Index::Reading { offset } => {
                let percent = if self.bytes > 0 {
                    (*offset as f64 / self.bytes as f64 * 100.).min(99.) as u32
                } else {
                    0
                };
                status.label = format!("Reading… {percent}%").into();
                status.text = self.summary().into();
            }
            Index::Done(outcome) => {
                if outcome.problem.is_none() || self.stats.requests > 0 {
                    status.text = self.summary().into();
                }
                if outcome.limited {
                    status.tone = reader_status_line::Tone::Warning;
                    status.label = format!(
                        "Only the first {} requests are listed.",
                        model::count(har::MAX_ENTRIES)
                    )
                    .into();
                }
                if let Some(Problem::Damaged(error)) = &outcome.problem {
                    if self.stats.requests > 0 {
                        status.tone = reader_status_line::Tone::Warning;
                        status.icon = Some(Icon::new(IconName::TriangleAlert));
                        status.label =
                            format!("Damaged at byte {}.", model::count(error.offset as usize))
                                .into();
                    }
                }
            }
        }
        status
    }

    fn find_count(&self) -> Option<SharedString> {
        (!self.shown_query.text.trim().is_empty()).then(|| {
            format!(
                "{} of {}",
                model::count(self.rows.len()),
                model::count(self.entries.len())
            )
            .into()
        })
    }

    /// A full-pane message when there is no list to show.
    fn message(&self) -> Option<String> {
        let outcome = self.outcome()?;
        if self.stats.requests > 0 {
            return None;
        }
        Some(match &outcome.problem {
            None => "This HAR file has no requests.".to_owned(),
            Some(Problem::NotHar(reason)) => format!("This is not a HAR file. {reason}."),
            Some(Problem::Damaged(error)) => format!(
                "This HAR file is damaged and no complete request could be read. {}.",
                error
            ),
            Some(Problem::Unreadable(reason)) => format!("This file could not be read ({reason})."),
            Some(Problem::TooLarge(bytes)) => format!(
                "This file is {}. Requests are listed for HAR files up to {}; Source opens files up to 64 MB. Open it with another app.",
                model::bytes(*bytes as i64),
                model::bytes(har::MAX_FILE_BYTES as i64)
            ),
        })
    }

    /// A banner above a partial list.
    fn banner(&self) -> Option<String> {
        let damage = self.damage();
        match (&self.notice, damage) {
            (Some(notice), Some(damage)) => Some(format!("{notice} {damage}")),
            (Some(notice), None) => Some(notice.to_string()),
            (None, damage) => damage,
        }
    }

    fn damage(&self) -> Option<String> {
        match self.outcome()?.problem.as_ref()? {
            Problem::Damaged(error) if self.stats.requests > 0 => Some(format!(
                "The file is damaged ({error}). The {} requests before that point are listed; Source shows the file as written.",
                model::count(self.stats.requests)
            )),
            Problem::Unreadable(reason) if self.stats.requests > 0 => Some(format!(
                "Reading stopped ({reason}). The {} requests read so far are listed.",
                model::count(self.stats.requests)
            )),
            _ => None,
        }
    }

    fn columns(&self) -> Vec<Column> {
        let width = self.list_width;
        Column::ALL
            .into_iter()
            .filter(|column| match column {
                // Before the first measurement every column is laid out;
                // the measured pane then decides.
                Column::Type => width == 0. || width >= TYPE_FROM,
                Column::Started => width == 0. || width >= STARTED_FROM,
                _ => true,
            })
            .collect()
    }
}

fn column_width(column: Column) -> Option<f32> {
    match column {
        Column::Method => Some(METHOD_WIDTH),
        Column::Status => Some(STATUS_WIDTH),
        Column::Name => None,
        Column::Type => Some(TYPE_WIDTH),
        Column::Size => Some(SIZE_WIDTH),
        Column::Time => Some(TIME_WIDTH),
        Column::Started => Some(STARTED_WIDTH),
    }
}

fn cell(column: Column) -> Div {
    let cell = div().px_2().truncate();
    match column_width(column) {
        Some(width) => cell.flex_none().w(px(width)),
        None => cell.flex_1().min_w(px(NAME_MIN)),
    }
    .when(column.numeric() && column != Column::Status, |cell| {
        cell.text_align(TextAlign::Right)
    })
}

/// Requests view in list mode; `source` adds it to the JSON viewer's views.
fn requests_view(viewer: &Entity<HarView>, active: bool, cx: &App) -> ViewerView {
    let reader = viewer.read(cx).reader.clone();
    let mut action = ViewerAction::new(
        "har-view-requests",
        Icon::default().path("icons/activity.svg"),
        "Requests",
        Rc::new(move |window, cx| {
            let _ = reader.update(cx, |reader, cx| reader.show_har_source(false, window, cx));
        }),
    );
    action.tooltip = "Requests: the capture as a list of requests".into();
    ViewerView { action, active }
}

fn source_view(viewer: &Entity<HarView>, active: bool, cx: &App) -> ViewerView {
    let reader = viewer.read(cx).reader.clone();
    let mut action = ViewerAction::new(
        "har-view-source",
        Icon::default().path("icons/code-xml.svg"),
        "Source",
        Rc::new(move |window, cx| {
            let _ = reader.update(cx, |reader, cx| reader.show_har_source(true, window, cx));
        }),
    );
    action.tooltip = "Source: the file as JSON, as written".into();
    ViewerView { action, active }
}

/// The header description in list mode.
pub(crate) fn chrome(viewer: &Entity<HarView>, cx: &App) -> ViewerChrome {
    let state = viewer.read(cx);
    let focus = viewer.clone();
    let close = viewer.clone();
    let find = reader_find_field::Target {
        input: state.filter.clone(),
        scope: "Find in requests".into(),
        count: state.find_count(),
        match_case: None,
        step: None,
        steps_visible: false,
        large_file: false,
        focus_override: Some(Rc::new(move |window, cx| {
            focus.update(cx, |this, cx| this.focus_find(window, cx))
        })),
        close: Rc::new(move |window, cx| {
            close.update(cx, |this, cx| {
                this.filter
                    .update(cx, |filter, cx| filter.set_value("", window, cx));
                this.refresh(cx);
                this.list_focus.focus(window, cx);
                cx.notify();
            })
        }),
    };
    ViewerChrome {
        views: vec![
            requests_view(viewer, true, cx),
            source_view(viewer, false, cx),
        ],
        find: Some(find),
        status: Some(state.status_line()),
        ..Default::default()
    }
}

/// Adds Requests to the JSON viewer's views while Source is shown.
pub(crate) fn with_requests_view(
    mut chrome: ViewerChrome,
    viewer: &Entity<HarView>,
    cx: &App,
) -> ViewerChrome {
    if chrome.views.is_empty() {
        chrome.views.push(source_view(viewer, true, cx));
    }
    chrome.views.insert(0, requests_view(viewer, false, cx));
    chrome
}

impl Reader {
    /// Switches the open HAR file between its request list and the JSON
    /// viewer. The JSON viewer is the ordinary code preview, so Find, Copy,
    /// folding and its own views work as for any JSON file.
    pub(crate) fn show_har_source(
        &mut self,
        source: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = self.file_preview.as_mut() else {
            return;
        };
        let Some(har) = preview.har.clone() else {
            return;
        };
        if source == preview.code.is_some() {
            return;
        }
        preview.code = if source {
            Some(har.update(cx, |har, cx| har.source_view(window, cx)))
        } else {
            None
        };
        self.focus_handle.focus(window, cx);
        cx.notify();
    }
}

impl HarView {
    fn header_cell(&self, column: Column, cx: &mut Context<Self>) -> AnyElement {
        let arrow = match self.sort {
            Some((c, true)) if c == column => " ↑",
            Some((c, false)) if c == column => " ↓",
            _ => "",
        };
        cell(column)
            .id(SharedString::from(format!("har-sort-{}", column.label())))
            .debug_selector(move || format!("har-sort-{}", column.label()))
            .cursor_pointer()
            .child(format!("{}{arrow}", column.label()))
            .on_click(cx.listener(move |this, _, _, cx| this.sort_by(column, cx)))
            .into_any_element()
    }

    fn row(&self, position: usize, columns: &[Column], cx: &mut Context<Self>) -> AnyElement {
        let Some((index, entry)) = self
            .rows
            .get(position)
            .and_then(|index| Some((index, self.entries.get(index)?)))
        else {
            return div().h(px(ROW_HEIGHT)).into_any_element();
        };
        let palette = brand::palette(cx);
        let hover = brand::reader_palette(cx).hover;
        let failed = entry.failed();
        let selected = self.selected == Some(index);
        let mut row = h_flex()
            .id(("har-row", index))
            .debug_selector(move || format!("har-row-{index}"))
            .h(px(ROW_HEIGHT))
            .w_full()
            .items_center()
            .border_b_1()
            .border_color(palette.border_subtle)
            .cursor_pointer()
            .when(selected, |row| row.bg(palette.selected))
            .when(!selected, |row| row.hover(move |row| row.bg(hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.select(index, cx)));
        for &column in columns {
            let content: AnyElement = match column {
                Column::Method => cell(column)
                    .child(entry.method.to_string())
                    .into_any_element(),
                Column::Status => cell(column)
                    .when(failed, |c| c.text_color(palette.danger))
                    .child(match entry.status {
                        Some(0) | None if !entry.error.is_empty() => "failed".to_owned(),
                        Some(status) => status.to_string(),
                        None => "—".to_owned(),
                    })
                    .into_any_element(),
                Column::Name => {
                    let host = entry.host();
                    cell(column)
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap_1()
                                .when(!host.is_empty(), |name| {
                                    name.child(
                                        div()
                                            .flex_shrink(1.)
                                            .max_w(relative(0.45))
                                            .truncate()
                                            .text_color(palette.text_muted)
                                            .child(host.to_owned()),
                                    )
                                })
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .when(failed, |c| c.text_color(palette.danger))
                                        .child(entry.path().to_owned()),
                                ),
                        )
                        .into_any_element()
                }
                Column::Type => cell(column)
                    .text_color(palette.text_muted)
                    .child(entry.type_label().to_owned())
                    .into_any_element(),
                Column::Size => cell(column)
                    .text_color(palette.text_muted)
                    .child(
                        model::shown_size(entry)
                            .map(model::bytes)
                            .unwrap_or_default(),
                    )
                    .into_any_element(),
                Column::Time => cell(column)
                    .text_color(palette.text_muted)
                    .child(entry.time.map(model::millis).unwrap_or_default())
                    .into_any_element(),
                Column::Started => cell(column)
                    .text_color(palette.text_muted)
                    .child(model::clock(&entry.started).to_owned())
                    .into_any_element(),
            };
            row = row.child(content);
        }
        row.into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = brand::palette(cx);
        let columns = self.columns();
        let mut header = h_flex()
            .debug_selector(|| "har-list-header".into())
            .h(px(ROW_HEIGHT))
            .w_full()
            .flex_none()
            .items_center()
            .bg(palette.surface)
            .font_weight(FontWeight::SEMIBOLD)
            .border_b_1()
            .border_color(palette.border_subtle);
        for &column in &columns {
            header = header.child(self.header_cell(column, cx));
        }
        let count = self.rows.len();
        let target = cx.entity().downgrade();
        let measured = self.list_width;
        let empty = (count == 0 && !self.reading() && self.stats.requests > 0).then(|| {
            div()
                .px_3()
                .py_2()
                .text_color(palette.text_muted)
                .child("No request matches.")
        });
        v_flex()
            .id("har-list")
            .debug_selector(|| "har-list".into())
            .key_context("HarList")
            .track_focus(&self.list_focus)
            // Esc is the Reader's Dismiss binding; open details take it first.
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| {
                if this.selected.is_some() {
                    this.close_details(cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "up" => this.step(-1, cx),
                    "down" => this.step(1, cx),
                    "pageup" => this.step(-20, cx),
                    "pagedown" => this.step(20, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .relative()
            .flex_1()
            .min_h_0()
            .mx_4()
            .border_1()
            .border_color(palette.border_subtle)
            .child(
                // The pane's own width decides the columns (never the window's).
                canvas(
                    move |bounds, _, cx| {
                        let width = bounds.size.width.as_f32();
                        if width > 0. && (width - measured).abs() > 0.5 {
                            cx.defer(move |cx| {
                                let _ = target.update(cx, |this, cx| {
                                    this.list_width = width;
                                    cx.notify();
                                });
                            });
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(header)
            .children(empty)
            .child(
                div().flex_1().min_h_0().w_full().child(
                    uniform_list(
                        "har-rows",
                        count,
                        cx.processor(move |this, range: Range<usize>, _, cx| {
                            let columns = this.columns();
                            range
                                .map(|position| this.row(position, &columns, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .size_full(),
                ),
            )
            .into_any_element()
    }

    fn render_detail_row(&self, visible: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(Detail::Ready(rows)) = &self.detail else {
            return div().into_any_element();
        };
        let Some(row) = self
            .detail_visible
            .get(visible)
            .and_then(|&index| rows.rows.get(index))
        else {
            return div().into_any_element();
        };
        let palette = brand::palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        match row {
            Row::Section { section, count } => {
                let section = *section;
                let collapsed = self.collapsed.contains(&section);
                h_flex()
                    .id(SharedString::from(format!(
                        "har-section-{}",
                        section.title()
                    )))
                    .debug_selector(move || format!("har-section-{}", section.title()))
                    .w_full()
                    .h(px(28.))
                    .mt_1()
                    .px_4()
                    .gap_1()
                    .items_center()
                    .cursor_pointer()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if collapsed { "▸" } else { "▾" })
                    .child(section.title())
                    .children(count.map(|count| {
                        div()
                            .text_color(palette.text_muted)
                            .font_weight(FontWeight::NORMAL)
                            .child(format!("({count})"))
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(section, cx)))
                    .into_any_element()
            }
            Row::Field { name, value } => h_flex()
                .w_full()
                .px_4()
                .py(px(2.))
                .gap_3()
                .items_start()
                .text_sm()
                .child(
                    div()
                        .flex_none()
                        .w(relative(0.3))
                        .max_w(px(240.))
                        .text_color(palette.text_muted)
                        .overflow_hidden()
                        .child(name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(mono)
                        .child(value.clone()),
                )
                .into_any_element(),
            Row::Timing {
                name,
                ms,
                start,
                width,
            } => h_flex()
                .w_full()
                .h(px(22.))
                .px_4()
                .gap_3()
                .items_center()
                .text_sm()
                .child(
                    div()
                        .flex_none()
                        .w(px(140.))
                        .text_color(palette.text_muted)
                        .child(name.clone()),
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .h(px(8.))
                        .rounded(px(2.))
                        .bg(palette.border_subtle)
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .h_full()
                                .left(relative(*start))
                                .w(relative(width.max(0.003)))
                                .rounded(px(2.))
                                .bg(palette.accent),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(84.))
                        .text_align(TextAlign::Right)
                        .child(model::millis(*ms)),
                )
                .into_any_element(),
            Row::Text(text) => div()
                .w_full()
                .px_4()
                .font_family(mono)
                .text_xs()
                .child(text.clone())
                .into_any_element(),
            Row::Note(text) => div()
                .w_full()
                .px_4()
                .py_1()
                .text_sm()
                .text_color(palette.text_muted)
                .child(text.clone())
                .into_any_element(),
        }
    }

    fn render_details(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let detail = self.detail.as_ref()?;
        let entry = self.entries.get(self.selected?)?;
        let palette = brand::palette(cx);
        let title = format!("{} {}", entry.method, entry.path());
        let bar = h_flex()
            .debug_selector(|| "har-detail-bar".into())
            .flex_none()
            .h(px(32.))
            .px_4()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(palette.border_subtle)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .child(
                reader_icon_button(
                    "har-detail-close",
                    Icon::new(IconName::Close),
                    "Close details (Esc)",
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| this.close_details(cx))),
            );
        let body = match detail {
            Detail::Loading => div()
                .px_4()
                .py_2()
                .text_color(palette.text_muted)
                .child("Reading request…")
                .into_any_element(),
            Detail::Failed(message) => div()
                .px_4()
                .py_2()
                .text_color(palette.text_muted)
                .child(message.clone())
                .into_any_element(),
            Detail::Ready(_) => list(
                self.detail_list.clone(),
                cx.processor(|this, visible: usize, _, cx| this.render_detail_row(visible, cx)),
            )
            .size_full()
            .into_any_element(),
        };
        Some(
            v_flex()
                .id("har-detail")
                .debug_selector(|| "har-detail".into())
                .track_focus(&self.detail_focus)
                .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.close_details(cx)))
                .size_full()
                .min_h_0()
                .child(bar)
                .child(div().flex_1().min_h_0().pb_2().child(body))
                .into_any_element(),
        )
    }

    fn show_source_button(&self) -> AnyElement {
        let reader = self.reader.clone();
        Button::new("har-show-source")
            .debug_selector(|| "har-show-source".into())
            .label("Show source")
            .icon(Icon::default().path("icons/code-xml.svg"))
            .small()
            .on_click(move |_, window, cx| {
                let _ = reader.update(cx, |reader, cx| reader.show_har_source(true, window, cx));
            })
            .into_any_element()
    }
}

impl Render for HarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = brand::palette(cx);
        let view = v_flex()
            .debug_selector(|| "har-view".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .gap_2();
        if let Some(message) = self.message() {
            return view
                .child(
                    v_flex()
                        .debug_selector(|| "har-message".into())
                        .px_6()
                        .py_4()
                        .gap_3()
                        .items_start()
                        .child(div().text_color(palette.text_muted).child(message))
                        .child(self.show_source_button()),
                )
                .into_any_element();
        }
        if self.reading() && self.stats.requests == 0 {
            return view
                .child(
                    div()
                        .px_6()
                        .text_color(palette.text_muted)
                        .child("Reading requests…"),
                )
                .into_any_element();
        }
        let banner = self.banner().map(|banner| {
            h_flex()
                .debug_selector(|| "har-banner".into())
                .mx_4()
                .px_3()
                .py_2()
                .gap_3()
                .items_center()
                .rounded(px(6.))
                .bg(brand::reader_palette(cx).warning_tint)
                .child(div().flex_1().min_w_0().child(banner))
                .child(self.show_source_button())
        });
        let list = v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .children(banner)
            .child(self.render_list(cx));
        let Some(details) = self.render_details(cx) else {
            return view.child(list).into_any_element();
        };
        view.child(reader_split::vertical(
            &self.split,
            |view: &mut Self| &mut view.split,
            list,
            details,
            cx,
        ))
        .into_any_element()
    }
}
