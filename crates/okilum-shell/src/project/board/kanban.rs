//! Fixed-width, independently virtualised lanes backed by the List projection.
use super::*;
use list::{Prepared, Row};

const CARD_PITCH: f32 = 88.;
const QUEUE_LIMIT: usize = 8;

/// Scroll belongs to the project view, including when its elements are hidden.
pub(super) struct State {
    pub lanes: [gpui::UniformListScrollHandle; 5],
    horizontal: ScrollHandle,
    queue: ScrollHandle,
}
impl State {
    pub(super) fn reset_scroll(&self) {
        for lane in &self.lanes {
            lane.0
                .borrow()
                .base_handle
                .set_offset(point(px(0.), px(0.)));
        }
        self.queue.set_offset(point(px(0.), px(0.)));
        self.horizontal.set_offset(point(px(0.), px(0.)));
    }
}
impl Default for State {
    fn default() -> Self {
        Self {
            lanes: std::array::from_fn(|_| gpui::UniformListScrollHandle::new()),
            horizontal: ScrollHandle::new(),
            queue: ScrollHandle::new(),
        }
    }
}

impl Board {
    pub(super) fn render_kanban(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = brand::palette(cx);
        let prepared = self.prepare_list();
        let binding = &prepared.binding;
        self.columns
            .observe(prepared.counts, prepared.sources_ready);
        let mut root = v_flex()
            .id("project-board")
            .debug_selector(|| "project-board".into())
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .min_w_0()
            .p_3()
            .gap_2()
            .bg(p.canvas)
            .text_color(p.text)
            .text_size(px(13.))
            .line_height(px(18.))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.filter_key(event, window, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    let issue = this.selected.take().is_some();
                    let local = this.selected_local.take().is_some();
                    if issue || local {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }
            }))
            .child(self.filter_bar(&prepared.projection, window, cx));
        if binding.repository.is_empty() {
            root = root.child(div().debug_selector(|| "board-local".into()).text_size(px(12.))
                .child("Local Board · PR and CI links are inactive. Add Repository in Manage board → Board details when needed."));
        }
        if let Some(waiting) = self.render_waiting(cx) {
            root = root.child(waiting);
        }
        if let Some(attention) = self.attention(binding, cx) {
            root = root.child(attention);
        }
        if let Some(delivery) = self.delivery_summary(binding, cx) {
            root = root.child(delivery);
        }
        if let Some(notes) = self.render_project_notes(binding, cx) {
            root = root.child(notes);
        }
        if let Some(notice) = &self.notice {
            root = root.child(notice.clone());
        }
        if self.filter.is_active()
            && (!self.binding_loading || self.binding_read.is_some())
            && (!self.local_loading || self.local_cards.last_success.is_some())
            && (!self.snapshot.busy() || self.snapshot.forgejo.last_success.is_some())
            && prepared.counts.iter().all(|n| *n == 0)
        {
            return root.child(self.filter_empty(cx)).into_any_element();
        }
        let mut empty = h_flex()
            .debug_selector(|| "board-empty-columns".into())
            .flex_wrap()
            .gap_2();
        let mut collapsed = 0;
        let mut lanes = h_flex()
            .id("board-lanes-scroll")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .items_stretch()
            .gap_3()
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&self.kanban.horizontal);
        for stage in [
            Column::Working,
            Column::Checks,
            Column::Ready,
            Column::Queue,
            Column::Verified,
        ] {
            if stage == Column::Verified && !self.verified_open {
                continue;
            }
            let count = prepared.counts[stage as usize];
            if stage != Column::Verified && self.columns.collapsed(stage, count) {
                empty = empty.child(self.expand_empty_column(stage, cx));
                collapsed += 1;
                continue;
            }
            let (tone, icon) = appearance::stage(stage, cx);
            let mut header = h_flex()
                .h(px(28.))
                .flex_none()
                .gap_2()
                .text_size(px(13.))
                .line_height(px(18.))
                .child(appearance::icon(icon, tone))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(list::label(stage)),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(p.text_muted)
                        .child(count.to_string()),
                );
            if stage == Column::Queue {
                header = header.child(
                    div()
                        .debug_selector(|| "board-queue-order-hint".into())
                        .ml_auto()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.))
                        .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                        .text_color(p.text_muted)
                        .child("от Олега сначала"),
                );
            }
            if count == 0 && self.columns.known_empty(stage) {
                header = header.child(self.collapse_empty_column(stage, cx));
            }
            let mut lane = v_flex()
                .id(("board-lane", stage as usize))
                .debug_selector(move || format!("board-lane-{stage:?}"))
                .w(px(if stage == Column::Queue { 240. } else { 268. }))
                .flex_none()
                .h_full()
                .min_h_0()
                .gap_2()
                .child(header);
            if count == 0 {
                if self.columns.known_empty(stage) {
                    lane = lane.child(
                        div()
                            .text_size(px(12.))
                            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                            .text_color(p.text_muted)
                            .child("No cards"),
                    );
                }
            } else if stage == Column::Queue {
                let mut body = v_flex()
                    .id("board-queue-scroll")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .gap_2()
                    .overflow_y_scroll()
                    .restrict_scroll_to_axis()
                    .track_scroll(&self.kanban.queue);
                for (index, row) in prepared.groups[stage as usize]
                    .iter()
                    .take(QUEUE_LIMIT)
                    .enumerate()
                {
                    body = body.child(self.queue_row(*row, index, &prepared, window, cx));
                }
                if count > QUEUE_LIMIT {
                    body = body.child(
                        Button::new("board-queue-more")
                            .debug_selector(|| "board-queue-more".into())
                            .ghost()
                            .small()
                            .w_full()
                            .h(px(32.))
                            .text_size(px(12.))
                            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                            .label(format!("＋ ещё {} · открыть списком", count - QUEUE_LIMIT))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_queue_list(false, window, cx)
                            })),
                    );
                }
                if let Some(problem) = binding.card_problems.first() {
                    body = body.child(
                        Button::new("board-queue-problems")
                            .debug_selector(|| "board-card-problem-0".into())
                            .ghost()
                            .small()
                            .w_full()
                            .text_color(p.warning)
                            .label(format!("Needs attention · {}", binding.card_problems.len()))
                            .tooltip(format!("{}: {}", problem.field, problem.detail))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_queue_list(true, window, cx)
                            })),
                    );
                }
                body = body.child(self.add_card_button(cx));
                lane = lane.child(body);
            } else {
                let rows = prepared.clone();
                lane = lane.child(
                    gpui::uniform_list(
                        ("board-stage-list", stage as usize),
                        count,
                        cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                            range
                                .map(|index| {
                                    div().h(px(CARD_PITCH)).pb(px(4.)).child(this.kanban_row(
                                        rows.groups[stage as usize][index],
                                        stage,
                                        &rows,
                                        window,
                                        cx,
                                    ))
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.kanban.lanes[stage as usize])
                    .map(|mut list| {
                        list.style().restrict_scroll_to_axis = Some(true);
                        list
                    })
                    .flex_1()
                    .min_h_0()
                    .w_full(),
                );
            }
            lanes = lanes.child(lane);
        }
        if collapsed > 0 {
            root = root.child(empty);
        }
        if prepared.sources_ready && prepared.counts.iter().all(|n| *n == 0) {
            root = root.child(div().debug_selector(|| "board-empty-state".into()).child(
                if self.filter.is_active() {
                    "No cards for this executor. Expand a column to view it."
                } else {
                    "No cards yet. Add a card to get started, or expand a column."
                },
            ));
            root = root.child(self.add_card_button(cx));
        }
        let verified = prepared.counts[Column::Verified as usize];
        let toggle = Button::new("board-verified-toggle")
            .debug_selector(|| "board-verified-toggle".into())
            .ghost()
            .w(px(34.))
            .flex_1()
            .min_h_0()
            .items_start()
            .p_0()
            .pt(px(6.))
            .accessibility_label(if self.verified_open {
                "Collapse verified results"
            } else {
                "Expand verified results"
            })
            .tooltip(format!("Проверено · {verified}"))
            .child(
                v_flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(12.))
                    .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                    .child(appearance::icon("circle-check", p.success))
                    .child(verified.to_string())
                    .child(appearance::icon(
                        if self.verified_open {
                            "chevron-left"
                        } else {
                            "chevron-right"
                        },
                        p.text_muted,
                    )),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.verified_open = !this.verified_open;
                cx.notify();
            }));
        let mut end = v_flex().w(px(36.)).flex_none().gap_2();
        end = end
            .child(
                Button::new("board-manage")
                    .debug_selector(|| "board-manage".into())
                    .ghost()
                    .small()
                    .child(appearance::icon("settings-2", p.text_muted))
                    .tooltip("Manage board")
                    .accessibility_label("Manage board")
                    .disabled(self.editor_loading)
                    .on_click(cx.listener(|this, _, window, cx| this.open_management(window, cx))),
            )
            .child(self.source_indicator_for(prepared.source_problem.clone(), cx))
            .child(toggle);
        root = root.child(
            h_flex()
                .debug_selector(|| "board-columns".into())
                .w_full()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .items_stretch()
                .gap_2()
                .child(lanes)
                .child(end),
        );
        // Variable-height actions do not change virtualised card geometry.
        let details = self
            .selected
            .and_then(|n| prepared.issue_indices.get(&n))
            .map(|i| {
                let scope = &binding.issues[*i];
                self.card_with_mapping(
                    scope,
                    self.delivery_column(scope),
                    binding,
                    prepared.mappings.get(&scope.number).copied().flatten(),
                    window,
                    cx,
                )
            })
            .or_else(|| {
                self.selected_local
                    .as_ref()
                    .and_then(|id| prepared.local_indices.get(id))
                    .map(|i| self.local_card(&self.local_cards.value.cards[*i], window, cx))
            });
        if let Some(details) = details {
            root = root.child(
                div()
                    .id("board-kanban-details")
                    .debug_selector(|| "board-kanban-details".into())
                    .h(px(260.))
                    .flex_none()
                    .overflow_y_scroll()
                    .child(details),
            );
        }
        root.child(
            div()
                .debug_selector(|| "board-live-status".into())
                .h(px(20.))
                .flex_none()
                .text_size(px(12.))
                .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                .text_color(p.text_muted)
                .truncate()
                .child(
                    div()
                        .when(self.waiting.problem.is_some(), |line| {
                            line.debug_selector(|| "board-waiting-problem".into())
                        })
                        .child(
                            self.waiting
                                .problem
                                .clone()
                                .or_else(|| {
                                    problem_line(
                                        source_problem(
                                            "Forgejo",
                                            &self.snapshot.forgejo,
                                            30,
                                            now(),
                                        )
                                        .into_iter()
                                        .collect(),
                                    )
                                })
                                .unwrap_or_default(),
                        ),
                ),
        )
        .into_any_element()
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(in crate::project) fn performance_horizontal_offset(&self) -> Point<Pixels> {
        self.kanban.horizontal.offset()
    }
    fn open_queue_list(&mut self, problem: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.list.preferences.mode = list::Mode::List;
        self.list.preferences.collapsed[Column::Queue as usize] = false;
        let prepared = self.prepare_list();
        let index = prepared
            .rows
            .iter()
            .position(|row| {
                if problem {
                    matches!(row, Row::Problem(_))
                } else {
                    matches!(row, Row::Group(Column::Queue))
                }
            })
            .unwrap_or(0);
        self.list
            .scroll
            .scroll_to_item_strict(index, gpui::ScrollStrategy::Top);
        self.save_presentation();
        self.focus.focus(window, cx);
        cx.notify();
    }
    fn kanban_row(
        &self,
        row: Row,
        stage: Column,
        prepared: &Prepared,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            Row::Issue(i) => self.compact_card(
                &prepared.binding.issues[i],
                stage,
                &prepared.binding,
                window,
                cx,
            ),
            Row::Local(i) => self.compact_local_card(&self.local_cards.value.cards[i], window, cx),
            Row::Problem(_) | Row::Group(_) | Row::WaitingGroup | Row::Waiting(_) => unreachable!("non-Queue issue lane"),
        }
    }
    fn queue_row(
        &self,
        row: Row,
        index: usize,
        prepared: &Prepared,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        #[cfg(all(target_os = "linux", feature = "project-ui-harness"))]
        let _work = super::super::performance::Work::start(1);
        self.card_rendered();
        let p = brand::palette(cx);
        let (title, id, issue, number, local) = match row {
            Row::Issue(i) => {
                let scope = &prepared.binding.issues[i];
                let issue = self.snapshot.forgejo.value.get(&scope.number);
                (
                    issue
                        .map(|i| i.title.clone())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| scope.expected.clone()),
                    format!("#{}", scope.number),
                    issue,
                    Some(scope.number),
                    None,
                )
            }
            Row::Local(i) => {
                let c = &self.local_cards.value.cards[i];
                (
                    c.title.clone(),
                    "Local".into(),
                    None,
                    None,
                    Some(c.id.clone()),
                )
            }
            Row::Problem(i) => {
                let problem = &prepared.binding.card_problems[i];
                return div()
                    .debug_selector(move || format!("board-card-problem-{i}"))
                    .h(px(30.))
                    .text_size(px(12.))
                    .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                    .text_color(p.warning)
                    .truncate()
                    .child(format!(
                        "{} · {}: {}",
                        problem.number.map(|n| format!("#{n}")).unwrap_or_default(),
                        problem.field,
                        problem.detail
                    ))
                    .into_any_element();
            }
            Row::Group(_) | Row::WaitingGroup | Row::Waiting(_) => unreachable!(),
        };
        let local_title = local.clone();
        let local_root = local.clone();
        let (button, focus) = controls::queue_disclosure(("board-queue-row", index), window, cx);
        #[cfg(test)]
        if let Some(number) = number {
            self.card_focus.borrow_mut().insert(number, focus);
        }
        #[cfg(not(test))]
        let _ = focus;
        let mini = button
            .rounded_none()
            .debug_selector(move || format!("board-queue-row-{index}"))
            .h(px(30.))
            .flex_none()
            .px(px(4.))
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .text_size(px(12.))
            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
            .cursor_pointer()
            .child(row_bits::type_tag(issue, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .debug_selector(move || {
                        number
                            .map(|n| format!("board-expand-{n}"))
                            .or_else(|| {
                                local_title
                                    .as_ref()
                                    .map(|id| format!("board-local-expand-{id}"))
                            })
                            .unwrap_or_else(|| "board-queue-local-title".into())
                    })
                    .child(title),
            )
            .child(div().flex_none().text_color(p.text_muted).child(id))
            .on_click(cx.listener(move |this, event, window, cx| {
                if let Some(number) = number {
                    this.selected = (this.selected != Some(number)).then_some(number);
                    this.selected_local = None;
                } else if let Some(id) = &local {
                    this.selected = None;
                    this.selected_local =
                        (this.selected_local.as_ref() != Some(id)).then(|| id.clone());
                }
                if !matches!(event, gpui::ClickEvent::Keyboard(_)) {
                    this.focus.focus(window, cx);
                }
                cx.notify();
            }))
            .into_any_element();
        let row = if let Some(id) = local_root {
            div()
                .debug_selector(move || format!("board-local-card-{id}"))
                .w_full()
                .min_w_0()
                .h(px(30.))
                .flex_none()
                .child(mini)
                .into_any_element()
        } else {
            mini
        };
        self.track_card_layout(row, cx)
    }
}
