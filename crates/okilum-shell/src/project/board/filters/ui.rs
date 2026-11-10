//! Native controls over the shared filter model.
use super::*;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};

pub(in super::super) struct Ui {
    pub search: Entity<InputState>,
    save: Option<Task<()>>,
    directory: Option<PathBuf>,
}
impl Ui {
    pub fn new(project: &Path, window: &mut Window, cx: &mut Context<Board>) -> (Model, Self) {
        #[cfg(test)]
        let directory = cx
            .try_global::<reader_history::TestSessionDirectory>()
            .map(|d| d.0.clone());
        #[cfg(not(test))]
        let directory = reader_history::state_directory().ok();
        let model = directory
            .as_ref()
            .map(|d| Model::load(d, project))
            .unwrap_or_default();
        let search = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Найти · /");
            input.set_value(model.search.clone(), window, cx);
            input
        });
        cx.subscribe(&search, |board, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let search = input.read(cx).value().to_string();
                if board.filter.search != search {
                    board.filter.search = search;
                    board.filter_changed(cx);
                }
            }
        })
        .detach();
        (
            model,
            Self {
                search,
                directory,
                save: None,
            },
        )
    }
}
pub(in super::super) use Ui as Controls;

impl Board {
    pub(in super::super) fn filter_changed(&mut self, cx: &mut Context<Self>) {
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.kanban.reset_scroll();
        self.list
            .scroll
            .0
            .borrow()
            .base_handle
            .set_offset(point(px(0.), px(0.)));
        if let Some(directory) = self.filter_ui.directory.clone() {
            let model = self.filter.clone();
            let area = self.area.clone();
            let previous = self.filter_ui.save.take();
            // Serialize writes off the UI thread, including multiple edits in
            // one event turn. A delayed older write cannot overwrite newer text.
            self.filter_ui.save = Some(cx.spawn(async move |_, cx| {
                if let Some(previous) = previous {
                    previous.await;
                }
                cx.background_executor()
                    .spawn(async move {
                        if let Err(error) = model.save(&directory, &area) {
                            diagnostic_error!("Board filter preferences: {error:#}");
                        }
                    })
                    .await;
            }));
        }
        cx.notify();
    }
    pub(in super::super) fn filter_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.filter_ui.search.read(cx).ime_composing() {
            return;
        }
        let focused = self
            .filter_ui
            .search
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if event.keystroke.key == "/"
            && !focused
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.platform
            && !event.keystroke.modifiers.alt
        {
            self.filter_ui
                .search
                .update(cx, |input, cx| input.focus(window, cx));
            cx.stop_propagation();
            window.prevent_default();
        } else if event.keystroke.key == "escape" && (focused || !self.filter.search.is_empty()) {
            self.filter.search.clear();
            self.filter_ui
                .search
                .update(cx, |input, cx| input.set_value("", window, cx));
            window.focus(&self.focus, cx);
            self.filter_changed(cx);
            cx.stop_propagation();
            window.prevent_default();
        }
    }
    pub(in super::super) fn reset_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter = Model::default();
        self.filter_ui
            .search
            .update(cx, |input, cx| input.set_value("", window, cx));
        window.focus(&self.focus, cx);
        self.filter_changed(cx);
    }
    pub(in super::super) fn filter_bar(
        &self,
        projection: &Arc<Projection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = brand::palette(cx);
        let mut chips = h_flex().min_w_0().flex_wrap().items_center().gap_2();
        for (index, chip) in Chip::ALL.into_iter().enumerate() {
            let count = projection.chip_counts[index];
            chips = chips.child(
                controls::compact_option(
                    ("board-chip", index),
                    self.filter.chip == chip,
                    window,
                    cx,
                )
                .debug_selector(move || format!("board-chip-{}", chip.id()))
                .rounded_full()
                .text_size(px(12.))
                .accessibility_label(format!("{}: {count}", chip.label()))
                .child(chip.label())
                .child(
                    div()
                        .debug_selector(move || format!("board-chip-count-{}-{count}", chip.id()))
                        .text_color(colors.text_muted)
                        .child(count.to_string()),
                )
                .on_click(cx.listener(move |board, _, window, cx| {
                    board.filter.chip = chip;
                    window.focus(&board.focus, cx);
                    board.filter_changed(cx);
                })),
            );
        }
        let choices = projection.clone();
        let selected = self.filter.clone();
        let weak = cx.entity().downgrade();
        chips = chips.child(
            Button::new("board-extra-filter")
                .debug_selector(|| "board-extra-filter".into())
                .ghost()
                .small()
                .text_size(px(12.))
                .label("＋ Фильтр")
                .dropdown_menu_with_anchor(Anchor::TopLeft, move |mut menu, _, _| {
                    menu = menu.label("Labels / epics");
                    for label in &choices.labels {
                        let weak = weak.clone();
                        let name = label.clone();
                        menu = menu.item(
                            PopupMenuItem::new(label.clone())
                                .checked(selected.labels.contains(label))
                                .on_click(move |_, _, cx| {
                                    let _ = weak.update(cx, |board, cx| {
                                        if !board.filter.labels.remove(&name) {
                                            board.filter.labels.insert(name.clone());
                                        }
                                        board.filter_changed(cx);
                                    });
                                }),
                        );
                    }
                    menu = menu.separator().label("Agent / executor");
                    for executor in &choices.executors {
                        let weak = weak.clone();
                        let name = executor.clone();
                        menu = menu.item(
                            PopupMenuItem::new(executor.clone())
                                .checked(selected.executor.as_ref() == Some(executor))
                                .on_click(move |_, _, cx| {
                                    let _ = weak.update(cx, |board, cx| {
                                        board.filter.executor = (board.filter.executor.as_ref()
                                            != Some(&name))
                                        .then(|| name.clone());
                                        board.filter_changed(cx);
                                    });
                                }),
                        );
                    }
                    menu.scrollable(true)
                }),
        );
        for (index, name) in self.filter.labels.iter().enumerate() {
            let label = name.clone();
            chips = chips.child(
                Button::new(("board-remove-label", index))
                    .debug_selector(move || format!("board-remove-label-{index}"))
                    .ghost()
                    .small()
                    .rounded_full()
                    .text_size(px(12.))
                    .label(format!("{name} ×"))
                    .accessibility_label(format!("Remove label filter {name}"))
                    .on_click(cx.listener(move |board, _, _, cx| {
                        board.filter.labels.remove(&label);
                        board.filter_changed(cx);
                    })),
            );
        }
        if let Some(executor) = &self.filter.executor {
            chips = chips.child(
                Button::new("board-remove-executor")
                    .ghost()
                    .small()
                    .rounded_full()
                    .debug_selector(|| "board-remove-executor".into())
                    .text_size(px(12.))
                    .label(format!("{executor} ×"))
                    .accessibility_label("Remove executor filter")
                    .on_click(cx.listener(|board, _, _, cx| {
                        board.filter.executor = None;
                        board.filter_changed(cx);
                    })),
            );
        }
        h_flex()
            .debug_selector(|| "board-filter-bar".into())
            .min_w_0()
            .w_full()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(div().flex_none().child(self.view_toggle(window, cx)))
            .child(chips)
            .child(div().flex_1())
            .child(
                div()
                    .debug_selector(|| "board-search".into())
                    .w(px(160.))
                    .min_w_0()
                    .child(
                        Input::new(&self.filter_ui.search)
                            .small()
                            .cleanable(true)
                            .prefix(appearance::icon("search", colors.text_muted)),
                    ),
            )
            .into_any_element()
    }
    pub(in super::super) fn filter_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = brand::palette(cx);
        let line = self
            .waiting
            .problem
            .clone()
            .or_else(|| {
                problem_line(
                    source_problem("Forgejo", &self.snapshot.forgejo, 30, now())
                        .into_iter()
                        .collect(),
                )
            })
            .unwrap_or_default();
        v_flex()
            .debug_selector(|| "board-filter-empty".into())
            .flex_1()
            .min_h_0()
            .relative()
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .text_size(px(12.))
                    .text_color(colors.text_muted)
                    .child("Ничего не найдено ·")
                    .child(
                        Button::new("board-reset-filters")
                            .debug_selector(|| "board-reset-filters".into())
                            .ghost()
                            .small()
                            .label("Сбросить фильтр")
                            .text_size(px(12.))
                            .on_click(
                                cx.listener(|board, _, window, cx| board.reset_filters(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .right_0()
                    .child(self.source_indicator(cx)),
            )
            .child(
                div()
                    .debug_selector(|| "board-live-status".into())
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_8()
                    .h(px(20.))
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.))
                    .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                    .text_color(colors.text_muted)
                    .child(
                        div()
                            .when(self.waiting.problem.is_some(), |line| {
                                line.debug_selector(|| "board-waiting-problem".into())
                            })
                            .child(line),
                    ),
            )
            .into_any_element()
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        // Closing the window must not cancel the final queued preference write.
        if let Some(save) = self.save.take() {
            save.detach();
        }
    }
}
