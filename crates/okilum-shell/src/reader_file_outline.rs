//! File structure in the existing right panel; no Reader/sidebar state or hooks.
use super::super::*;
use gpui_component::input::{EditorState, InputEvent};
use std::{collections::BTreeSet, sync::Arc};
#[path = "reader_file_outline/parse.rs"]
mod parse;

pub(crate) struct View {
    code: Option<Entity<reader_code_file::CodePreview>>,
    input: Option<Entity<EditorState>>,
    language: Option<&'static str>,
    code_observer: Option<Subscription>,
    input_observers: Vec<Subscription>,
    task: Option<Task<()>>,
    generation: u64,
    data: Arc<parse::Outline>,
    expanded: BTreeSet<usize>,
    visible: Vec<usize>,
    current: Option<usize>,
    list: ListState,
}

impl View {
    pub(crate) fn new() -> Self {
        Self {
            code: None,
            input: None,
            language: None,
            code_observer: None,
            input_observers: Vec::new(),
            task: None,
            generation: 0,
            data: Arc::default(),
            expanded: BTreeSet::new(),
            visible: Vec::new(),
            current: None,
            list: ListState::new(0, ListAlignment::Top, px(200.)),
        }
    }

    /// Called only when the right panel is actually shown. Loading, large and
    /// unsupported files cost no parse; no text is copied on scroll notifications.
    pub(crate) fn bind(view: &Entity<Self>, reader: &Reader, cx: &mut App) {
        let code = reader
            .file_preview
            .as_ref()
            .and_then(|file| file.code.clone());
        let language = reader
            .file_preview
            .as_ref()
            .and_then(|file| reader_code_file::language(&file.rel))
            .filter(|lang| parse::supported(lang));
        let input = language.and_then(|_| {
            reader
                .source_input()
                .or_else(|| code.as_ref().and_then(|code| code.read(cx).input()))
        });
        view.update(cx, |this, cx| {
            if this.code != code || this.language != language {
                this.code = code.clone();
                this.language = language;
                this.code_observer = code.as_ref().filter(|_| language.is_some()).map(|code| {
                    cx.observe(code, |this, code, cx| {
                        let input = code.read(cx).input();
                        // A visible writable editor owns its own text and observer.
                        if this.input.is_none() || this.input == input {
                            this.attach(input, true, cx);
                        }
                    })
                });
                this.attach(input, true, cx);
            } else if this.input != input {
                this.attach(input, true, cx);
            }
        });
    }

    fn attach(&mut self, input: Option<Entity<EditorState>>, force: bool, cx: &mut Context<Self>) {
        if !force && self.input == input {
            return;
        }
        self.input_observers.clear();
        self.input = input.clone();
        if let Some(input) = input {
            // The editor publishes its laid-out viewport on notify, including
            // scrolls that do not rerender the Reader. Only a changed section
            // notifies this view and its cached parent panel.
            self.input_observers
                .push(cx.observe(&input, |this, _, cx| this.sync_current(cx)));
            self.input_observers
                .push(cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.reparse(cx);
                    }
                }));
        }
        self.reparse(cx);
    }

    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        if self.code.is_none() && self.input.is_none() {
            return;
        }
        self.code = None;
        self.language = None;
        self.code_observer = None;
        self.attach(None, true, cx);
    }

    fn reparse(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.task = None;
        self.data = Arc::default();
        self.visible.clear();
        self.expanded.clear();
        self.current = None;
        self.list.reset(0);
        cx.notify();
        let Some((input, language)) = self.input.as_ref().zip(self.language) else {
            return;
        };
        // Check the borrowed rope before materializing it; large inputs never copy.
        if input.read(cx).text().len() > parse::MAX_BYTES {
            return;
        }
        let source = input.read(cx).text().clone();
        let generation = self.generation;
        self.task = Some(cx.spawn(async move |this, cx| {
            // Coalesce edits and Pretty/Raw transitions before entering the parser.
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let data = cx
                .background_executor()
                .spawn(async move { parse::parse(language, &source.to_string()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.data = Arc::new(data);
                this.refresh_rows(cx);
            });
        }));
    }

    fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        let top = self.list.logical_scroll_top();
        let anchor = self.visible.get(top.item_ix).copied();
        self.visible = self.data.visible(&self.expanded);
        self.list
            .reset_with_uniform_height(self.visible.len(), px(OUTLINE_ROW_HEIGHT));
        if let Some(row) = anchor.and_then(|ix| self.visible.binary_search(&ix).ok()) {
            self.list.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: top.offset_in_item,
            });
        }
        self.sync_current(cx);
        cx.notify();
    }

    fn sync_current(&mut self, cx: &mut Context<Self>) {
        let current = self.section(cx);
        if self.current != current {
            self.current = current;
            cx.notify();
        }
    }

    fn section(&self, cx: &App) -> Option<usize> {
        self.input
            .as_ref()
            .and_then(|input| input.read(cx).top_visible_line())
            .and_then(|line| self.data.current(line, &self.visible))
    }

    pub(super) fn has_entries(&self) -> bool {
        !self.visible.is_empty()
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        if !self.expanded.remove(&ix) {
            self.expanded.insert(ix);
        }
        self.refresh_rows(cx);
    }

    fn jump(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.data.entries.get(ix) else {
            return;
        };
        if let Some(input) = &self.input {
            input.update(cx, |input, cx| {
                input.scroll_line_to_top(entry.line, cx);
                input.focus_handle(cx).focus(window, cx);
            });
        }
    }
}

impl Render for View {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("file-outline")
            .debug_selector(|| "file-outline-list".into())
            .flex_none()
            .h(px(
                self.visible.len() as f32 * OUTLINE_ROW_HEIGHT + 2. * OUTLINE_PADDING_Y
            ))
            .max_h(relative(0.6))
            .py(px(OUTLINE_PADDING_Y))
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, row: usize, _, cx| {
                        let Some(&ix) = this.visible.get(row) else {
                            return div().into_any_element();
                        };
                        let entry = &this.data.entries[ix];
                        let p = brand::palette(cx);
                        let hover = brand::reader_palette(cx).hover;
                        let current = this.current == Some(ix);
                        h_flex()
                            .id(("file-outline-row", ix))
                            .debug_selector(move || format!("file-outline-row-{ix}"))
                            .flex_none()
                            .h(px(OUTLINE_ROW_HEIGHT))
                            .items_center()
                            .pl(px(14. + entry.depth.min(12) as f32 * 12.))
                            .pr_3()
                            .border_l_2()
                            .border_color(if current {
                                p.accent
                            } else {
                                gpui::transparent_black()
                            })
                            .text_color(if current { p.text } else { p.text_muted })
                            .when(current, |row| row.font_weight(FontWeight::MEDIUM))
                            .overflow_hidden()
                            .cursor_pointer()
                            .hover(move |row| row.bg(hover))
                            .child(
                                div()
                                    .id(("file-outline-toggle", ix))
                                    .w(px(16.))
                                    .flex_none()
                                    .when(entry.children, |toggle| {
                                        toggle
                                            .debug_selector(move || {
                                                format!("file-outline-toggle-{ix}")
                                            })
                                            .child(
                                                Icon::new(if this.expanded.contains(&ix) {
                                                    IconName::ChevronDown
                                                } else {
                                                    IconName::ChevronRight
                                                })
                                                .xsmall(),
                                            )
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.toggle(ix, cx);
                                            }))
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .debug_selector(move || format!("file-outline-text-{ix}"))
                                    .child(entry.label.clone()),
                            )
                            .on_click(
                                cx.listener(move |this, _, window, cx| this.jump(ix, window, cx)),
                            )
                            .into_any_element()
                    }),
                )
                .size_full(),
            )
    }
}

#[cfg(test)]
#[path = "reader_file_outline/tests.rs"]
mod tests;
