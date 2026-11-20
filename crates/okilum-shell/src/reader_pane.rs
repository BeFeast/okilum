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
}

impl Surface {
    pub(super) fn new(find_input: Entity<InputState>, use_html: bool) -> Self {
        Self {
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
        }
    }
}
