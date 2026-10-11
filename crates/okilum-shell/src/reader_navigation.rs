//! Navigation owned by one document surface, never shared with sibling panes.
use gpui::ListOffset;

/// Keep history and in-flight landing/load generations together when moving a
/// document between pane hosts. Vault indexes and window layout do not belong here.
#[derive(Default)]
pub(super) struct State {
    pub generation: u64,
    pub pending_landing: Option<ListOffset>,
    pub landing_generation: u64,
    pub preparation_generation: u64,
    pub reconciliation_generation: u64,
    /// Notes opened, oldest first, and the position in it. Every `open_note`
    /// that is not itself a history move pushes onto it (#48).
    pub history: Vec<String>,
    pub history_positions: Vec<ListOffset>,
    pub history_ix: usize,
    pub history_nav: Option<usize>,
    /// A link's line landing highlights its block once it is reached (#1049).
    pub flash_pending: Option<usize>,
    /// The highlighted block and its serial (each highlight fades once).
    pub flash: Option<(usize, u64)>,
}
