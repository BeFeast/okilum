//! What the right panel's Contents reads from this viewer (#2814): the raw
//! whole-file tree it already holds, and navigation in the active view.
use super::*;

/// One sample of «where you are» (#2814 9b). A changed `caret` is a caret
/// move, a Find hit or a Tree selection; a changed `top` alone is scrolling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Whereabouts {
    pub view: View,
    pub page: usize,
    pub caret: Option<usize>,
    pub caret_node: Option<usize>,
    pub top: usize,
    pub top_node: Option<usize>,
}

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
    /// The raw document Contents rows index, for «where you are».
    pub(crate) fn document_raw_for_contents(&self) -> Option<Arc<Document>> {
        self.raw_document.clone()
    }

    #[cfg(test)]
    pub(crate) fn tree_selection_for_tests(&self) -> Option<usize> {
        self.structure.selected
    }

    /// JSON, YAML, TOML and XML (#2814 9a, 9c). `.jsonc`, Rust and Python
    /// keep their private outline.
    fn contents_format(&self) -> bool {
        let rel = self.rel.to_ascii_lowercase();
        (is_json(&self.rel) && !rel.ends_with(".har"))
            || matches!(language(&self.rel), Some("yaml" | "toml" | "xml"))
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
            // Only strict verdicts (JSON, YAML) blank the rows; TOML and XML
            // with a syntax problem keep what the tolerant parse gives.
            Some(doc) if doc.invalid.is_some() => ContentsSource::Invalid(match doc.format {
                Some("YAML") => "No outline: the file is not valid YAML.",
                _ => "No outline: the file is not valid JSON.",
            }),
            Some(doc) if doc.semantic => ContentsSource::Ready(doc.clone()),
            Some(_) => ContentsSource::Absent,
        }
    }

    /// What «where you are» samples after each paint (#2814 9b): the caret and
    /// the top of the view, each with the raw-document node it names. In Raw
    /// and Formatted the caret is the cursor and the node the innermost one
    /// holding it; in Tree it is the selected row, which a Find hit also sets.
    /// Formatted has the same preorder as the raw document. O(log n).
    pub(crate) fn contents_whereabouts(&self, cx: &App) -> Option<Whereabouts> {
        let raw = self.raw_document.as_ref()?;
        let active = self.document.as_ref()?;
        if active.structure.len() != raw.structure.len() {
            return None;
        }
        let node = |index: Option<usize>| index.filter(|&i| i > 0 && i < raw.structure.len());
        match self.view {
            View::Tree => {
                let top = self.structure.scroll.logical_scroll_top_index();
                Some(Whereabouts {
                    view: View::Tree,
                    page: 0,
                    caret: self.structure.selected,
                    caret_node: node(self.structure.selected),
                    top,
                    top_node: node(self.structure.visible.get(top).copied()),
                })
            }
            View::Raw | View::Formatted => {
                use gpui_component::RopeExt as _;
                let input = self.input()?;
                let input = input.read(cx);
                let start = active.pages.get(self.page)?.start;
                let line = input.top_visible_line()?;
                let mut top = start + input.text().line_start_offset(line);
                // The line's indentation belongs to the parent; its first
                // character names the node on that line.
                top += active.text[top..]
                    .bytes()
                    .take_while(|b| *b == b' ' || *b == b'\t')
                    .count();
                let cursor = input.cursor();
                let caret = start + cursor;
                Some(Whereabouts {
                    view: self.view,
                    page: self.page,
                    caret: Some(cursor),
                    caret_node: node(active.row_at(&(caret..caret))),
                    top: line,
                    top_node: node(active.row_at(&(top..top))),
                })
            }
            View::Table => None,
        }
    }

    /// Go to a raw-document node in the active view; never switch views.
    /// Tree opens its parents and the node itself; Formatted and Raw put the
    /// caret at the node start, unfold around it and scroll it to the top.
    /// `rows` is the document the clicked row indexes. While a newer parse
    /// is shown here, the node is found again by its key path.
    pub(crate) fn reveal_contents_node(
        &mut self,
        rows: &Arc<Document>,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(raw), Some(active)) = (self.raw_document.clone(), self.document.clone()) else {
            return;
        };
        let index = if Arc::ptr_eq(&raw, rows) {
            index
        } else {
            match rows.node_in(index, &raw) {
                Some(index) => index,
                None => return,
            }
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
