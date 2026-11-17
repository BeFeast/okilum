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
            #[cfg(test)]
            preparation_hold: None,
            #[cfg(test)]
            refused: Vec::new(),
        }
    }

    pub(super) fn id(&self) -> PaneId {
        self.id
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
        (self.pane.id == pane).then_some(&mut self.pane)
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

    /// Replays what the now-active pane finished while it was inactive, then
    /// publishes its pending first document.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "#773 stage 4 calls this from its pane switch")
    )]
    pub(super) fn resume_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for work in std::mem::take(&mut self.pane.deferred) {
            work(self, window, cx);
        }
        self.publish_pending_document(window, cx);
    }
}

#[cfg(test)]
mod tests;
