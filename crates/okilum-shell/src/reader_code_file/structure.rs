//! Virtualized semantic rows. The synthetic document root is never painted.
use super::*;
use gpui_component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement as _;
use std::collections::HashSet;

pub(super) mod digit_glyphs;

#[derive(Default)]
pub(super) struct Structure {
    collapsed: bool,
    digit_glyphs: digit_glyphs::DigitGlyphs,
    exceptions: HashSet<usize>,
    find_opened: HashSet<usize>,
    pub selected: Option<usize>,
    hovered: Option<usize>,
    pub match_node: Option<usize>,
    pub visible: Vec<usize>,
    pub(super) scroll: UniformListScrollHandle,
}

impl Structure {
    fn start(doc: &Document) -> usize {
        usize::from(doc.json_tree && !doc.structure.is_empty())
    }
    pub fn reset(&mut self, doc: &Document) {
        self.collapsed = true;
        self.exceptions.clear();
        if !doc.json_tree && !doc.structure.is_empty() {
            self.exceptions.insert(0);
        }
        self.find_opened.clear();
        self.selected = if doc.json_tree {
            doc.children(0).next()
        } else {
            (!doc.structure.is_empty()).then_some(0)
        };
        self.match_node = None;
        self.hovered = None;
        self.rebuild(doc);
    }
    pub(super) fn folded(&self, index: usize) -> bool {
        self.collapsed != self.exceptions.contains(&index)
    }
    fn rebuild(&mut self, doc: &Document) {
        self.visible.clear();
        let mut index = Self::start(doc);
        while index < doc.structure.len() {
            self.visible.push(index);
            index = if self.folded(index) {
                doc.structure[index].subtree_end
            } else {
                index + 1
            };
        }
    }
    fn set_folded(&mut self, index: usize, folded: bool) {
        if folded != self.collapsed {
            self.exceptions.insert(index);
        } else {
            self.exceptions.remove(&index);
        }
    }
    fn all(&mut self, folded: bool, doc: &Document) {
        self.collapsed = folded;
        self.exceptions.clear();
        self.find_opened.clear();
        self.rebuild(doc);
        if self.selected.is_some_and(|i| !self.visible.contains(&i)) {
            self.selected = self.visible.first().copied();
        }
    }
    pub(super) fn toggle(&mut self, index: usize, doc: &Document) {
        self.set_folded(index, !self.folded(index));
        self.find_opened.remove(&index);
        self.selected = Some(index);
        self.rebuild(doc);
    }
    fn scroll_selected(&self) {
        if let Some(index) = self
            .selected
            .and_then(|i| self.visible.binary_search(&i).ok())
        {
            self.scroll
                .scroll_to_item_strict(index, ScrollStrategy::Center);
        }
    }
    pub fn reveal(&mut self, range: &std::ops::Range<usize>, doc: &Document) {
        let Some(index) = doc.row_at(range) else {
            return;
        };
        // Separators/outer whitespace belong only to the hidden JSON root.
        // Keep the current visible selection; no other row owns this hit.
        if index < Self::start(doc) {
            self.match_node = None;
            return;
        }
        let mut parent = doc.parent(index);
        while let Some(i) = parent {
            if self.folded(i) {
                self.find_opened.insert(i);
                self.set_folded(i, false);
            }
            parent = doc.parent(i);
        }
        self.selected = Some(index);
        self.match_node = self.selected;
        self.rebuild(doc);
        self.scroll_selected();
    }
    pub fn finish_find(&mut self, doc: &Document) {
        let mut retained = HashSet::new();
        let mut parent = self.selected.and_then(|i| doc.parent(i));
        while let Some(i) = parent {
            retained.insert(i);
            parent = doc.parent(i);
        }
        for index in std::mem::take(&mut self.find_opened) {
            if !retained.contains(&index) {
                self.set_folded(index, true);
            }
        }
        self.match_node = None;
        self.rebuild(doc);
        self.scroll_selected();
    }
}

impl CodePreview {
    pub(super) fn fold_all(&mut self, folded: bool, cx: &mut Context<Self>) {
        self.provisional_interacted.set(true);
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
    pub(super) fn tree_move(&mut self, direction: i32, cx: &mut Context<Self>) {
        if self.view != View::Tree || self.structure.visible.is_empty() {
            return;
        }
        let at = self
            .structure
            .selected
            .and_then(|i| self.structure.visible.binary_search(&i).ok())
            .unwrap_or(0);
        let at = (at as i64 + direction as i64).clamp(0, self.structure.visible.len() as i64 - 1)
            as usize;
        self.structure.selected = Some(self.structure.visible[at]);
        self.structure.scroll_selected();
        cx.notify();
    }
    pub(super) fn tree_branch(&mut self, right: bool, cx: &mut Context<Self>) {
        if self.view != View::Tree {
            return;
        }
        let (Some(doc), Some(index)) = (&self.document, self.structure.selected) else {
            return;
        };
        let branch = doc.structure[index].subtree_end > index + 1;
        if right {
            if branch && self.structure.folded(index) {
                self.structure.toggle(index, doc);
            } else if branch {
                self.structure.selected = Some(index + 1);
            }
        } else if branch && !self.structure.folded(index) {
            self.structure.toggle(index, doc);
        } else if let Some(parent) = doc
            .parent(index)
            .filter(|i| *i >= structure::Structure::start(doc))
        {
            self.structure.selected = Some(parent);
        }
        self.structure.scroll_selected();
        cx.notify();
    }
    pub(super) fn show_selected_raw(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = self.structure.selected.and_then(|i| {
            self.document
                .as_ref()?
                .structure
                .get(i)
                .map(|r| r.range.clone())
        }) else {
            return;
        };
        self.apply_view(View::Raw, window, cx);
        let Some(doc) = &self.document else {
            return;
        };
        self.show_page(doc.page_at(range.start), window, cx);
        let page = self.document.as_ref().unwrap().pages[self.page].clone();
        if let Some(input) = self.input() {
            input.update(cx, |s, cx| {
                use gpui_component::input::RopeExt as _;
                s.unfold_at(s.text().offset_to_position(range.start - page.start), cx);
                s.set_selected_range(
                    range.start - page.start..range.end.min(page.end) - page.start,
                    cx,
                );
            });
        }
        if let Some(editor) = &self.editor_find {
            editor.update(cx, |editor, _| editor.preserve_selection());
        }
        self.focus.focus(window, cx);
    }
    pub(super) fn structure_element(&self, cx: &mut Context<Self>) -> AnyElement {
        let visible = &self.structure.visible;
        let fractions: Vec<_> = self
            .scrollbar_marks
            .iter()
            .map(|row| {
                let at = visible.partition_point(|i| i <= row).saturating_sub(1);
                at as f32 / visible.len().max(1) as f32
            })
            .collect();
        let current_mark = brand::reader_palette(cx).find_mark_current;
        let highlight = brand::reader_palette(cx).find_mark;
        let track_color = brand::reader_palette(cx).find_mark_track;
        let current = self
            .structure
            .match_node
            .and_then(|i| visible.binary_search(&i).ok())
            .map(|i| i as f32 / visible.len().max(1) as f32);
        let tree = div()
            .id("data-tree")
            .key_context("ReaderStructure")
            .track_focus(&self.focus)
            .debug_selector(|| "data-structure-viewport".into())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, w, cx| this.focus.focus(w, cx)),
            )
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(self.structure.digit_glyphs.element(cx))
            .child(
                uniform_list(
                    "data-structure-rows",
                    visible.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        range.map(|i| this.structure_row(i, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.structure.scroll)
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(2.))
                    .w(px(8.))
                    .child(
                        canvas(
                            |bounds, _, _| bounds,
                            move |bounds, _, window, _| {
                                paint_find_quad(window, bounds, track_color, 0);
                            },
                        )
                        .size_full(),
                    )
                    .debug_selector(|| "data-find-mark-track".into())
                    .when(fractions.is_empty(), |track| track.invisible()),
            )
            // The track is below the draggable thumb; ticks remain above it.
            .vertical_scrollbar(&self.structure.scroll)
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |bounds, _, w, _| {
                        for fraction in &fractions {
                            paint_find_quad(
                                w,
                                Bounds::new(
                                    point(
                                        bounds.left(),
                                        (bounds.top() + bounds.size.height * *fraction)
                                            .min(bounds.bottom() - px(3.)),
                                    ),
                                    size(px(6.), px(3.)),
                                ),
                                highlight,
                                1,
                            );
                        }
                        if let Some(fraction) = current {
                            paint_find_quad(
                                w,
                                Bounds::new(
                                    point(
                                        bounds.left(),
                                        (bounds.top() + bounds.size.height * fraction)
                                            .min(bounds.bottom() - px(4.)),
                                    ),
                                    size(px(6.), px(4.)),
                                ),
                                current_mark,
                                2,
                            );
                        }
                    },
                )
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(2.))
                .w(px(8.)),
            )
            .into_any_element();
        #[cfg(feature = "data-view-native-harness")]
        let tree = toolbar_native::trace::Trace::new(tree, 1).into_any_element();
        tree
    }

    fn row_highlights(
        &self,
        index: usize,
        label: &str,
        cx: &App,
    ) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
        let current = self.structure.match_node == Some(index);
        let matches: Vec<_> = self
            .match_pattern
            .as_ref()
            .map_or_else(Vec::new, |pattern| {
                pattern
                    .find_iter(label)
                    .map(|hit| {
                        (
                            hit.range(),
                            HighlightStyle {
                                background_color: Some(if current {
                                    brand::reader_palette(cx).find_current
                                } else {
                                    brand::reader_palette(cx).find_match
                                }),
                                ..Default::default()
                            },
                        )
                    })
                    .collect()
            });
        let doc = self.document.as_ref().unwrap();
        let values = doc.row_value_start(index, label).map(|start| {
            let mut style = if doc.container(index).is_some() {
                HighlightStyle {
                    color: Some(brand::reader_palette(cx).text_faint),
                    ..Default::default()
                }
            } else if let Some((kind, _)) = doc.scalar(index) {
                let name = match kind {
                    document::ScalarKind::String => "string",
                    document::ScalarKind::Number => "number",
                    document::ScalarKind::Boolean => "boolean",
                    document::ScalarKind::Null => "constant",
                };
                cx.theme()
                    .highlight_theme
                    .style
                    .syntax
                    .style(name)
                    .unwrap_or_default()
            } else {
                HighlightStyle {
                    color: Some(brand::reader_palette(cx).text_faint),
                    ..Default::default()
                }
            };
            style.font_weight = Some(FontWeight::NORMAL);
            (start..label.len(), style)
        });
        combine_highlights(values, matches).collect()
    }

    #[inline(never)]
    fn structure_row(&self, visible: usize, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(feature = "data-view-native-harness")]
        NATIVE_ROW_BUILDS.with(|count| count.set(count.get() + 1));
        #[cfg(feature = "data-view-native-harness")]
        let _work = toolbar_native::trace::Work::start(13);
        let doc = self.document.as_ref().unwrap();
        let index = self.structure.visible[visible];
        let row = &doc.structure[index];
        let branch = row.subtree_end > index + 1;
        let selected = self.structure.selected == Some(index);
        let current = self.structure.match_node == Some(index);
        let hit = self
            .matches
            .get(self.match_index)
            .filter(|_| current)
            .or_else(|| self.match_rows.get(&index));
        let label = hit.map_or_else(|| doc.row_label(index), |h| doc.row_hit_label(index, h));
        let highlights = self.row_highlights(index, &label, cx);
        let entity = cx.entity();
        let menu = entity.clone();
        let show_actions = selected || self.structure.hovered == Some(index);
        let element = h_flex()
            .id(("data-structure-row", index))
            .debug_selector(move || format!("data-structure-row-{index}"))
            .w_full()
            .h(px(32.))
            .min_h(px(32.))
            .max_h(px(32.))
            .flex_none()
            .overflow_hidden()
            .items_center()
            .pr(px(12.))
            .gap_1()
            .pl(px(
                row.depth.saturating_sub(usize::from(doc.json_tree)).min(16) as f32 * 16.,
            ))
            .border_l_2()
            .border_color(if current {
                brand::palette(cx).accent
            } else {
                transparent_black()
            })
            .when(selected, |r| r.bg(brand::palette(cx).selected))
            .hover(|r| r.bg(brand::reader_palette(cx).hover))
            .on_hover(cx.listener(move |s, hovered: &bool, _, cx| {
                let next = if *hovered {
                    Some(index)
                } else {
                    s.structure.hovered.filter(|old| *old != index)
                };
                if s.structure.hovered != next {
                    s.structure.hovered = next;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |s, _, w, cx| {
                    s.structure.selected = Some(index);
                    s.focus.focus(w, cx);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |s, _, w, cx| {
                    s.structure.selected = Some(index);
                    s.focus.focus(w, cx);
                    cx.notify();
                }),
            )
            .child(
                Button::new(("data-node-fold", index))
                    .small()
                    .ghost()
                    .w(px(28.))
                    .h(px(28.))
                    .icon(if self.structure.folded(index) {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    })
                    .when(!branch, |b| b.invisible())
                    .disabled(!branch)
                    .debug_selector(move || format!("data-node-fold-{index}"))
                    .on_click(cx.listener(move |s, _, _, cx| {
                        if let Some(doc) = &s.document {
                            s.structure.toggle(index, doc);
                        }
                        cx.notify();
                    })),
            )
            .child({
                let label = div()
                    .id(("data-node-label", index))
                    .debug_selector(move || format!("data-node-label-{index}"))
                    .when(self.match_rows.contains_key(&index), |d| {
                        d.debug_selector(|| "data-tree-match-label".into())
                    })
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(text_size(cx))
                    .font_weight(FontWeight::NORMAL)
                    .font_family(reader_fonts::code(cx))
                    .child(StyledText::new(label).with_highlights(highlights));
                #[cfg(test)]
                let label = label.child(
                    canvas(
                        |_, _, _| (),
                        |_, _, window, _| {
                            LABEL_WEIGHTS.with(|weights| {
                                weights.borrow_mut().push(window.text_style().font_weight)
                            })
                        },
                    )
                    .absolute(),
                );
                let label = label.into_any_element();
                #[cfg(feature = "data-view-native-harness")]
                let label = toolbar_native::trace::Trace::new(label, 3).into_any_element();
                label
            })
            .child(
                h_flex()
                    .w(px(60.))
                    .flex_none()
                    .when(show_actions, |slot| slot.child(self.row_actions(index, cx))),
            )
            .context_menu(move |m, w, cx| {
                menu.update(cx, |s, _| s.structure.selected = Some(index));
                Self::tree_menu(m, w, cx)
            })
            .into_any_element();
        #[cfg(feature = "data-view-native-harness")]
        let element = toolbar_native::trace::Trace::new(element, 2).into_any_element();
        element
    }
    #[inline(never)]
    fn row_actions(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(test)]
        ACTION_BUILDS.with(|count| count.set(count.get() + 1));
        let copy = cx.entity();
        let raw = copy.clone();
        h_flex()
            .child(
                reader_icon_button(
                    ("data-node-copy", index),
                    IconName::Copy,
                    reader_shortcuts::hint("Copy node", &CopyNode, cx),
                    cx,
                )
                .debug_selector(move || format!("data-node-copy-{index}"))
                .on_click(move |_, w, cx| {
                    copy.update(cx, |s, cx| {
                        s.structure.selected = Some(index);
                        s.copy_node(w, cx);
                    })
                }),
            )
            .child(
                reader_icon_button(
                    ("data-node-source", index),
                    Icon::default().path("icons/code-xml.svg"),
                    reader_shortcuts::hint("Show in raw text", &ShowNodeRaw, cx),
                    cx,
                )
                .debug_selector(move || format!("data-node-source-{index}"))
                .on_click(move |_, w, cx| {
                    raw.update(cx, |s, cx| {
                        s.structure.selected = Some(index);
                        s.show_selected_raw(w, cx);
                    })
                }),
            )
            .into_any_element()
    }
    fn tree_menu(menu: PopupMenu, _: &mut Window, _cx: &mut Context<PopupMenu>) -> PopupMenu {
        let items = [
            PopupMenuItem::new("Copy node").action(Box::new(CopyNode)),
            PopupMenuItem::new("Show in raw text").action(Box::new(ShowNodeRaw)),
            PopupMenuItem::new("Fold all").action(Box::new(FoldAll)),
            PopupMenuItem::new("Unfold all").action(Box::new(UnfoldAll)),
        ];
        #[cfg(test)]
        TREE_MENU.with(|record| {
            *record.borrow_mut() = Some((
                _cx.entity().downgrade(),
                items
                    .iter()
                    .filter_map(|item| {
                        if let PopupMenuItem::Item {
                            label,
                            action: Some(action),
                            ..
                        } = item
                        {
                            Some((label.clone(), action.boxed_clone()))
                        } else {
                            None
                        }
                    })
                    .collect(),
            ))
        });
        let mut menu = menu;
        for (index, item) in items.into_iter().enumerate() {
            if index == 2 {
                menu = menu.separator();
            }
            menu = menu.item(item);
        }
        menu
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn code2810_tree_value_and_match_styles_follow_light_dark_and_high_contrast(
        cx: &mut TestAppContext,
    ) {
        let (code, v, _temp) = super::super::view_tests::fixture(
            cx,
            "data.json",
            "{\"title\":\"Account\",\"group\":{\"other\":\"Account\"}}",
        );
        code.update_in(v, |s, w, cx| {
            s.set_view(View::Tree, w, cx);
            let target = s.find_target(cx.entity(), cx);
            target.set_query("Account", w, cx);
        });
        super::super::view_tests::settle(v);
        for theme in [brand::ThemeId::Okilum, brand::ThemeId::HighContrast] {
            for mode in [
                gpui_component::ThemeMode::Light,
                gpui_component::ThemeMode::Dark,
            ] {
                v.update(|_, cx| {
                    cx.set_global(brand::ThemeChoice(theme));
                    gpui_component::Theme::change(mode, None, cx);
                    brand::apply_theme(cx);
                });
                code.read_with(v, |s, cx| {
                    let doc = s.document.as_ref().unwrap();
                    assert_eq!(
                        s.structure.match_node,
                        Some(1),
                        "positive control: the actual query selected title"
                    );
                    let styles = s.row_highlights(1, &doc.row_label(1), cx);
                    let string = cx
                        .theme()
                        .highlight_theme
                        .style
                        .syntax
                        .style("string")
                        .unwrap()
                        .color
                        .expect("real syntax token");
                    assert!(
                        styles.iter().any(|(_, h)| h.color == Some(string)),
                        "string value retains the syntax color"
                    );
                    assert!(
                        styles.iter().any(|(_, h)| h.color == Some(string)
                            && h.background_color == Some(brand::reader_palette(cx).find_current)),
                        "current-match amber combines with syntax, without replacing it"
                    );
                    let styles = s.row_highlights(2, &doc.row_label(2), cx);
                    assert!(
                        styles
                            .iter()
                            .any(|(_, h)| h.color == Some(brand::reader_palette(cx).text_faint)),
                        "counts use the theme count token"
                    );
                });
            }
        }
    }

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
        assert_eq!(view.visible, vec![1, 5]); // top-level keys; hidden document root
        view.all(true, &doc);
        assert_eq!(view.visible, vec![1, 5]);
        view.toggle(1, &doc);
        assert_eq!(view.visible.len(), 4);
        view.all(false, &doc);
        assert_eq!(view.visible.len(), doc.structure.len() - 1);
        assert_eq!(view.visible.len(), 5);
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
                visible.len() >= 2,
                "positive control: multiple structure rows are drawn"
            );
            for (row_selector, label_selector) in [
                ("data-structure-row-1", "data-node-label-1"),
                ("data-structure-row-3", "data-node-label-3"),
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

#[cfg(test)]
type MenuProbe = (WeakEntity<PopupMenu>, Vec<(SharedString, Box<dyn Action>)>);
#[cfg(test)]
thread_local! {
    pub(super) static TREE_MENU: std::cell::RefCell<Option<MenuProbe>> = const {std::cell::RefCell::new(None)};
}

#[cfg(feature = "data-view-native-harness")]
thread_local! {static NATIVE_ROW_BUILDS:std::cell::Cell<usize>=const {std::cell::Cell::new(0)};}
#[cfg(feature = "data-view-native-harness")]
pub(super) fn native_row_builds() -> usize {
    NATIVE_ROW_BUILDS.with(std::cell::Cell::get)
}

#[cfg(test)]
thread_local! {
    static ACTION_BUILDS: std::cell::Cell<usize> = const {std::cell::Cell::new(0)};
}
#[cfg(test)]
pub(super) fn action_builds() -> usize {
    ACTION_BUILDS.with(std::cell::Cell::get)
}

#[cfg(test)]
thread_local! {
    pub(super) static LABEL_WEIGHTS: std::cell::RefCell<Vec<FontWeight>> = const {std::cell::RefCell::new(Vec::new())};
}

#[cfg(test)]
thread_local! {
    pub(super) static MARK_PAINTS: std::cell::RefCell<Vec<(u8,Hsla)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn paint_find_quad(window: &mut Window, bounds: Bounds<Pixels>, color: Hsla, _role: u8) {
    window.paint_quad(fill(bounds, color));
    #[cfg(test)]
    MARK_PAINTS.with(|paints| paints.borrow_mut().push((_role, color)));
}
