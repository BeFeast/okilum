//! Erase each control/row before composing the toolbar. Large fluent builders
//! otherwise keep overlapping debug stack frames on Windows' small main stack.
use super::*;

impl CodePreview {
    #[inline(never)]
    pub(super) fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(feature = "data-view-native-harness")]
        native_trace("toolbar.begin");
        let tools = self.tools(cx);
        let mut view = v_flex().gap_1().pb_2().child(tools);
        if self.paged
            || self
                .document
                .as_ref()
                .is_some_and(|doc| !doc.structure.is_empty())
        {
            view = view.child(self.paged_toolbar(cx));
        }
        #[cfg(feature = "data-view-native-harness")]
        native_trace("toolbar.end");
        view.into_any_element()
    }

    #[inline(never)]
    fn tools(&self, cx: &mut Context<Self>) -> AnyElement {
        let controls = vec![
            self.find_control(cx),
            self.fold_control(true, cx),
            self.fold_control(false, cx),
            self.copy_control(false, cx),
            self.copy_control(true, cx),
            self.external_control(),
        ];
        h_flex()
            .gap_1()
            .flex_wrap()
            .children(controls)
            .into_any_element()
    }

    #[inline(never)]
    fn find_control(&self, cx: &mut Context<Self>) -> AnyElement {
        Button::new("data-find")
            .small()
            .ghost()
            .label("Find")
            .on_click(cx.listener(|this, _, window, cx| {
                if !this.open_paged_find(window, cx) {
                    if let Some(input) = this.input() {
                        input.update(cx, |state, cx| {
                            state.open_search(false, cx);
                            state.focus(window, cx);
                        });
                    }
                }
            }))
            .into_any_element()
    }

    #[inline(never)]
    fn fold_control(&self, folded: bool, cx: &mut Context<Self>) -> AnyElement {
        let id = if folded {
            "data-fold-all"
        } else {
            "data-unfold-all"
        };
        Button::new(id)
            .small()
            .ghost()
            .label(if folded { "Fold all" } else { "Unfold all" })
            .disabled(self.document.is_none())
            .debug_selector(move || id.into())
            .on_click(cx.listener(move |this, _, _, cx| this.fold_all(folded, cx)))
            .into_any_element()
    }

    #[inline(never)]
    fn copy_control(&self, node: bool, cx: &mut Context<Self>) -> AnyElement {
        let id = if node {
            "data-copy-node"
        } else {
            "data-copy-selection"
        };
        let disabled = if node {
            self.document.is_none() || language(&self.rel) == Some("text")
        } else {
            self.structure_view
        };
        Button::new(id)
            .small()
            .ghost()
            .label(if node { "Copy node" } else { "Copy selection" })
            .disabled(disabled)
            .debug_selector(move || id.into())
            .on_click(cx.listener(move |this, _, _, cx| {
                if node {
                    this.copy_node(cx);
                } else if let Some(input) = this.input() {
                    let state = input.read(cx);
                    let range = state.selected_range();
                    if !range.is_empty() {
                        let text = state.text().slice(range).to_string();
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                }
            }))
            .into_any_element()
    }

    #[inline(never)]
    fn external_control(&self) -> AnyElement {
        let root = self.root.clone();
        let rel = self.rel.clone();
        Button::new("data-open-external")
            .small()
            .ghost()
            .label("Open in default editor")
            .on_click(move |_, window, cx| {
                reader_files::run(reader_files::FileAction::Open, &root, &rel, window, cx);
            })
            .into_any_element()
    }

    #[inline(never)]
    fn paged_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(feature = "data-view-native-harness")]
        native_trace("paged-toolbar.begin");
        let modes = self.mode_row(cx);
        #[cfg(feature = "data-view-native-harness")]
        native_trace("paged-toolbar.end");
        v_flex()
            .gap_1()
            .child(modes)
            .when(
                !self.structure_view && self.paged && self.document.is_some(),
                |view| view.child(self.page_row(cx)),
            )
            .when(self.structure_view || self.paged, |view| {
                view.child(self.search_row(cx))
            })
            .into_any_element()
    }

    #[inline(never)]
    fn mode_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let structure = self.mode_control(true, cx);
        let source = self.mode_control(false, cx);
        h_flex()
            .gap_1()
            .children([structure, source])
            .into_any_element()
    }

    #[inline(never)]
    fn mode_control(&self, structure: bool, cx: &mut Context<Self>) -> AnyElement {
        let id = if structure {
            "data-structure"
        } else {
            "data-source"
        };
        Button::new(id)
            .small()
            .ghost()
            .label(if structure { "Tree" } else { "Source" })
            .selected(self.structure_view == structure)
            .disabled(
                structure
                    && self
                        .document
                        .as_ref()
                        .is_none_or(|doc| doc.structure.is_empty()),
            )
            .debug_selector(move || id.into())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.requested_tree = Some(structure);
                this.structure_view = structure;
                this.show_match(window, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    #[inline(never)]
    fn page_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let doc = self.document.as_ref().unwrap();
        let range = &doc.pages[self.page];
        let label = div()
            .debug_selector(|| "data-page-range".into())
            .text_sm()
            .child(format!(
                "Long-line view · page {}/{} · bytes {}–{} of {}",
                self.page + 1,
                doc.pages.len(),
                range.start + 1,
                range.end,
                doc.text.len()
            ))
            .into_any_element();
        let previous = self.page_control(false, cx);
        let next = self.page_control(true, cx);
        h_flex()
            .gap_2()
            .items_center()
            .flex_wrap()
            .children([label, previous, next])
            .into_any_element()
    }

    #[inline(never)]
    fn page_control(&self, next: bool, cx: &mut Context<Self>) -> AnyElement {
        let id = if next {
            "data-next-page"
        } else {
            "data-prev-page"
        };
        let disabled = if next {
            self.document
                .as_ref()
                .is_none_or(|doc| self.page + 1 >= doc.pages.len())
        } else {
            self.page == 0
        };
        Button::new(id)
            .small()
            .ghost()
            .label(if next { "Next" } else { "Previous" })
            .disabled(disabled)
            .on_click(cx.listener(move |this, _, window, cx| {
                let page = if next {
                    this.page + 1
                } else {
                    this.page.saturating_sub(1)
                };
                this.show_page(page, window, cx);
            }))
            .into_any_element()
    }

    #[inline(never)]
    fn search_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let input = div()
            .w_64()
            .debug_selector(|| "data-whole-file-find".into())
            .child(Input::new(&self.find).small())
            .into_any_element();
        let label = div()
            .text_sm()
            .child(if self.searching {
                "Searching…".into()
            } else {
                format!(
                    "{} / {} matches",
                    if self.matches.is_empty() {
                        0
                    } else {
                        self.match_index + 1
                    },
                    self.matches.len()
                )
            })
            .into_any_element();
        let previous = self.match_control(false, cx);
        let next = self.match_control(true, cx);
        h_flex()
            .gap_2()
            .items_center()
            .children([input, label, previous, next])
            .into_any_element()
    }

    #[inline(never)]
    fn match_control(&self, next: bool, cx: &mut Context<Self>) -> AnyElement {
        let id = if next {
            "data-next-match"
        } else {
            "data-prev-match"
        };
        Button::new(id)
            .small()
            .ghost()
            .label(if next { "Next match" } else { "Previous match" })
            .disabled(self.matches.is_empty())
            .on_click(cx.listener(move |this, _, window, cx| {
                if !this.matches.is_empty() {
                    this.match_index = if next {
                        (this.match_index + 1) % this.matches.len()
                    } else {
                        (this.match_index + this.matches.len() - 1) % this.matches.len()
                    };
                    this.show_match(window, cx);
                }
            }))
            .into_any_element()
    }
}
