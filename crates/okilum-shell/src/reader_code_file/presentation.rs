//! Small presentation-only handoff across explicit Read/Edit. No source tree
//! or full text is retained beside a large editable buffer.
use super::*;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PresentationState {
    pub(crate) views: Vec<View>,
    pub(crate) query: String,
    pub(crate) case_sensitive: bool,
    view: View,
    page: usize,
    tree: structure::Saved,
    match_index: usize,
    selection: std::ops::Range<usize>,
    scroll: Point<Pixels>,
    table: Option<reader_delimited::Saved>,
}

impl CodePreview {
    pub(crate) fn save_presentation(&self, cx: &App) -> PresentationState {
        PresentationState {
            views: View::ALL
                .into_iter()
                .filter(|v| self.availability(*v) != Availability::Absent)
                .collect(),
            view: self.view,
            page: self.page,
            tree: self.structure.save(),
            query: self.find.read(cx).value().to_string(),
            case_sensitive: self.case_sensitive.get(),
            table: self.table.as_ref().map(|t| t.read(cx).save_presentation()),
            match_index: self.match_index,
            selection: self
                .input()
                .map_or(0..0, |input| input.read(cx).selected_range()),
            scroll: self.input().map_or(point(px(0.), px(0.)), |input| {
                input.read(cx).scroll_offset()
            }),
        }
    }

    pub(crate) fn restore_presentation(
        &mut self,
        saved: PresentationState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.restored_presentation = Some(saved);
        self.automatic_view = false;
        self.provisional_interacted.set(true);
        self.restore_loaded_presentation(window, cx);
    }

    pub(super) fn restore_loaded_presentation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(saved) = self.restored_presentation.clone() else {
            return;
        };
        if let Some(table_saved) = self
            .restored_presentation
            .as_mut()
            .and_then(|saved| saved.table.take())
        {
            if let Some(table) = &self.table {
                table.update(cx, |table, cx| {
                    table.restore_presentation(table_saved, window, cx)
                });
            }
        }
        if reader_delimited::eligible(&self.rel)
            && saved.view == View::Table
            && self.availability(View::Table).available()
        {
            self.apply_view(View::Table, window, cx);
            self.restored_presentation = None;
            return;
        }
        if self.document.is_none() {
            return;
        }
        let view = if self.availability(saved.view).available() {
            saved.view
        } else {
            View::Raw
        };
        self.apply_view(view, window, cx);
        if self.json.as_ref().is_some_and(|json| json.formatting) {
            return;
        }
        self.case_sensitive.set(saved.case_sensitive);
        if self.find.read(cx).value().as_ref() != saved.query {
            self.find.update(cx, |find, cx| {
                find.set_value(saved.query.clone(), window, cx)
            });
        }
        self.search(window, cx);
        if !self.searching {
            self.finish_presentation_restore(window, cx);
        }
    }

    pub(super) fn finish_presentation_restore(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(saved) = self.restored_presentation.take() else {
            return;
        };
        self.match_index = saved.match_index.min(self.matches.len().saturating_sub(1));
        self.show_page(saved.page, window, cx);
        if self.paged && self.view != View::Tree && self.view != View::Table {
            self.install_page_matches(cx);
        }
        if let Some(doc) = &self.document {
            self.structure.restore(saved.tree, doc);
        }
        if let Some(input) = self.input() {
            input.update(cx, |input, cx| {
                let len = input.text().len();
                let start = saved.selection.start.min(len);
                let end = saved.selection.end.min(len).max(start);
                input.set_selected_range(start..end, cx);
                input.set_scroll_offset(saved.scroll, cx);
            });
        }
        if let Some(find) = &self.editor_find {
            find.update(cx, |find, _| find.preserve_selection());
        }
        cx.notify();
    }
}
