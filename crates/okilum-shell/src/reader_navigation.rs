//! Navigation owned by one document surface, never shared with sibling panes.
use super::reader_view_query::Bindings;
use gpui::{px, ListOffset};
use std::{collections::HashMap, path::PathBuf};

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
    pub history_views: HashMap<usize, Bindings>,
    pub history_ix: usize,
    pub history_nav: Option<usize>,
    /// Latest reading place per canonical root/document, independent of the
    /// Back/Forward branch. Contents derives its active section from this viewport.
    pub places: HashMap<(PathBuf, String), ListOffset>,
    /// A link's line landing highlights its block once it is reached (#1049).
    pub flash_pending: Option<usize>,
    /// The highlighted block and its serial (each highlight fades once).
    pub flash: Option<(usize, u64)>,
}

impl State {
    pub(super) fn view_bindings(&self) -> Option<&Bindings> {
        self.history_views.get(&self.history_ix)
    }

    pub(super) fn push_history(&mut self, path: String, view: Option<Bindings>) {
        let keep = self.history_ix + 1;
        self.history.truncate(keep);
        self.history_positions.truncate(keep);
        self.history_views.retain(|&index, _| index < keep);
        self.history.push(path);
        self.history_positions.push(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        self.history_ix = self.history.len() - 1;
        if let Some(view) = view {
            self.history_views.insert(self.history_ix, view);
        }
    }
}

#[cfg(test)]
mod tests;
