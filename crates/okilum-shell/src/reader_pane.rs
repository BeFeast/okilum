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

pub(super) struct Surface {
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
}

impl Surface {
    pub(super) fn new(find_input: Entity<InputState>) -> Self {
        Self {
            find_input,
            find_open: false,
            link_choices: Vec::new(),
            prepared_links: Arc::default(),
            link_presentations: Arc::default(),
            link_preparation_generation: 0,
            link_original_source: None,
            link_rendered_source: None,
            link_identities: Arc::default(),
        }
    }
}
