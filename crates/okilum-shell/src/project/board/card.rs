//! Compact kanban summaries and the existing explicit card disclosure.
use super::*;

impl Board {
    pub(super) fn card_with_mapping(
        &self,
        scope: &okilum_t3::board::Scope,
        column: Column,
        binding: &Binding,
        mapping: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = brand::palette(cx);
        let number = scope.number;
        let issue = self.snapshot.forgejo.value.get(&number);
        let mut card = v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(self.compact_card_control(scope, column, binding, false, window, cx));
        if let Some(problem) = mapping {
            card = card.child(
                div()
                    .debug_selector(move || format!("board-mapping-{number}"))
                    .text_size(px(12.))
                    .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                    .text_color(colors.warning)
                    .child(problem),
            );
        }
        if self.selected == Some(number) {
            card = card.child(
                div()
                    .text_size(px(13.))
                    .line_height(px(18.))
                    .child(scope.expected.clone()),
            );
            if self.delivery.enabled() {
                card = card.child(self.delivery_details(scope, binding, cx));
            }
            let url = binding.issue_url(number);
            let context = self.context(scope);
            let mut actions = h_flex()
                .px_2()
                .pb_2()
                .gap_1()
                .flex_wrap()
                .child(
                    Button::new(("board-open-issue", number as usize))
                        .ghost()
                        .small()
                        .label("Issue")
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
                .child(
                    Button::new(("board-discuss", number as usize))
                        .debug_selector(move || format!("board-discuss-{number}"))
                        .ghost()
                        .small()
                        .label("Discuss")
                        .tooltip("Discuss with Manager · draft")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(Action::Discuss(context.clone()))
                        })),
                );
            for pr in &scope.pull_requests {
                let url = binding.pull_url(*pr);
                actions = actions.child(
                    Button::new(("board-pr", *pr as usize))
                        .ghost()
                        .small()
                        .label(format!("PR #{pr}"))
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                );
            }
            for (index, build) in scope.builds.iter().enumerate() {
                let url = build.url.clone();
                actions = actions.child(
                    Button::new((SharedString::from(format!("board-build-{number}")), index))
                        .ghost()
                        .small()
                        .label("Build")
                        .tooltip(build.label.clone())
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                );
            }
            for (index, note) in scope.notes.iter().enumerate() {
                if note == &binding.stage_note {
                    continue;
                }
                let note = note.clone();
                actions = actions.child(
                    Button::new((SharedString::from(format!("board-note-{number}")), index))
                        .ghost()
                        .small()
                        .debug_selector(move || format!("board-note-{number}-{index}"))
                        .label(if okilum_t3::board::https_link(&note) {
                            "Link"
                        } else {
                            "Note"
                        })
                        .tooltip(note.clone())
                        .on_click(cx.listener(move |this, _, _, cx| this.note(&note, cx))),
                );
            }
            card = card.child(actions);
            if let Some(issue) = issue {
                card = card.child(
                    div()
                        .text_size(px(12.))
                        .child(issue.body.chars().take(1800).collect::<String>()),
                );
            }
        }
        card.into_any_element()
    }
    pub(super) fn compact_local_card(
        &self,
        card: &okilum_t3::board::local::Card,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = brand::palette(cx);
        let id = card.id.clone();
        let selected = self.selected_local.as_ref() == Some(&id);
        self.card_rendered();
        let (button, _) = controls::disclosure(
            SharedString::from(format!("local-summary-{id}")),
            window,
            cx,
        );
        let card = button
            .h_auto()
            .p_0()
            .w_full()
            .min_w_0()
            .debug_selector({
                let id = id.clone();
                move || format!("board-local-expand-{id}")
            })
            .child(
                v_flex()
                    .debug_selector({
                        let id = id.clone();
                        move || format!("board-local-card-{id}")
                    })
                    .w_full()
                    .min_w_0()
                    .p(px(2.))
                    .gap(px(0.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(if selected { p.accent } else { p.border_subtle })
                    .bg(p.surface_raised)
                    .hover(|s| s.bg(p.surface))
                    .text_color(p.text)
                    .font_weight(FontWeight::NORMAL)
                    .child(
                        div()
                            .h(px(18.))
                            .text_size(px(12.))
                            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                            .child("Local"),
                    )
                    .child(
                        div()
                            .min_h(px(39.))
                            .w_full()
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(px(13.))
                            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                            .line_clamp(2)
                            .child(card.title.clone()),
                    )
                    .child(
                        div()
                            .h(px(18.))
                            .text_size(px(12.))
                            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                            .text_color(p.text_muted)
                            .child(card.state.label()),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.selected = None;
                this.selected_local = (!selected).then(|| id.clone());
                this.focus.focus(window, cx);
                cx.notify();
            }))
            .into_any_element();
        self.track_card_layout(card, cx)
    }
    /// Exactly three rows; selection opens actions outside the virtualised lane.
    pub(super) fn compact_card(
        &self,
        scope: &okilum_t3::board::Scope,
        column: Column,
        binding: &Binding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.compact_card_control(scope, column, binding, true, window, cx)
    }
    fn compact_card_control(
        &self,
        scope: &okilum_t3::board::Scope,
        column: Column,
        binding: &Binding,
        record_focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        #[cfg(all(target_os = "linux", feature = "project-ui-harness"))]
        let _work = super::super::performance::Work::start(1);
        self.card_rendered();
        let p = brand::palette(cx);
        let number = scope.number;
        let issue = self.snapshot.forgejo.value.get(&number);
        let agent = live::agent(scope, &self.snapshot.t3.value, now(), 4 * 3600);
        let mut meta = h_flex()
            .w_full()
            .min_w_0()
            .h(px(20.))
            .gap(px(2.))
            .text_size(px(12.))
            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
            .text_color(p.text_muted)
            .child(div().flex_none().child(format!("#{number}")))
            .child(row_bits::type_tag(issue, cx));
        let mut labels = h_flex().min_w_0().flex_1().gap(px(2.)).overflow_hidden();
        if let Some(origin) = appearance::origin(issue) {
            labels = labels.child(row_bits::label_pill(origin, p.warning, cx));
        }
        if let Some(epic) = issue.and_then(|i| {
            i.labels.iter().find(|l| {
                l.name == "epic" || l.name.starts_with("epic:") || l.name.starts_with("epic/")
            })
        }) {
            labels = labels.child(row_bits::label_pill(
                epic.name.clone(),
                brand::reader_palette(cx).callout_important,
                cx,
            ));
        }
        meta = meta.child(labels);
        if let Some(agent) = &agent {
            #[cfg(target_os = "linux")]
            let url = self
                .manager_display
                .identity
                .as_ref()
                .map(|identity| identity.thread_url(&agent.thread.thread_id));
            #[cfg(not(target_os = "linux"))]
            let url = self.manager.as_ref().and_then(|m| {
                m.read(cx)
                    .identity
                    .as_ref()
                    .map(|identity| identity.thread_url(&agent.thread.thread_id))
            });
            meta = meta.child(
                Button::new(("board-agent-link", number as usize))
                    .debug_selector(move || format!("board-agent-link-{number}"))
                    .ghost()
                    .small()
                    .size(px(22.))
                    .p_0()
                    .flex_none()
                    .disabled(url.is_none())
                    .tooltip(format!(
                        "{} · {} (time since thread update)",
                        scope.executor,
                        agent.label()
                    ))
                    .accessibility_label(format!("Agent {}: {}", scope.executor, agent.label()))
                    .child(
                        div()
                            .debug_selector(move || format!("board-agent-{number}"))
                            .child(row_bits::agent_avatar(
                                &scope.executor,
                                row_bits::active(agent.thread),
                                cx,
                            )),
                    )
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        if let Some(url) = &url {
                            cx.open_url(url);
                        }
                    }),
            );
        } else if !scope.executor.is_empty() {
            meta = meta.child(
                div()
                    .debug_selector(move || format!("board-agent-{number}"))
                    .child(row_bits::agent_avatar(&scope.executor, false, cx)),
            );
        }
        let pr = scope
            .pull_requests
            .first()
            .or_else(|| issue.and_then(|issue| issue.open_pull_requests.first()));
        let checks = pr
            .and_then(|pr| issue.and_then(|i| i.live_pulls.iter().find(|pull| pull.number == *pr)))
            .map(|p| p.checks.as_str());
        let failed = matches!(checks, Some("failure" | "error"));
        let stale = scope.stuck || agent.as_ref().is_some_and(|a| a.stuck);
        #[cfg(target_os = "linux")]
        let progress = agent
            .as_ref()
            .and_then(|a| self.manager_display.progress(scope, a.thread));
        #[cfg(not(target_os = "linux"))]
        let progress = agent.as_ref().and_then(|a| {
            self.manager
                .as_ref()
                .and_then(|m| live::progress(scope, a.thread, m.read(cx).chat.as_ref()))
        });
        let state = if failed {
            "CI failed".to_owned()
        } else if stale {
            format!(
                "⚠ {} · долго",
                agent
                    .as_ref()
                    .map(|a| a.label())
                    .unwrap_or_else(|| "Blocked".into())
            )
        } else {
            progress.unwrap_or_else(|| {
                agent
                    .as_ref()
                    .map(|a| a.state_label().to_owned())
                    .unwrap_or_else(|| list::label(column).into())
            })
        };
        let mut now_row = h_flex()
            .w_full()
            .min_w_0()
            .h(px(18.))
            .gap(px(8.))
            .text_size(px(12.))
            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
            .text_color(p.text_muted)
            .child(
                div()
                    .debug_selector(move || format!("board-state-{number}"))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if failed {
                        p.danger
                    } else if stale {
                        p.warning
                    } else {
                        p.text_muted
                    })
                    .child(
                        div()
                            .debug_selector(move || {
                                if failed {
                                    format!("board-card-ci-failed-{number}")
                                } else if stale {
                                    format!("board-card-stale-{number}")
                                } else {
                                    format!("board-card-state-{number}")
                                }
                            })
                            .child(state),
                    ),
            );
        if let Some(pr) = pr {
            let url = binding.pull_url(*pr);
            now_row = now_row.child(
                div()
                    .id(("board-pr-badge", number as usize))
                    .debug_selector(move || format!("board-pr-badge-{number}"))
                    .flex_none()
                    .cursor_pointer()
                    .child(
                        h_flex()
                            .gap(px(5.))
                            .child(row_bits::ci_dot(checks, cx))
                            .child(pr.to_string()),
                    )
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        cx.open_url(&url);
                    }),
            );
        }
        let (button, focus) = controls::disclosure(("board-expand", number as usize), window, cx);
        #[cfg(test)]
        if record_focus {
            self.card_focus.borrow_mut().insert(number, focus);
        }
        #[cfg(not(test))]
        let _ = (record_focus, focus);
        let card = button
            .h_auto()
            .p_0()
            .accessibility_label(format!("Expand issue {number}"))
            .debug_selector(move || format!("board-expand-{number}"))
            .w_full()
            .min_w_0()
            .cursor_pointer()
            .on_click(cx.listener(move |this, event, window, cx| {
                this.selected_local = None;
                this.selected = (this.selected != Some(number)).then_some(number);
                if !matches!(event, gpui::ClickEvent::Keyboard(_)) {
                    this.focus.focus(window, cx);
                }
                cx.notify();
            }))
            .child(
                v_flex()
                    .debug_selector(move || format!("board-card-{number}"))
                    .w_full()
                    .min_w_0()
                    .p(px(2.))
                    .gap(px(0.))
                    .border_1()
                    .border_color(if self.selected == Some(number) {
                        p.accent
                    } else {
                        p.border_subtle
                    })
                    .rounded(px(8.))
                    .bg(p.surface_raised)
                    .hover(|s| s.bg(p.surface))
                    .text_color(p.text)
                    .font_weight(FontWeight::NORMAL)
                    .child(meta.debug_selector(move || format!("board-card-meta-{number}")))
                    .child(
                        div()
                            .debug_selector(move || format!("board-card-title-{number}"))
                            .w_full()
                            .min_w_0()
                            .min_h(px(39.))
                            .text_size(px(13.))
                            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                            .whitespace_normal()
                            .line_clamp(2)
                            .child(
                                issue
                                    .map(|i| i.title.clone())
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| scope.expected.clone()),
                            ),
                    )
                    .child(now_row.debug_selector(move || format!("board-card-now-{number}"))),
            )
            .into_any_element();
        self.track_card_layout(card, cx)
    }
}

#[cfg(test)]
mod tests;
