//! Virtualized structure for minified files: full AST, independent of source pages.
use super::*;
use gpui_component::scroll::ScrollableElement as _;
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct Structure {
    collapsed: bool,
    exceptions: HashSet<usize>,
    pub selected: Option<usize>,
    pub match_node: Option<usize>,
    pub visible: Vec<usize>,
    scroll: UniformListScrollHandle,
}

impl Structure {
    pub fn reset(&mut self, doc: &Document) {
        self.collapsed = true;
        self.exceptions.clear();
        self.exceptions.insert(0); // Show the root and its direct children.
        self.selected = (!doc.structure.is_empty()).then_some(0);
        self.match_node = None;
        self.rebuild(doc);
    }

    pub(super) fn folded(&self, index: usize) -> bool {
        self.collapsed != self.exceptions.contains(&index)
    }

    fn rebuild(&mut self, doc: &Document) {
        self.visible.clear();
        let mut index = 0;
        while index < doc.structure.len() {
            self.visible.push(index);
            index = if self.folded(index) {
                doc.structure[index].subtree_end
            } else {
                index + 1
            };
        }
    }

    fn all(&mut self, folded: bool, doc: &Document) {
        self.collapsed = folded;
        self.exceptions.clear();
        self.rebuild(doc);
    }

    fn toggle(&mut self, index: usize, doc: &Document) {
        if !self.exceptions.remove(&index) {
            self.exceptions.insert(index);
        }
        self.selected = Some(index);
        self.rebuild(doc);
    }

    pub fn reveal(&mut self, range: &std::ops::Range<usize>, doc: &Document) {
        let Some(index) = doc.row_at(range) else {
            return;
        };
        let mut parent = doc.structure[index].parent;
        while let Some(index) = parent {
            if self.collapsed {
                self.exceptions.insert(index);
            } else {
                self.exceptions.remove(&index);
            }
            parent = doc.structure[index].parent;
        }
        self.selected = Some(index);
        self.match_node = Some(index);
        self.rebuild(doc);
        if let Some(visible) = self.visible.iter().position(|&row| row == index) {
            self.scroll
                .scroll_to_item_strict(visible, ScrollStrategy::Center);
        }
    }
}

impl CodePreview {
    pub(super) fn fold_all(&mut self, folded: bool, cx: &mut Context<Self>) {
        if !self.fold_availability().available() {
            return;
        }
        if self.view == View::Tree {
            if let Some(doc) = &self.document {
                self.structure.all(folded, doc);
            }
        } else if let Some(input) = self.input() {
            input.update(cx, |s, cx| s.set_all_folded(folded, cx));
        }
        cx.notify();
    }

    pub(super) fn structure_element(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .debug_selector(|| "data-structure-viewport".into())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus.focus(window, cx)),
            )
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                uniform_list(
                    "data-structure-rows",
                    self.structure.visible.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| this.structure_row(index, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.structure.scroll)
                .size_full(),
            )
            .vertical_scrollbar(&self.structure.scroll)
            .into_any_element()
    }

    fn structure_row(&self, visible: usize, cx: &mut Context<Self>) -> AnyElement {
        let doc = self.document.as_ref().unwrap();
        let index = self.structure.visible[visible];
        let row = &doc.structure[index];
        let branch = row.subtree_end > index + 1;
        let hit = self
            .matches
            .get(self.match_index)
            .filter(|_| self.structure.match_node == Some(index));
        let label = hit.map_or_else(|| doc.row_label(index), |hit| doc.row_hit_label(index, hit));
        let highlights = hit.and_then(|hit| {
            let needle = &doc.text[hit.clone()];
            label.find(needle).map(|start| {
                (
                    start..start + needle.len(),
                    HighlightStyle {
                        background_color: Some(cx.theme().warning),
                        ..Default::default()
                    },
                )
            })
        });
        let range = row.range.clone();
        let source_offset = range.start;
        let node_source = doc.text.clone();
        h_flex()
            .id(("data-structure-row", index))
            .debug_selector(move || format!("data-structure-row-{index}"))
            .w_full()
            .h(px(32.))
            .min_h(px(32.))
            .max_h(px(32.))
            .flex_none()
            .overflow_hidden()
            .items_center()
            .gap_2()
            .pl(px(row.depth.min(16) as f32 * 16.))
            .when(self.structure.selected == Some(index), |row| {
                row.bg(cx.theme().accent)
            })
            .child(
                Button::new(("data-node-fold", index))
                    .small()
                    .ghost()
                    .label(if !branch {
                        "·"
                    } else if self.structure.folded(index) {
                        "▸"
                    } else {
                        "▾"
                    })
                    .disabled(!branch)
                    .debug_selector(move || format!("data-node-fold-{index}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(doc) = &this.document {
                            this.structure.toggle(index, doc);
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{}", row.line)),
            )
            .child(
                div()
                    .id(("data-node-label", index))
                    .debug_selector(move || format!("data-node-label-{index}"))
                    .when(hit.is_some(), |label| {
                        label.debug_selector(|| "data-tree-match-label".into())
                    })
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_family(reader_fonts::code(cx))
                    .child(StyledText::new(label).with_highlights(highlights))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.structure.selected = Some(index);
                            cx.notify();
                        }),
                    ),
            )
            .child(
                Button::new(("data-node-copy", index))
                    .small()
                    .ghost()
                    .label("Copy node")
                    .debug_selector(move || format!("data-node-copy-{index}"))
                    .on_click(move |_, window, cx| {
                        copy_text_node(&node_source[range.clone()], window, cx)
                    }),
            )
            .child(
                Button::new(("data-node-source", index))
                    .small()
                    .ghost()
                    .debug_selector(move || format!("data-node-source-{index}"))
                    .label("Go to Raw")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let Some(doc) = &this.document else {
                            return;
                        };
                        let page = doc.page_at(source_offset);
                        this.apply_view(View::Raw, window, cx);
                        this.show_page(page, window, cx);
                    })),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn data_view_2669_minified_structure_folds_all_and_individual_nodes_across_pages() {
        crate::source_presentation::code_languages::install();
        let text = format!(
            "{{\"outer\":{{\"long\":\"{}\",\"inner\":{{\"answer\":42}}}},\"tail\":true}}",
            "x".repeat(100_000)
        );
        let doc = Document::new(text.clone(), "json");
        let mut view = Structure::default();
        view.reset(&doc);
        assert_eq!(view.visible.len(), 3); // object + its two properties
        view.all(true, &doc);
        assert_eq!(view.visible, vec![0]);
        view.toggle(0, &doc);
        assert_eq!(view.visible.len(), 3);
        view.all(false, &doc);
        assert_eq!(view.visible.len(), doc.structure.len());
        assert_eq!(view.visible.len(), 6);
        let nested = doc
            .structure
            .iter()
            .position(|r| &doc.text[r.range.clone()] == "\"inner\":{\"answer\":42}")
            .unwrap();
        assert_eq!(doc.structure[nested].depth, 2);
        view.toggle(nested, &doc);
        assert!(!view.visible.contains(&(nested + 1)));
        assert!(
            view.visible.contains(&(doc.structure.len() - 1)),
            "a sibling remains visible"
        );
        let restored = doc
            .pages
            .iter()
            .map(|r| &doc.text[r.clone()])
            .collect::<String>();
        assert_eq!(restored, text);
    }
    #[gpui::test]
    fn json2787_structure_labels_stay_inside_fixed_rows_at_every_width(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("__test2.json");
        std::fs::write(
            &path,
            format!(
                "{{\"info\":{{\"description\":\"{}\"}},\"paths\":{{\"long\":true}}}}",
                "wide label ".repeat(20_000)
            ),
        )
        .unwrap();
        cx.update(crate::test_init);
        let mut preview = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                CodePreview::new(
                    fixture.path().to_owned(),
                    "__test2.json".into(),
                    path.clone(),
                    window,
                    cx,
                )
            });
            preview = Some(view.clone());
            Root::new(cx.new(|_| Surface(view)), window, cx)
        });
        let preview = preview.unwrap();
        visual.run_until_parked();
        preview.update_in(visual, |p, window, cx| p.set_view(View::Tree, window, cx));
        visual.run_until_parked();
        for width in [360., 640., 1200.] {
            visual.simulate_resize(size(px(width), px(600.)));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let visible = preview.read_with(visual, |p, _| {
                assert!(p.view == View::Tree);
                assert!(
                    p.outline_input().is_none(),
                    "a page is not an outline document"
                );
                p.structure.visible.clone()
            });
            assert!(
                visible.len() >= 3,
                "positive control: multiple structure rows are drawn"
            );
            for (row_selector, label_selector) in [
                ("data-structure-row-0", "data-node-label-0"),
                ("data-structure-row-1", "data-node-label-1"),
            ] {
                let row = visual.debug_bounds(row_selector).unwrap();
                let label = visual.debug_bounds(label_selector).unwrap();
                assert_eq!(row.size.height, px(32.));
                assert!(
                    label.top() >= row.top() && label.bottom() <= row.bottom(),
                    "label overlaps a neighbouring row at width {width}: {label:?} outside {row:?}"
                );
            }
        }
    }
}
