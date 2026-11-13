//! File structure in the existing right panel; no Reader/sidebar state or hooks.
use super::super::*;
use gpui_component::input::{EditorState, InputEvent};
use reader_code_file::contents::{ContentsSource, Whereabouts};
use reader_code_file::document::Document;
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};
#[path = "reader_file_outline/parse.rs"]
mod parse;
#[path = "reader_file_outline/tree.rs"]
pub(crate) mod tree;

/// «Reading structure…» appears only when reading takes longer than this.
const READING_DELAY: Duration = Duration::from_millis(300);

/// Contents from the viewer's whole-file tree (#2814): JSON in Read.
/// Listed group rows by parent: `(first_index, last_index, row)` sorted by
/// `first_index`, inner groups after the outer group they share it with.
type GroupRows = HashMap<usize, Vec<(usize, usize, usize)>>;

#[derive(Default)]
struct Snapshot {
    active: bool,
    file: String,
    /// The document the shown rows index; replaced only together with them.
    doc: Option<Arc<Document>>,
    /// A newer document whose rows are still being computed.
    pending: Option<Arc<Document>>,
    state: SnapshotState,
    rows: Arc<Vec<tree::Row>>,
    /// Row position of each listed node, computed with the rows.
    positions: Arc<HashMap<usize, usize>>,
    /// Listed group rows by parent, computed with the rows.
    groups: Arc<GroupRows>,
    /// The marked row: the node where you are, or its nearest listed ancestor.
    current: Option<usize>,
    /// The last sample of the viewer; a new file or view starts a new one.
    seen: Option<Whereabouts>,
    /// The caret (or Tree selection) leads until the view is scrolled.
    follow_caret: bool,
    /// A node a caret move named: its path opens once and its row scrolls
    /// into view when listed. `true` once the path was opened.
    reveal: Option<(usize, bool)>,
    generation: u64,
    task: Option<Task<()>>,
    reading: Option<Task<()>>,
    /// Open rows per file, by key path: survive rebuilds, view switches and
    /// closing the panel.
    expansions: HashMap<String, tree::Expansion>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum SnapshotState {
    #[default]
    Absent,
    Loading {
        shown: bool,
    },
    Invalid(&'static str),
    Ready,
}

pub(crate) struct View {
    code: Option<Entity<reader_code_file::CodePreview>>,
    input: Option<Entity<EditorState>>,
    language: Option<&'static str>,
    code_observer: Option<Subscription>,
    input_observers: Vec<Subscription>,
    task: Option<Task<()>>,
    generation: u64,
    data: Arc<parse::Outline>,
    expanded: BTreeSet<usize>,
    visible: Vec<usize>,
    current: Option<usize>,
    sample_pending: bool,
    list: ListState,
    snapshot: Snapshot,
}

impl View {
    pub(crate) fn new() -> Self {
        Self {
            code: None,
            input: None,
            language: None,
            code_observer: None,
            input_observers: Vec::new(),
            task: None,
            generation: 0,
            data: Arc::default(),
            expanded: BTreeSet::new(),
            visible: Vec::new(),
            current: None,
            sample_pending: false,
            list: ListState::new(0, ListAlignment::Top, px(200.)),
            snapshot: Snapshot::default(),
        }
    }

    /// Called only when the right panel is actually shown. Loading, large and
    /// unsupported files cost no parse; no text is copied on scroll notifications.
    pub(crate) fn bind(view: &Entity<Self>, reader: &Reader, cx: &mut App) {
        let code = reader
            .file_preview
            .as_ref()
            .and_then(|file| file.code.clone());
        // Read mode of a JSON file: rows come from the viewer's own tree.
        if let Some(code) = code.clone().filter(|_| reader.source_input().is_none()) {
            let source = code.read(cx).contents_source();
            if !matches!(source, ContentsSource::Absent) || Self::snapshot_file(reader) {
                let file = reader.selected_file().to_owned();
                view.update(cx, |this, cx| this.bind_snapshot(code, file, source, cx));
                return;
            }
        }
        view.update(cx, |this, cx| this.leave_snapshot(cx));
        let language = reader
            .file_preview
            .as_ref()
            .and_then(|file| reader_code_file::language(&file.rel))
            .filter(|lang| parse::supported(lang));
        let input = language.and_then(|_| {
            reader
                .source_input()
                .or_else(|| code.as_ref().and_then(|code| code.read(cx).outline_input()))
        });
        view.update(cx, |this, cx| {
            if this.code != code || this.language != language {
                this.code = code.clone();
                this.language = language;
                this.code_observer = code.as_ref().filter(|_| language.is_some()).map(|code| {
                    cx.observe(code, |this, code, cx| {
                        let preview_input = code.read(cx).input();
                        let input = code.read(cx).outline_input();
                        // A visible writable editor owns its own text and observer.
                        // Re-attach only when the input itself changes; its text
                        // changes arrive through the subscription below.
                        if this.input.is_none() || this.input == preview_input {
                            this.attach(input, false, cx);
                        }
                    })
                });
                this.attach(input, true, cx);
            } else if this.input != input {
                this.attach(input, true, cx);
            }
        });
    }

    fn attach(&mut self, input: Option<Entity<EditorState>>, force: bool, cx: &mut Context<Self>) {
        if !force && self.input == input {
            return;
        }
        self.input_observers.clear();
        self.input = input.clone();
        if let Some(input) = input {
            self.input_observers
                .push(cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.reparse(cx);
                    }
                }));
        }
        self.reparse(cx);
    }

    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        // Open rows are kept per file; only the binding is released.
        self.leave_snapshot(cx);
        if self.code.is_none() && self.input.is_none() {
            return;
        }
        self.code = None;
        self.language = None;
        self.code_observer = None;
        self.attach(None, true, cx);
    }

    fn reparse(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.task = None;
        self.data = Arc::default();
        self.visible.clear();
        self.expanded.clear();
        self.current = None;
        self.list.reset(0);
        cx.notify();
        let Some((input, language)) = self.input.as_ref().zip(self.language) else {
            return;
        };
        // Check the borrowed rope before materializing it; large inputs never copy.
        if input.read(cx).text().len() > parse::MAX_BYTES {
            return;
        }
        let source = input.read(cx).text().clone();
        let generation = self.generation;
        self.task = Some(cx.spawn(async move |this, cx| {
            // Coalesce edits and Pretty/Raw transitions before entering the parser.
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let data = cx
                .background_executor()
                .spawn(async move { parse::parse(language, &source.to_string()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.data = Arc::new(data);
                this.refresh_rows(cx);
            });
        }));
    }

    fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        let top = self.list.logical_scroll_top();
        let anchor = self.visible.get(top.item_ix).copied();
        self.visible = self.data.visible(&self.expanded);
        self.list
            .reset_with_uniform_height(self.visible.len(), px(OUTLINE_ROW_HEIGHT));
        if let Some(row) = anchor.and_then(|ix| self.visible.binary_search(&ix).ok()) {
            self.list.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: top.offset_in_item,
            });
        }
        self.sync_current(cx);
        cx.notify();
    }

    /// Paint this outside the cached panel. Editor notifications precede layout,
    /// and the editor publishes its viewport during paint. A deferred sample
    /// sees that completed paint regardless of sibling order, without requesting
    /// another frame unless the highlighted row actually changed.
    pub(crate) fn track_viewport(
        outline: impl Fn(&App) -> Option<Entity<Self>> + 'static,
    ) -> impl IntoElement {
        canvas(
            |_, _, _| (),
            move |_, _, _, cx| {
                let Some(view) = outline(cx) else { return };
                view.update(cx, |this, cx| {
                    let tracks = this.input.is_some()
                        || this.snapshot.active && this.snapshot.state == SnapshotState::Ready;
                    if !tracks || this.sample_pending {
                        return;
                    }
                    this.sample_pending = true;
                    let view = cx.entity().downgrade();
                    cx.defer(move |cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.sample_pending = false;
                            this.sync_current(cx);
                        });
                    });
                });
            },
        )
        .absolute()
        .size_full()
    }

    fn sync_current(&mut self, cx: &mut Context<Self>) {
        if self.snapshot.active {
            let current = self.snapshot_current(cx);
            if self.snapshot.current != current {
                self.snapshot.current = current;
                cx.notify();
            }
            return;
        }
        let current = self
            .input
            .as_ref()
            .and_then(|input| input.read(cx).top_visible_line())
            .and_then(|line| self.data.current(line, &self.visible));
        if self.current != current {
            self.current = current;
            cx.notify();
        }
    }

    pub(super) fn has_entries(&self) -> bool {
        if self.snapshot.active {
            return match self.snapshot.state {
                SnapshotState::Absent => false,
                SnapshotState::Loading { shown } => shown,
                SnapshotState::Invalid(_) | SnapshotState::Ready => true,
            };
        }
        !self.visible.is_empty()
    }

    fn snapshot_file(reader: &Reader) -> bool {
        let rel = reader.selected_file();
        rel.to_ascii_lowercase().ends_with(".json")
            || matches!(
                reader_code_file::language(rel),
                Some("yaml" | "toml" | "xml")
            )
    }

    fn bind_snapshot(
        &mut self,
        code: Entity<reader_code_file::CodePreview>,
        file: String,
        source: ContentsSource,
        cx: &mut Context<Self>,
    ) {
        if !self.snapshot.active || self.code.as_ref() != Some(&code) {
            // Leave the private-parser path and watch the viewer cheaply: a
            // notify only compares the snapshot it already has.
            self.input_observers.clear();
            self.input = None;
            self.task = None;
            self.language = None;
            self.code = Some(code.clone());
            self.code_observer = Some(cx.observe(&code, |this, code, cx| {
                let source = code.read(cx).contents_source();
                let file = this.snapshot.file.clone();
                this.apply_source(file, source, cx);
            }));
            self.snapshot.active = true;
        }
        self.apply_source(file, source, cx);
    }

    fn leave_snapshot(&mut self, cx: &mut Context<Self>) {
        if !self.snapshot.active {
            return;
        }
        let expansions = std::mem::take(&mut self.snapshot.expansions);
        self.snapshot = Snapshot {
            expansions,
            ..Default::default()
        };
        self.code = None;
        self.code_observer = None;
        self.list.reset(0);
        cx.notify();
    }

    fn apply_source(&mut self, file: String, source: ContentsSource, cx: &mut Context<Self>) {
        let other_file = self.snapshot.file != file;
        if other_file {
            // A new file starts from nothing: no rows, no pending rebuild and
            // no «Reading structure…» timer of the previous file.
            self.snapshot.file = file;
            self.snapshot.doc = None;
            self.snapshot.pending = None;
            self.snapshot.rows = Arc::default();
            self.snapshot.positions = Arc::default();
            self.snapshot.groups = Arc::default();
            self.snapshot.current = None;
            self.snapshot.seen = None;
            self.snapshot.follow_caret = false;
            self.snapshot.reveal = None;
            self.snapshot.task = None;
            self.snapshot.reading = None;
            self.snapshot.state = SnapshotState::Absent;
            self.list.reset(0);
        }
        let before = self.snapshot.state;
        match source {
            ContentsSource::Ready(doc) => {
                self.snapshot.reading = None;
                self.snapshot.state = SnapshotState::Ready;
                let newest = self
                    .snapshot
                    .pending
                    .as_ref()
                    .or(self.snapshot.doc.as_ref());
                if newest.is_none_or(|old| !Arc::ptr_eq(old, &doc)) {
                    // The shown rows keep their document until the new rows land.
                    self.snapshot.pending = Some(doc);
                    self.rebuild_rows(cx);
                }
            }
            ContentsSource::Loading => {
                // Keep the last rows of this file while a newer parse is pending.
                if !matches!(self.snapshot.state, SnapshotState::Loading { .. })
                    && self.snapshot.rows.is_empty()
                {
                    self.snapshot.state = SnapshotState::Loading { shown: false };
                    self.snapshot.reading = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(READING_DELAY).await;
                        let _ = this.update(cx, |this, cx| {
                            if let SnapshotState::Loading { shown } = &mut this.snapshot.state {
                                *shown = true;
                                cx.notify();
                            }
                        });
                    }));
                }
            }
            ContentsSource::Invalid(reason) => {
                self.snapshot.reading = None;
                self.snapshot.state = SnapshotState::Invalid(reason);
                self.snapshot.doc = None;
                self.snapshot.pending = None;
                self.snapshot.task = None;
                self.snapshot.rows = Arc::default();
                self.list.reset(0);
            }
            ContentsSource::Absent => {
                self.snapshot.reading = None;
                self.snapshot.state = SnapshotState::Absent;
                self.snapshot.doc = None;
                self.snapshot.pending = None;
                self.snapshot.task = None;
                self.snapshot.rows = Arc::default();
                self.list.reset(0);
            }
        }
        if other_file || before != self.snapshot.state {
            cx.notify();
        }
    }

    /// The marked row (#2814 2s-1). After a caret move, a Find hit or a Tree
    /// selection it is that node: its path opens and its row scrolls into
    /// view. After plain scrolling it is the node at the top of the view, and
    /// nothing opens. A node not listed marks its nearest listed ancestor or
    /// the closed group that holds it.
    fn snapshot_current(&mut self, cx: &mut Context<Self>) -> Option<usize> {
        let code = self.code.clone()?;
        let doc = self.snapshot.doc.clone()?;
        let raw = code.read(cx).document_raw_for_contents()?;
        // The viewer holds a newer parse whose rows are still being built:
        // the mark waits for them (the swap syncs it) instead of remapping
        // every viewport sample through both documents.
        if !Arc::ptr_eq(&raw, &doc) {
            return self.snapshot.current;
        }
        let now = code.read(cx).contents_whereabouts(cx)?;
        match self.snapshot.seen.replace(now) {
            // A view switch is not a caret move, nor a new page whose caret
            // is still where loading the page put it (the position scrubber):
            // the top of the view leads. A Find hit or a Contents click on
            // another page places the caret inside it.
            Some(before)
                if before.view != now.view || (before.page != now.page && now.caret == Some(0)) =>
            {
                self.snapshot.follow_caret = false;
                self.snapshot.reveal = None;
            }
            // Selection wins over scroll when both changed in one sample.
            Some(before) if before.caret != now.caret && now.caret_node.is_some() => {
                self.snapshot.follow_caret = true;
                self.snapshot.reveal = now.caret_node.map(|node| (node, false));
            }
            Some(before) if before.top != now.top => {
                self.snapshot.follow_caret = false;
                self.snapshot.reveal = None;
            }
            _ => {}
        }
        let node = self
            .snapshot
            .follow_caret
            .then_some(now.caret_node)
            .flatten()
            .or(now.top_node)?;
        if let Some((target, opened)) = self.snapshot.reveal {
            if target != node {
                self.snapshot.reveal = None;
            } else if let Some(&row) = self.snapshot.positions.get(&node) {
                self.snapshot.reveal = None;
                self.list.scroll_to_reveal_item(row);
                return Some(row);
            } else if !opened {
                // Rows with the path open arrive from the worker; the swap
                // syncs again and scrolls the row into view.
                self.snapshot.reveal = Some((node, true));
                tree::open_path(&doc, node, self.expansion());
                self.rebuild_rows(cx);
            }
        }
        let mut at = Some(node);
        while let Some(index) = at {
            if let Some(&row) = self.snapshot.positions.get(&index) {
                return Some(row);
            }
            let parent = doc.parent(index);
            if let Some(row) = parent.and_then(|parent| self.group_row(parent, index)) {
                return Some(row);
            }
            at = parent;
        }
        None
    }

    /// The innermost listed group of `parent` holding the child `index`.
    /// Listed groups of one parent tile its children, so the last group
    /// starting at or before `index` is the only candidate.
    fn group_row(&self, parent: usize, index: usize) -> Option<usize> {
        let groups = self.snapshot.groups.get(&parent)?;
        let at = groups.partition_point(|&(first, _, _)| first <= index);
        let &(_, last, row) = groups.get(at.checked_sub(1)?)?;
        (index <= last).then_some(row)
    }

    fn expansion(&mut self) -> &mut tree::Expansion {
        let file = self.snapshot.file.clone();
        self.snapshot.expansions.entry(file).or_default()
    }

    /// Visible rows on a worker; the list keeps showing the previous rows
    /// until the new ones arrive.
    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = self
            .snapshot
            .pending
            .clone()
            .or_else(|| self.snapshot.doc.clone())
        else {
            return;
        };
        let used = doc.clone();
        let expansion = self.expansion().clone();
        self.snapshot.generation += 1;
        let generation = self.snapshot.generation;
        self.snapshot.task = Some(cx.spawn(async move |this, cx| {
            let (rows, positions, groups) = cx
                .background_executor()
                .spawn(async move {
                    let rows = tree::visible_rows(&doc, &expansion);
                    let mut positions = HashMap::new();
                    let mut groups = GroupRows::new();
                    for (at, row) in rows.iter().enumerate() {
                        match row {
                            tree::Row::Node { index, .. } => {
                                positions.insert(*index, at);
                            }
                            tree::Row::Group {
                                parent,
                                first_index,
                                last_index,
                                ..
                            } => groups.entry(*parent).or_default().push((
                                *first_index,
                                *last_index,
                                at,
                            )),
                        }
                    }
                    // Rows list an outer group before its inner groups, so a
                    // stable sort keeps the inner one last among equal starts.
                    for list in groups.values_mut() {
                        list.sort_by_key(|&(first, _, _)| first);
                    }
                    (rows, positions, groups)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.snapshot.generation != generation {
                    return;
                }
                let top = this.list.logical_scroll_top();
                // Rows and the document they index change together.
                if this
                    .snapshot
                    .pending
                    .as_ref()
                    .is_some_and(|pending| Arc::ptr_eq(pending, &used))
                {
                    this.snapshot.pending = None;
                }
                this.snapshot.doc = Some(used);
                this.snapshot.rows = Arc::new(rows);
                this.snapshot.positions = Arc::new(positions);
                this.snapshot.groups = Arc::new(groups);
                this.list
                    .reset_with_uniform_height(this.snapshot.rows.len(), px(OUTLINE_ROW_HEIGHT));
                this.list.scroll_to(ListOffset {
                    item_ix: top.item_ix.min(this.snapshot.rows.len().saturating_sub(1)),
                    offset_in_item: top.offset_in_item,
                });
                // After the list keeps its place: a revealed row scrolls into view.
                this.sync_current(cx);
                cx.notify();
            });
        }));
    }

    /// The chevron or a group row: Contents only, never the viewer's folds.
    fn toggle_snapshot_row(&mut self, row: &tree::Row, cx: &mut Context<Self>) {
        self.expansion().toggle(row);
        self.rebuild_rows(cx);
    }

    fn jump_snapshot(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        // The row's index belongs to the document of the shown rows.
        if let (Some(code), Some(rows)) = (self.code.clone(), self.snapshot.doc.clone()) {
            code.update(cx, |code, cx| {
                code.reveal_contents_node(&rows, index, window, cx)
            });
        }
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        if !self.expanded.remove(&ix) {
            self.expanded.insert(ix);
        }
        self.refresh_rows(cx);
    }

    fn jump(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.data.entries.get(ix) else {
            return;
        };
        if let Some(input) = &self.input {
            input.update(cx, |input, cx| {
                input.scroll_line_to_top(entry.line, cx);
                input.focus_handle(cx).focus(window, cx);
            });
        }
    }
}

impl View {
    #[inline(never)]
    fn render_snapshot(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let line = |text: &'static str| {
            div()
                .debug_selector(|| "file-outline-state".into())
                .flex_none()
                .px_4()
                .py_1()
                .text_color(p.text_muted)
                .child(text)
                .into_any_element()
        };
        match self.snapshot.state {
            SnapshotState::Loading { .. } => return line("Reading structure…"),
            SnapshotState::Invalid(reason) => return line(reason),
            SnapshotState::Absent => return div().into_any_element(),
            SnapshotState::Ready => {}
        }
        let rows = self.snapshot.rows.len() as f32 * OUTLINE_ROW_HEIGHT;
        v_flex()
            .id("file-outline")
            .debug_selector(|| "file-outline-list".into())
            // The list takes the height «Linked from» does not need (#2814),
            // never more than its rows, never less than a few of them.
            .flex_shrink(1.)
            .h(px(rows + 2. * OUTLINE_PADDING_Y))
            .min_h(px(
                (rows + 2. * OUTLINE_PADDING_Y).min(6. * OUTLINE_ROW_HEIGHT)
            ))
            .py(px(OUTLINE_PADDING_Y))
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, ix: usize, _, cx| this.snapshot_row(ix, cx)),
                )
                .size_full(),
            )
            .into_any_element()
    }

    fn snapshot_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let (Some(row), Some(doc)) = (
            self.snapshot.rows.get(ix).cloned(),
            self.snapshot.doc.clone(),
        ) else {
            return div().into_any_element();
        };
        let p = brand::palette(cx);
        let hover = brand::reader_palette(cx).hover;
        let (key, value, tooltip, open, branch) = match row {
            tree::Row::Node { index, .. } => {
                let branch = tree::child_count(&doc, index) > 0;
                let open = branch && self.expansion().row_open(&row);
                (
                    tree::row_key(&doc, index),
                    tree::row_value(&doc, index),
                    tree::row_tooltip(&doc, index),
                    open,
                    branch,
                )
            }
            tree::Row::Group { .. } => {
                let (key, value) = tree::group_label(&doc, &row);
                let open = self.expansion().row_open(&row);
                (key.clone(), value, key, open, true)
            }
        };
        let indent = 14. + (row.depth().saturating_sub(1)).min(12) as f32 * 12.;
        let chevron_row = row.clone();
        h_flex()
            .id(("file-outline-row", ix))
            .debug_selector(move || format!("file-outline-row-{ix}"))
            .flex_none()
            .h(px(OUTLINE_ROW_HEIGHT))
            .items_center()
            .gap_2()
            .pl(px(indent))
            .pr_3()
            .border_l_2()
            .border_color(gpui::transparent_black())
            .text_color(p.text_muted)
            // Where you are: the accent bar and the weight, never a grey alone.
            .when(self.snapshot.current == Some(ix), |row| {
                row.border_color(p.accent)
                    .text_color(p.text)
                    .font_weight(FontWeight::MEDIUM)
            })
            .overflow_hidden()
            .cursor_pointer()
            .hover(move |row| row.bg(hover))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .child(
                div()
                    .id(("file-outline-toggle", ix))
                    .w(px(16.))
                    .flex_none()
                    .when(branch, |toggle| {
                        toggle
                            .debug_selector(move || format!("file-outline-toggle-{ix}"))
                            .child(
                                Icon::new(if open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .xsmall(),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_snapshot_row(&chevron_row, cx);
                            }))
                    }),
            )
            // The key keeps its room; the right part yields first.
            .child(
                div()
                    .flex_none()
                    .max_w(relative(0.7))
                    .min_w_0()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(p.text)
                    .debug_selector(move || format!("file-outline-text-{ix}"))
                    .child(key),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_right()
                    .debug_selector(move || format!("file-outline-value-{ix}"))
                    .child(value),
            )
            .on_click(cx.listener(move |this, _, window, cx| match &row {
                tree::Row::Node { index, .. } => this.jump_snapshot(*index, window, cx),
                // A group row opens or closes on a click anywhere.
                tree::Row::Group { .. } => this.toggle_snapshot_row(&row, cx),
            }))
            .into_any_element()
    }
}

impl Render for View {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.snapshot.active {
            return self.render_snapshot(cx);
        }
        let rows = self.visible.len() as f32 * OUTLINE_ROW_HEIGHT;
        v_flex()
            .id("file-outline")
            .debug_selector(|| "file-outline-list".into())
            .flex_shrink(1.)
            .h(px(rows + 2. * OUTLINE_PADDING_Y))
            .min_h(px(
                (rows + 2. * OUTLINE_PADDING_Y).min(6. * OUTLINE_ROW_HEIGHT)
            ))
            .py(px(OUTLINE_PADDING_Y))
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, row: usize, _, cx| {
                        let Some(&ix) = this.visible.get(row) else {
                            return div().into_any_element();
                        };
                        let entry = &this.data.entries[ix];
                        let p = brand::palette(cx);
                        let hover = brand::reader_palette(cx).hover;
                        let current = this.current == Some(ix);
                        h_flex()
                            .id(("file-outline-row", ix))
                            .debug_selector(move || format!("file-outline-row-{ix}"))
                            .flex_none()
                            .h(px(OUTLINE_ROW_HEIGHT))
                            .items_center()
                            .pl(px(14. + entry.depth.min(12) as f32 * 12.))
                            .pr_3()
                            .border_l_2()
                            .border_color(gpui::transparent_black())
                            .text_color(p.text_muted)
                            .when(current, |row| {
                                row.border_color(p.accent)
                                    .text_color(p.text)
                                    .font_weight(FontWeight::MEDIUM)
                            })
                            .overflow_hidden()
                            .cursor_pointer()
                            .hover(move |row| row.bg(hover))
                            .child(
                                div()
                                    .id(("file-outline-toggle", ix))
                                    .w(px(16.))
                                    .flex_none()
                                    .when(entry.children, |toggle| {
                                        toggle
                                            .debug_selector(move || {
                                                format!("file-outline-toggle-{ix}")
                                            })
                                            .child(
                                                Icon::new(if this.expanded.contains(&ix) {
                                                    IconName::ChevronDown
                                                } else {
                                                    IconName::ChevronRight
                                                })
                                                .xsmall(),
                                            )
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.toggle(ix, cx);
                                            }))
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .debug_selector(move || format!("file-outline-text-{ix}"))
                                    .child(entry.label.clone()),
                            )
                            .on_click(
                                cx.listener(move |this, _, window, cx| this.jump(ix, window, cx)),
                            )
                            .into_any_element()
                    }),
                )
                .size_full(),
            )
            .into_any_element()
    }
}

#[cfg(test)]
#[path = "reader_file_outline/contents_tests.rs"]
mod contents_tests;
#[cfg(test)]
#[path = "reader_file_outline/tests.rs"]
mod tests;
