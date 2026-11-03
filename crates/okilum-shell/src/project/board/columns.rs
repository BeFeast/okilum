//! Window-local empty-column presentation. Source refresh never clears the
//! last completed emptiness decision or the user's explicit expansion.
use super::*;

#[derive(Default)]
pub(super) struct State {
    empty: [bool; 5],
    expanded: [bool; 5],
}
impl State {
    pub(super) fn invalidate(&mut self) {
        self.empty = [false; 5];
    }
    pub(super) fn observe(&mut self, counts: [usize; 5], complete: bool) {
        if complete {
            self.empty = counts.map(|count| count == 0);
        }
    }
    pub(super) fn known_empty(&self, column: Column) -> bool {
        self.empty[column as usize]
    }
    pub(super) fn collapsed(&self, column: Column, count: usize) -> bool {
        count == 0 && self.known_empty(column) && !self.expanded[column as usize]
    }
}

impl Board {
    pub(super) fn column_counts(&self, binding: &Binding) -> [usize; 5] {
        let mut counts = [0; 5];
        for scope in &binding.issues {
            if self
                .filter
                .as_ref()
                .is_none_or(|filter| filter == &scope.executor)
            {
                counts[self.delivery_column(scope) as usize] += 1;
            }
        }
        if self.filter.is_none() {
            for card in &self.local_cards.value.cards {
                counts[card.state.column() as usize] += 1;
            }
        }
        counts[Column::Queue as usize] += binding.card_problems.len();
        counts
    }

    pub(super) fn column_sources_ready(&self, binding: &Binding) -> bool {
        !self.binding_loading
            && !self.local_loading
            && !self.snapshot.busy()
            && !self.delivery_loading
            && self.local_cards.last_success.is_some()
            && self.local_cards.error.is_none()
            && self.local_cards.value.problems.is_empty()
            // Even an annotation-free repository Board must finish its first
            // live read. A failed discovery is not evidence of zero issues.
            && (binding.repository.is_empty()
                || (self.snapshot.forgejo.last_success.is_some()
                    && self.snapshot.forgejo.error.is_none()))
            && (binding.issues.is_empty()
                || (self.snapshot.forgejo.last_success.is_some()
                    && self.snapshot.forgejo.error.is_none()
                    && self.delivery.config.last_success.is_some()
                    && self.delivery.config.error.is_none()
                    && (!binding
                        .issues
                        .iter()
                        .any(|scope| live::thread_id(scope).is_some())
                        || (self.snapshot.t3.last_success.is_some()
                            && self.snapshot.t3.error.is_none()))))
    }

    pub(super) fn expand_empty_column(
        &self,
        column: Column,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (color, glyph) = appearance::stage(column, cx);
        Button::new(("board-expand-empty", column as usize))
            .debug_selector(move || format!("board-expand-empty-{column:?}"))
            .ghost()
            .small()
            .accessibility_label(format!("Expand empty {} column, 0 cards", column.label()))
            .tooltip(format!("Expand {}", column.label()))
            .child(appearance::icon(glyph, color))
            .label(format!("{} 0", column.label()))
            .child(appearance::icon("chevron-right", color))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.columns.expanded[column as usize] = true;
                cx.notify();
            }))
    }

    pub(super) fn collapse_empty_column(
        &self,
        column: Column,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new(("board-collapse-empty", column as usize))
            .debug_selector(move || format!("board-collapse-empty-{column:?}"))
            .ghost()
            .small()
            .accessibility_label(format!("Collapse empty {} column", column.label()))
            .tooltip(format!("Collapse empty {}", column.label()))
            .child(appearance::icon(
                "chevron-left",
                brand::palette(cx).text_muted,
            ))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.columns.expanded[column as usize] = false;
                if column == Column::Verified {
                    this.verified_open = false;
                }
                cx.notify();
            }))
    }

    pub(super) fn add_card_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("board-local-add")
            .debug_selector(|| "board-local-add".into())
            .ghost()
            .small()
            .label("Add card")
            .disabled(self.editor_loading)
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_local_management(local::Mode::Add, window, cx)
            }))
    }
}

#[cfg(test)]
mod tests;
