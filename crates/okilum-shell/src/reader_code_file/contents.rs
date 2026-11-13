//! What the right panel's Contents reads from this viewer (#2814): the raw
//! whole-file tree it already holds, and navigation in the active view.
use super::*;

/// Contents for the current file, in the order the panel checks.
#[derive(Clone)]
pub(crate) enum ContentsSource {
    /// No structure is read for this file or view.
    Absent,
    /// The viewer is still reading or parsing the file.
    Loading,
    /// A strict parser rejected the file: the one-line reason.
    Invalid(&'static str),
    Ready(Arc<Document>),
}

impl CodePreview {
    #[cfg(test)]
    pub(crate) fn tree_selection_for_tests(&self) -> Option<usize> {
        self.structure.selected
    }

    /// JSON only in this slice (9a); `.jsonc` keeps its private outline.
    fn contents_format(&self) -> bool {
        is_json(&self.rel) && !self.rel.to_ascii_lowercase().ends_with(".har")
    }

    /// The raw document, never the Formatted one, so row indices and open rows
    /// stay the same in every view. Absent while Table is the active view.
    pub(crate) fn contents_source(&self) -> ContentsSource {
        if !self.contents_format() || self.view == View::Table {
            return ContentsSource::Absent;
        }
        match &self.raw_document {
            None if matches!(self.state, State::Loading | State::Content(_)) => {
                ContentsSource::Loading
            }
            None => ContentsSource::Absent,
            Some(doc) if doc.invalid.is_some() => {
                ContentsSource::Invalid("No outline: the file is not valid JSON.")
            }
            Some(doc) if doc.semantic => ContentsSource::Ready(doc.clone()),
            Some(_) => ContentsSource::Absent,
        }
    }

    /// Go to a raw-document node in the active view; never switch views.
    /// Tree opens its parents and the node itself; Formatted and Raw put the
    /// caret at the node start, unfold around it and scroll it to the top.
    pub(crate) fn reveal_contents_node(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(raw), Some(active)) = (self.raw_document.clone(), self.document.clone()) else {
            return;
        };
        // Formatted is the same JSON with another layout: the preorder index
        // names the same node in both snapshots.
        if index >= raw.structure.len() || active.structure.len() != raw.structure.len() {
            return;
        }
        match self.view {
            View::Tree => {
                self.structure.reveal_node(index, &active);
            }
            View::Raw | View::Formatted => {
                let start = active.structure[index].range.start;
                let page = active.page_at(start);
                if page != self.page {
                    self.show_page(page, window, cx);
                }
                let page = active.pages[self.page].clone();
                if let Some(input) = self.input() {
                    input.update(cx, |input, cx| {
                        use gpui_component::input::RopeExt as _;
                        let local = start.saturating_sub(page.start).min(page.len());
                        let position = input.text().offset_to_position(local);
                        input.unfold_at(position, cx);
                        input.set_selected_range(local..local, cx);
                        input.scroll_line_to_top(position.line as usize, cx);
                    });
                }
                if let Some(editor) = &self.editor_find {
                    editor.update(cx, |editor, _| editor.preserve_selection());
                }
            }
            View::Table => return,
        }
        cx.notify();
    }
}
