//! State owned by one visible document surface (#773, decomposition stage 1).
//!
//! This is the `S` of `reader_document::Document<S, E>`: everything here
//! belongs to the note shown in one pane and moves with it, never shared with
//! a sibling pane. Window-level state (vault, watcher, sidebar, panels,
//! toasts, Quick Open) stays on `Reader`. Groups move here one at a time;
//! see `specs/773-note-panes.md` §Reader decomposition.
//!
//! `Reader` keeps the active pane's surface inline in `Reader::pane`, so call
//! sites use plain field paths (`self.pane.find_open`) and keep disjoint
//! borrows. The second pane's host stores the inactive surface beside it and
//! swaps on focus, so these call sites do not change again.
use super::*;
use reader_panes::PaneId;

/// What background work for a surface captures (#773, stage 2): the pane it
/// was started for and the generation current then. A completion publishes
/// only while both still match, so a late result can never land in a pane
/// that was replaced, closed or reused for another note.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Ticket {
    pane: PaneId,
    generation: u64,
}

impl Ticket {
    pub(super) fn pane(&self) -> PaneId {
        self.pane
    }
}

#[cfg(test)]
thread_local! {
    /// Test-only: panes whose completions arrived after the pane was closed
    /// and were dropped at the routing boundary, before any ticket check.
    pub(super) static DROPPED_FOR_CLOSED_PANE: std::cell::RefCell<Vec<PaneId>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A completion kept for a pane that was not active when it finished.
pub(super) type Deferred = Box<dyn FnOnce(&mut Reader, &mut Window, &mut Context<Reader>)>;

pub(super) struct Surface {
    /// Stable for this surface's lifetime; a replacement surface gets a new one.
    id: PaneId,

    // The rendered document and this note's history, positions and pending
    // landing.
    pub content: Entity<TextViewState>,
    pub _content_sub: Subscription,
    pub navigation: reader_navigation::State,

    // Which note this surface shows and its source.
    pub current_rel: String,
    pub current_title: String,
    /// Source of the open note as rendered, before any find marks. Kept so
    /// the find bar can re-mark it on every keystroke and restore it on Esc.
    pub note_source: String,
    pub note_canonical_source: Option<Arc<str>>,
    pub built_in_document: bool,

    // Find.
    pub find_input: Entity<InputState>,
    pub find_open: bool,

    // Link preparation for the displayed source.
    pub link_choices: Vec<(String, Option<String>)>,
    pub prepared_links: prepared_links::States,
    /// Last verified appearance for this exact displayed source. Pending action
    /// evidence is cleared separately during same-document verification.
    pub link_presentations: prepared_links::States,
    pub link_preparation_generation: u64,
    pub link_original_source: Option<Arc<str>>,
    pub link_rendered_source: Option<Arc<str>>,
    pub link_identities: Arc<Vec<okilum_core::document_links::prepared::LinkIdentity>>,

    // Rendered document: what the right panel and the document chrome show
    // for this note.
    pub use_html: bool,
    /// Top-level headings of the open document (docs/design/reader.md §Right
    /// panel, #337). Prepared with the document; never reparsed on scroll.
    pub outline: Vec<okilum_core::document_links::HeadingEntry>,
    /// Read-only properties of the open note (#386); `Err` carries a YAML
    /// error to show instead of silently hiding the block.
    pub properties: Result<Vec<okilum_core::properties::Property>, String>,
    /// Inline properties expanded above the document (right panel closed).
    pub properties_open: bool,
    pub show_hidden_properties: bool,
    pub backlinks: Vec<Backlink>,
    /// `backlinks` is grouped by source note: one range per card of the «Linked from» list.
    /// Kept with `Reader::backlinks_list`, which counts exactly these cards (#1135).
    pub backlink_groups: Vec<std::ops::Range<usize>>,
    /// Linking notes whose places are all shown (#394, «Show N more»).
    pub backlinks_expanded: std::collections::HashSet<String>,
    /// A wide table shown at window width over the document (#368).
    pub table_overlay: Option<Entity<TextViewState>>,
    pub timeline: Option<reader_timeline::Timeline>,
    pub file_preview: Option<reader_files::FilePreview>,
    pub typed_navigation: reader_typed_view::Navigation,
    pub query_navigation: reader_view_query::Navigation,
    pub hover_preview: reader_hover::HoverPreview,

    // Editor and recovery for this document. The handle is pane-owned; which
    // pane may edit a note is decided window-wide (stage 3, #773).
    pub editing: Option<reader_editor::Editing>,
    pub recovery_offer: bool,
    pub recovery_checked: bool,
    pub recovery_error: bool,
    pub recovery_dismissed: bool,
    pub recovery_startup: bool,

    // Load pipeline: the document being published into this surface and
    // what it has recorded as usable. The vault's own preparation
    // (`Reader::loading`) stays window-level.
    pub pending_open_document: Option<reader_loading::PendingDocument>,
    pub queued_open_note: Option<String>,
    /// Where an external link queued with `queued_open_note` lands (#1049).
    pub queued_landing: Option<okilum_core::deep_link::Position>,
    pub usable_document: bool,
    pub last_recorded_document: Option<(PathBuf, String, u64)>,
    /// Completions that finished while this pane was inactive, in arrival
    /// order; `resume_active_pane` replays them. Each one rechecks its own
    /// ticket, so a result that went stale meanwhile still changes nothing.
    pub deferred: Vec<Deferred>,

    /// This pane's note was deleted outside Okilum (#1986): its contents now
    /// exist only here. Per pane, so a deleted note in the inactive pane
    /// keeps its record and its Save As / Don't Save choice (#773).
    pub deleted_note: Option<reader_deleted_note::DeletedNote>,
    /// The person chose Don't Save / Save As for exactly this document and
    /// navigation generation; leaving it no longer asks.
    pub deleted_note_leaving: Option<(PathBuf, String, u64)>,

    /// The Find field's subscription for a surface made after startup; the
    /// first surface's lives with the Reader's own subscriptions.
    pub _find_sub: Option<Subscription>,

    /// Test-only: holds the next document preparation before it publishes.
    #[cfg(test)]
    pub preparation_hold: Option<async_channel::Receiver<()>>,
    /// Test-only: completions this surface observed and refused as stale.
    #[cfg(test)]
    pub refused: Vec<Ticket>,
}

impl Surface {
    pub(super) fn new(
        content: Entity<TextViewState>,
        content_sub: Subscription,
        find_input: Entity<InputState>,
        use_html: bool,
        recovery_startup: bool,
    ) -> Self {
        Self {
            id: PaneId::next(),
            content,
            _content_sub: content_sub,
            navigation: Default::default(),
            current_rel: String::new(),
            current_title: String::new(),
            note_source: String::new(),
            note_canonical_source: None,
            built_in_document: false,
            find_input,
            find_open: false,
            link_choices: Vec::new(),
            prepared_links: Arc::default(),
            link_presentations: Arc::default(),
            link_preparation_generation: 0,
            link_original_source: None,
            link_rendered_source: None,
            link_identities: Arc::default(),
            use_html,
            outline: Vec::new(),
            properties: Ok(Vec::new()),
            properties_open: false,
            show_hidden_properties: false,
            backlinks: Vec::new(),
            backlink_groups: Vec::new(),
            backlinks_expanded: std::collections::HashSet::new(),
            table_overlay: None,
            timeline: None,
            file_preview: None,
            typed_navigation: Default::default(),
            query_navigation: Default::default(),
            hover_preview: Default::default(),
            editing: None,
            recovery_offer: false,
            recovery_checked: false,
            recovery_error: false,
            recovery_dismissed: false,
            recovery_startup,
            pending_open_document: None,
            queued_open_note: None,
            queued_landing: None,
            usable_document: false,
            last_recorded_document: None,
            deferred: Vec::new(),
            deleted_note: None,
            deleted_note_leaving: None,
            _find_sub: None,
            #[cfg(test)]
            preparation_hold: None,
            #[cfg(test)]
            refused: Vec::new(),
        }
    }

    pub(super) fn id(&self) -> PaneId {
        self.id
    }

    /// The file this surface's editor owns, while it is editing.
    pub(super) fn edited_path(&self) -> Option<&Path> {
        self.editing.as_ref().map(reader_editor::Editing::path)
    }

    /// For work that must not outlive the document's navigation generation.
    pub(super) fn navigation_ticket(&self) -> Ticket {
        Ticket {
            pane: self.id,
            generation: self.navigation.generation,
        }
    }

    /// For a document preparation request (`open_note` and its variants).
    pub(super) fn preparation_ticket(&self) -> Ticket {
        Ticket {
            pane: self.id,
            generation: self.navigation.preparation_generation,
        }
    }

    /// Whether work stamped with `ticket` was started for this surface. Check
    /// it before touching any state, so work for another pane changes nothing.
    pub(super) fn owns(&self, ticket: Ticket) -> bool {
        ticket.pane == self.id
    }

    pub(super) fn holds_navigation(&mut self, ticket: Ticket) -> bool {
        self.holds(ticket, self.navigation.generation)
    }

    pub(super) fn holds_preparation(&mut self, ticket: Ticket) -> bool {
        self.holds(ticket, self.navigation.preparation_generation)
    }

    fn holds(&mut self, ticket: Ticket, generation: u64) -> bool {
        let current = ticket.pane == self.id && ticket.generation == generation;
        #[cfg(test)]
        if !current {
            self.refused.push(ticket);
        }
        current
    }

    /// Test-only: this surface is closed and a fresh one takes its place with
    /// the same generations, so only the pane identity tells them apart.
    #[cfg(test)]
    pub(super) fn reopen_as_new_pane(&mut self) {
        self.id = PaneId::next();
    }
}

/// Routing of background work to the pane it was started for (#773). One
/// pane today: the inline `Reader::pane`. The second pane's host extends
/// these three, and its focus switch calls `resume_active_pane`.
impl Reader {
    /// The surface with this id, active or not; `None` once it was closed.
    pub(super) fn surface_mut(&mut self, pane: PaneId) -> Option<&mut Surface> {
        if self.pane.id == pane {
            return Some(&mut self.pane);
        }
        self.split
            .as_mut()
            .map(|split| &mut split.other)
            .filter(|other| other.id == pane)
    }

    pub(super) fn is_active_pane(&self, pane: PaneId) -> bool {
        self.pane.id == pane
    }

    /// Runs a completion for `pane` now when it is the active pane. A
    /// completion for an inactive pane is kept with that pane and replayed by
    /// `resume_active_pane`; one for a closed pane is dropped with it. Either
    /// way it never touches the pane that happens to be active.
    pub(super) fn run_for_pane(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
        work: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        #[cfg(test)]
        if !self.is_active_pane(pane) && self.surface_mut(pane).is_none() {
            DROPPED_FOR_CLOSED_PANE.with(|dropped| dropped.borrow_mut().push(pane));
        }
        if self.is_active_pane(pane) {
            work(self, window, cx);
        } else if let Some(surface) = self.surface_mut(pane) {
            surface.deferred.push(Box::new(work));
        }
    }

    /// `run_for_pane` for completions that run without a window.
    pub(super) fn run_for_pane_without_window(
        &mut self,
        pane: PaneId,
        cx: &mut Context<Self>,
        work: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        #[cfg(test)]
        if !self.is_active_pane(pane) && self.surface_mut(pane).is_none() {
            DROPPED_FOR_CLOSED_PANE.with(|dropped| dropped.borrow_mut().push(pane));
        }
        if self.is_active_pane(pane) {
            work(self, cx);
        } else if let Some(surface) = self.surface_mut(pane) {
            surface
                .deferred
                .push(Box::new(move |this, _, cx| work(this, cx)));
        }
    }

    /// Runs `f` for every pane of this window, each as the active surface
    /// for that call, and returns every result: it never stops early, so Quit
    /// and window close protect every pane even after one fails. One pane
    /// today; the second pane's host swaps each inactive surface in for its
    /// call, the same swap it makes on focus.
    pub(super) fn for_each_pane<R>(
        &mut self,
        cx: &mut Context<Self>,
        mut f: impl FnMut(&mut Self, &mut Context<Self>) -> R,
    ) -> Vec<R> {
        let mut results = vec![f(self, cx)];
        if self.split.is_some() {
            self.swap_panes();
            results.push(f(self, cx));
            self.swap_panes();
        }
        results
    }

    /// Replays what the now-active pane finished while it was inactive, then
    /// publishes its pending first document.
    pub(super) fn resume_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for work in std::mem::take(&mut self.pane.deferred) {
            work(self, window, cx);
        }
        self.publish_pending_document(window, cx);
    }
}

/// Shown when Edit is refused because another pane of this window edits the file.
pub(super) const EDITED_ELSEWHERE: &str =
    "This note is being edited in the other pane. Its saved text is shown here.";

/// Why a pane could not be closed, unsplit or have its document replaced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PaneExit {
    /// No pane with this id in the window.
    Missing,
    /// Closing or unsplitting would leave the window without a document.
    LastPane,
    /// The existing save/protect/conflict lifecycle refused; the pane, its
    /// editor and its draft stay as they were.
    Refused,
}

/// Window-level editor ownership (#773, stage 3). The owner registry is
/// derived from each surface's editor handle, so there is no second map to
/// keep in step with the many places that open or drop an editor.
impl Reader {
    /// Every document surface of this window: one until the second pane exists.
    pub(super) fn surfaces(&self) -> impl Iterator<Item = &Surface> {
        std::iter::once(&self.pane).chain(self.split.as_ref().map(|split| &split.other))
    }

    /// The pane whose editor owns `path`, if any.
    pub(super) fn editor_owner(&self, path: &Path) -> Option<PaneId> {
        self.surfaces()
            .find(|surface| surface.edited_path() == Some(path))
            .map(Surface::id)
    }

    /// Ok when `pane` may open an editor for `path`; otherwise the owning pane.
    /// The other pane keeps showing the saved source; nothing is cloned or
    /// transferred.
    pub(super) fn claim_editor(&self, pane: PaneId, path: &Path) -> Result<(), PaneId> {
        match self.editor_owner(path) {
            Some(owner) if owner != pane => Err(owner),
            _ => Ok(()),
        }
    }

    /// Leaving `pane`'s document (navigation, replace, close, unsplit) runs the
    /// existing save/protect/conflict lifecycle. On refusal everything stays.
    pub(super) fn guard_pane_exit(
        &mut self,
        pane: PaneId,
        cx: &mut Context<Self>,
    ) -> Result<(), PaneExit> {
        if pane == self.pane.id() {
            return if self.leave_source(cx) {
                Ok(())
            } else {
                Err(PaneExit::Refused)
            };
        }
        if self
            .split
            .as_ref()
            .is_none_or(|split| split.other.id != pane)
        {
            return Err(PaneExit::Missing);
        }
        // The guard runs on the active surface: swap the inactive pane in for
        // the call only, then put it back whatever the outcome. Leaving an
        // editor also writes window-level presentation (the remembered Live
        // Preview choice, the «On this page» rows); none of that may come
        // from a pane that is not the active one.
        let live_preview = self.ui_state.live_preview;
        self.swap_panes();
        let left = self.leave_source(cx);
        self.swap_panes();
        self.ui_state.live_preview = live_preview;
        self.outline_list
            .reset_with_uniform_height(self.pane.outline.len(), px(OUTLINE_ROW_HEIGHT));
        if left {
            Ok(())
        } else {
            Err(PaneExit::Refused)
        }
    }

    /// Closes `pane` after its exit guard. The last pane is refused before the
    /// guard runs, so its editor and draft are never touched.
    pub(super) fn close_pane(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), PaneExit> {
        if !self.surfaces().any(|surface| surface.id() == pane) {
            return Err(PaneExit::Missing);
        }
        if self.surfaces().count() == 1 {
            return Err(PaneExit::LastPane);
        }
        self.guard_pane_exit(pane, cx)?;
        // Only now, after the guard, is the layout collapsed.
        if pane == self.pane.id() {
            self.swap_panes();
            self.split = None;
            self.pane_activated(window, cx);
        } else {
            self.split = None;
        }
        cx.notify();
        Ok(())
    }

    /// Keeps the active pane and closes the other one through `close_pane`.
    pub(super) fn unsplit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), PaneExit> {
        let active = self.pane.id();
        let Some(other) = self.surfaces().map(Surface::id).find(|id| *id != active) else {
            return Err(PaneExit::LastPane);
        };
        self.close_pane(other, window, cx)
    }
}

/// The second pane (#773, stage 4): the inactive surface beside the active
/// one. `Reader::pane` always holds the active surface, so every call site
/// keeps working on "the" document; focus swaps the two.
pub(super) struct Split {
    pub other: Surface,
    /// The active pane is drawn on the left.
    pub active_left: bool,
}

/// Where a modified click on a resolved local note opens it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Destination {
    /// The pane the link is in: an ordinary click.
    Here,
    /// The other pane, creating it when the window has one: Cmd/Ctrl+click.
    Beside,
    /// A new window through the existing New Window path: Cmd/Ctrl+Shift+click.
    NewWindow,
}

impl Destination {
    pub(super) fn of_click(modifiers: gpui::Modifiers) -> Self {
        match (modifiers.secondary(), modifiers.shift) {
            (true, true) => Self::NewWindow,
            (true, false) => Self::Beside,
            _ => Self::Here,
        }
    }
}

/// The second pane's host: create, focus, swap and draw (#773, stage 4).
impl Reader {
    /// A fresh document surface with its own TextView state and Find field,
    /// configured like the first one.
    pub(super) fn new_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Surface {
        let use_html = self.pane.use_html;
        let sel_format = self.sel_format;
        let find_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in note…"));
        let content = cx.new(|cx| {
            if use_html {
                TextViewState::html("", cx)
                    .retain_selection_on_layout(true)
                    .scrollable(true)
                    .selectable(true)
            } else {
                TextViewState::markdown("", cx)
                    .retain_selection_on_layout(true)
                    .scrollable(true)
                    .selectable(true)
                    .selection_format(sel_format)
            }
        });
        let content_sub = cx.observe(&content, |this, _, cx| this.content_changed(cx));
        let mut surface = Surface::new(content, content_sub, find_input.clone(), use_html, false);
        surface.properties_open = self.pane.properties_open;
        surface.show_hidden_properties = self.pane.show_hidden_properties;
        surface._find_sub = Some(cx.subscribe_in(
            &find_input,
            window,
            |this: &mut Reader, _, e: &InputEvent, window, cx| match e {
                InputEvent::Change => this.run_find(window, cx),
                InputEvent::PressEnter { shift: true, .. } => this.find_step(-1, cx),
                InputEvent::PressEnter { .. } => this.find_step(1, cx),
                _ => {}
            },
        ));
        surface
    }

    /// Runs `f` with the pane whose editor owns `input` as the active surface,
    /// swapping it in for the call when it is the inactive one. Nothing runs
    /// when no pane edits with that input any more.
    pub(super) fn with_pane_of_input(
        &mut self,
        input: EntityId,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, &mut Context<Self>),
    ) {
        let owns = |surface: &Surface| {
            surface
                .editing
                .as_ref()
                .is_some_and(|editing| editing.input().entity_id() == input)
        };
        if owns(&self.pane) {
            f(self, cx);
        } else if self.split.as_ref().is_some_and(|split| owns(&split.other)) {
            self.swap_panes();
            f(self, cx);
            self.swap_panes();
        }
    }

    /// Exchanges the active and inactive surfaces without any other effect.
    /// Every caller restores the order or follows with `pane_activated`.
    pub(super) fn swap_panes(&mut self) {
        if let Some(split) = &mut self.split {
            std::mem::swap(&mut self.pane, &mut split.other);
            split.active_left = !split.active_left;
        }
    }

    /// Makes `pane` the active pane. Focus alone never saves, drops an editor
    /// or resets history, caret or scroll; the pane that loses focus keeps
    /// everything, including an editor and its draft.
    pub(super) fn focus_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_active_pane(pane)
            || self
                .split
                .as_ref()
                .is_none_or(|split| split.other.id != pane)
        {
            return;
        }
        self.swap_panes();
        self.pane_activated(window, cx);
    }

    pub(super) fn focus_other_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(other) = self.split.as_ref().map(|split| split.other.id) {
            self.focus_pane(other, window, cx);
        }
    }

    /// The window-level views that follow the active pane: Contents and
    /// Linked from, the window title, the session record and the document
    /// itself, re-read in case the source changed while the pane was inactive.
    pub(super) fn pane_activated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.outline_list
            .reset_with_uniform_height(self.pane.outline.len(), px(OUTLINE_ROW_HEIGHT));
        if !self.pane.current_rel.is_empty() {
            let rel = self.pane.current_rel.clone();
            self.set_backlinks(self.vault.backlinks(&rel), true);
            self.show_window_title(Some(self.pane.current_title.clone()), window, cx);
        }
        self.resume_active_pane(window, cx);
        // The deleted-note notice describes the active pane's note.
        self.sync_deleted_notice(window, cx);
        self.record_usable_document(cx);
        if self.pane.usable_document && !self.pane.current_rel.is_empty() {
            self.reconcile_published_document(window, cx);
        }
        let focus = match &self.pane.editing {
            Some(editing) => editing.input().focus_handle(cx),
            None => self.pane.content.read(cx).focus_handle().clone(),
        };
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Cmd/Ctrl+\: split the window, showing the current note in a new pane
    /// on the right (Read mode; only one pane may edit a note), or unsplit,
    /// keeping the active pane after the other one's exit guard succeeds.
    pub(super) fn toggle_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.split.is_some() {
            if self.unsplit(window, cx).is_err() {
                // The guard already reported the conflict or save failure.
                cx.notify();
            }
            return;
        }
        let rel = self.pane.current_rel.clone();
        self.open_beside(&rel, None, window, cx);
    }

    /// Opens a note in the other pane, creating it when the window has one,
    /// and makes that pane active. The pane the link was in keeps its note,
    /// editor, history and scroll.
    pub(super) fn open_beside(
        &mut self,
        rel: &str,
        heading: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.split.is_none() {
            let other = self.new_surface(window, cx);
            self.split = Some(Split {
                other,
                active_left: true,
            });
        }
        self.focus_other_pane(window, cx);
        if !rel.is_empty() {
            self.open_note_at(rel, None, heading, window, cx);
        }
    }

    /// The inactive pane: its note title and its rendered document, read-only.
    /// A layer above it takes the first click and only focuses the pane, so a
    /// click never acts on the active pane's state through this drawing.
    pub(super) fn render_inactive_pane(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let split = self.split.as_ref()?;
        let other = &split.other;
        let pane = other.id;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let title = if other.current_title.is_empty() {
            other.current_rel.clone()
        } else {
            other.current_title.clone()
        };
        let mut style = reader_text_zoom::document_style(cx);
        style.heading_base_font_size = px(reader_ui_state::font_size(cx));
        let editing = other.editing.is_some();
        Some(
            v_flex()
                .id("reader-inactive-pane")
                .debug_selector(|| "reader-inactive-pane".into())
                .size_full()
                .min_w(px(0.))
                .overflow_hidden()
                .relative()
                .child(
                    h_flex()
                        .px_4()
                        .py_2()
                        .gap_2()
                        .text_sm()
                        .text_color(muted)
                        .child(div().truncate().child(title))
                        .when(editing, |row| row.child("· Editing")),
                )
                .child(
                    div().flex_1().min_h_0().w_full().child(
                        TextView::new(&other.content)
                            .font_family(reader_fonts::note(cx))
                            .scrollable(true)
                            .selectable(false)
                            .selection_format(self.sel_format)
                            .style(style)
                            .text_size(px(reader_ui_state::font_size(cx)))
                            .pl(note_gutter(window))
                            .pr(px(READER_SIDE_PADDING))
                            .w_full()
                            .h_full(),
                    ),
                )
                .child(
                    div()
                        .id("reader-inactive-pane-focus")
                        .debug_selector(|| "reader-inactive-pane-focus".into())
                        .absolute()
                        .inset_0()
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.focus_pane(pane, window, cx);
                            }),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The active pane's surface beside the inactive one, in their order.
    pub(super) fn render_split(
        &self,
        active: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(inactive) = self.render_inactive_pane(window, cx) else {
            return active;
        };
        let active_left = self.split.as_ref().is_some_and(|split| split.active_left);
        let border = cx.theme().border;
        let active = div()
            .id("reader-active-pane")
            .debug_selector(|| "reader-active-pane".into())
            .flex_1()
            .flex_basis(px(0.))
            .min_w(px(0.))
            .h_full()
            .child(active)
            .into_any_element();
        let inactive = div()
            .flex_1()
            .flex_basis(px(0.))
            .min_w(px(0.))
            .h_full()
            .child(inactive)
            .into_any_element();
        let divider = div().w(px(1.)).h_full().bg(border).into_any_element();
        let (left, right) = if active_left {
            (active, inactive)
        } else {
            (inactive, active)
        };
        h_flex()
            .id("reader-split")
            .size_full()
            .child(left)
            .child(divider)
            .child(right)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
