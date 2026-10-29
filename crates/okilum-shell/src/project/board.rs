//! Native project center. Source reads are independent of the existing Manager poller.
use super::*;
use okilum_t3::board::{Binding, Column, Snapshot, Source};
use std::time::{SystemTime, UNIX_EPOCH};

mod appearance;
mod live;
mod manage;

type IssueReader = fn(&Binding) -> Vec<(u64, Result<okilum_t3::board::Issue, String>)>;

pub(crate) struct Board {
    pub(super) area: PathBuf,
    binding: Option<Binding>,
    read_issues: IssueReader,
    binding_error: Option<String>,
    binding_read: Option<u64>,
    binding_loading: bool,
    snapshot: Snapshot,
    manager: Option<Entity<ProjectView>>,
    active: bool,
    ticker: Option<Task<()>>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    verified_open: bool,
    filter: Option<String>,
    selected: Option<u64>,
    notice: Option<String>,
    editor: Option<manage::Editor>,
    editor_loading: bool,
}
#[derive(Clone)]
pub(crate) enum Action {
    Note(String),
    Discuss(String),
}
impl EventEmitter<Action> for Board {}
impl Focusable for Board {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn source_problem<T>(
    name: &str,
    source: &Source<T>,
    interval: u64,
    at: u64,
) -> Option<(String, Option<u64>)> {
    let stale = source
        .last_success
        .is_some_and(|read| at.saturating_sub(read) > interval.saturating_mul(2));
    if source.error.is_some() {
        Some((format!("{name} unavailable"), source.last_success))
    } else if stale {
        Some((format!("{name} out of date"), source.last_success))
    } else {
        None
    }
}
fn problem_line(problems: Vec<(String, Option<u64>)>) -> Option<String> {
    if problems.is_empty() {
        return None;
    }
    let last_read = problems.iter().filter_map(|(_, read)| *read).min();
    let mut text = problems
        .into_iter()
        .map(|(text, _)| text)
        .collect::<Vec<_>>()
        .join(" · ");
    if let Some(time) = last_read
        .and_then(|read| chrono::DateTime::<chrono::Utc>::from_timestamp(read.try_into().ok()?, 0))
    {
        text.push_str(&format!(
            " · data from {}",
            time.with_timezone(&chrono::Local).format("%H:%M")
        ));
    }
    Some(text)
}
impl Board {
    fn with_reader(
        area: PathBuf,
        read_issues: IssueReader,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let owner = area.to_string_lossy().into_owned();
        Self {
            area,
            binding: None,
            read_issues,
            binding_error: None,
            binding_read: None,
            binding_loading: false,
            snapshot: Snapshot::new(owner),
            manager: None,
            active: false,
            ticker: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            verified_open: false,
            filter: None,
            selected: None,
            notice: None,
            editor: None,
            editor_loading: false,
        }
    }
    fn reload_binding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.active || self.binding_loading {
            return;
        }
        self.binding_loading = true;
        let area = self.area.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { Binding::read(&area) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.binding_loading = false;
                match result {
                    Ok(binding) => {
                        let same_source = this
                            .binding
                            .as_ref()
                            .zip(binding.as_ref())
                            .is_some_and(|(old, new)| old.same_issue_source(new));
                        if this.binding != binding && !same_source {
                            this.snapshot.invalidate();
                            this.snapshot.forgejo = Source::default();
                            this.selected = None;
                        }
                        if let Some(next) = &binding {
                            if this
                                .binding
                                .as_ref()
                                .is_none_or(|old| old.card_problems != next.card_problems)
                            {
                                for problem in &next.card_problems {
                                    eprintln!(
                                        "Board card problem: {}: {}",
                                        problem.field, problem.detail
                                    );
                                }
                            }
                        }
                        this.binding = binding;
                        this.binding_read = Some(now());
                        this.binding_error = None;
                        if this.active {
                            this.refresh(window, cx);
                        }
                    }
                    Err(error) => {
                        let detail = format!("{error:#}");
                        if this.binding_error.as_deref() != Some(detail.as_str()) {
                            eprintln!("Board binding error: {detail}");
                        }
                        this.binding_error = Some(detail);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(crate) fn set_manager(
        &mut self,
        manager: Entity<ProjectView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.manager.as_ref() == Some(&manager) {
            return;
        }
        self.snapshot.invalidate();
        self.snapshot.t3 = Source::default();
        self.manager = Some(manager);
        if self.active {
            self.refresh(window, cx);
        }
    }
    pub(crate) fn set_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active == visible {
            return;
        }
        self.active = visible;
        self.ticker = None; // Dropping the task cancels the visible-only timer.
        self.snapshot.invalidate();
        if visible {
            window.focus(&self.focus, cx);
            self.reload_binding(window, cx);
            self.ticker = Some(cx.spawn_in(window, async move |this, cx| loop {
                let seconds = match this.update(cx, |this, _| {
                    this.binding
                        .as_ref()
                        .map(|b| b.refresh_seconds)
                        .unwrap_or(30)
                }) {
                    Ok(seconds) => seconds,
                    Err(_) => break,
                };
                cx.background_executor()
                    .timer(Duration::from_secs(seconds))
                    .await;
                if this
                    .update_in(cx, |this, window, cx| this.reload_binding(window, cx))
                    .is_err()
                {
                    break;
                }
            }));
        }
        cx.notify();
    }
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.active {
            return;
        }
        let Some(binding) = self.binding.clone() else {
            return;
        };
        let Some(ticket) = self.snapshot.begin() else {
            return;
        }; // Coalesce overlapping refreshes.
        let forgejo_ticket = ticket.clone();
        let read_issues = self.read_issues;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { read_issues(&binding) })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.snapshot.issues(&forgejo_ticket, result, now());
                cx.notify();
            });
        })
        .detach();
        let connection = self.manager.as_ref().and_then(|manager| {
            let manager = manager.read(cx);
            Some((
                manager.gateway.clone()?,
                manager.binding.as_ref()?.executor.project_id.clone(),
            ))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    connection
                        .ok_or_else(|| {
                            "T3 not connected; Forgejo and notes remain available".to_owned()
                        })
                        .and_then(|(gateway, project)| {
                            gateway.list_threads(&project).map_err(|e| e.to_string())
                        })
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.snapshot.threads(&ticket, result, now());
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn problem(&self, at: u64) -> Option<String> {
        let mut problems = Vec::new();
        if let Some(detail) = &self.binding_error {
            problems.push((format!("Board: {detail}"), self.binding_read));
        }
        if self
            .binding
            .as_ref()
            .is_some_and(|binding| binding.registry_error.is_some())
        {
            problems.push(("Executor registry unavailable".into(), self.binding_read));
        }
        let interval = self
            .binding
            .as_ref()
            .map(|b| b.refresh_seconds)
            .unwrap_or(30);
        problems.extend(source_problem(
            "Forgejo",
            &self.snapshot.forgejo,
            interval,
            at,
        ));
        problems.extend(source_problem("T3", &self.snapshot.t3, interval, at));
        if self.snapshot.t3.error.is_none()
            && self.snapshot.t3.last_success.is_some()
            && self.binding.as_ref().is_some_and(|binding| {
                binding.issues.iter().filter_map(live::thread_id).any(|id| {
                    self.snapshot
                        .t3
                        .value
                        .iter()
                        .filter(|t| t.thread_id == id)
                        .count()
                        != 1
                })
            })
        {
            problems.push((
                "Agent binding unavailable".into(),
                self.snapshot.t3.last_success,
            ));
        }
        problem_line(problems)
    }
    fn agent_filter(&self, binding: &Binding, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let roles: std::collections::BTreeSet<_> =
            binding.issues.iter().map(|i| i.executor.clone()).collect();
        let selected = self.filter.clone();
        let weak = cx.entity().downgrade();
        let label = selected.clone().unwrap_or_else(|| "All agents".into());
        let mut button = Button::new("board-agent-filter")
            .debug_selector(|| "board-agent-filter".into())
            .small()
            .ghost()
            .accessibility_label(format!("Agent filter: {label}"))
            .tooltip(format!("Agent filter: {label}"));
        if compact {
            button = button.size(px(28.)).child(appearance::icon(
                "list-filter",
                brand::palette(cx).text_muted,
            ));
        } else {
            button = button.label(label).max_w(px(150.)).min_w_0();
        }
        button
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                let all = weak.clone();
                menu = menu.item(
                    PopupMenuItem::new("All agents")
                        .checked(selected.is_none())
                        .on_click(move |_, _, cx| {
                            let _ = all.update(cx, |board, cx| {
                                board.filter = None;
                                cx.notify();
                            });
                        }),
                );
                for role in &roles {
                    let owner = weak.clone();
                    let role = role.clone();
                    menu = menu.item(
                        PopupMenuItem::new(role.clone())
                            .checked(selected.as_ref() == Some(&role))
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |board, cx| {
                                    board.filter = Some(role.clone());
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }
    fn attention(&self, binding: &Binding, cx: &mut Context<Self>) -> Option<AnyElement> {
        if binding.attention.is_empty() {
            return None;
        }
        let colors = brand::palette(cx);
        let mut cards = h_flex().w_full().min_w_0().flex_wrap().gap_2();
        for (index, item) in binding.attention.iter().enumerate() {
            let tone = if item.kind == okilum_t3::board::AttentionKind::Hands {
                colors.warning
            } else {
                brand::reader_palette(cx).callout_important
            };
            let link = item.link.clone();
            cards = cards.child(
                Button::new(("board-attention", index))
                    .debug_selector(move || format!("board-attention-{index}"))
                    .accessibility_label(item.title.clone())
                    .ghost()
                    .flex_1()
                    .flex_basis(px(180.))
                    .min_w_0()
                    .h_auto()
                    .p_3()
                    .border_1()
                    .border_color(tone.opacity(0.25))
                    .rounded(px(8.))
                    .bg(appearance::tint(tone))
                    .child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .items_start()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_size(px(12.))
                                    .text_color(tone)
                                    .child(item.kind.label())
                                    .child(appearance::icon("arrow-up-right", tone)),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(item.title.clone()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .text_size(px(12.))
                                    .text_color(colors.text_muted)
                                    .child(item.subline.clone()),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if okilum_t3::board::relative_note(&link) {
                            this.note(&link, cx);
                        } else {
                            cx.open_url(&link);
                        }
                    })),
            );
        }
        Some(
            v_flex()
                .debug_selector(|| "board-attention".into())
                .w_full()
                .min_w_0()
                .gap_3()
                .child(
                    h_flex()
                        .gap_2()
                        .child(appearance::icon("sparkles", colors.accent))
                        .child("Waiting for you")
                        .child(binding.attention.len().to_string())
                        .child(div().flex_1())
                        .child(self.agent_filter(binding, false, cx)),
                )
                .child(cards)
                .into_any_element(),
        )
    }
    fn card(
        &self,
        scope: &okilum_t3::board::Scope,
        column: Column,
        binding: &Binding,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = brand::palette(cx);
        let number = scope.number;
        let issue = self.snapshot.forgejo.value.get(&number);
        let (kind, glyph) = appearance::kind(issue);
        let tone = appearance::type_color(kind, cx);
        let selected = self.selected == Some(number);
        let agent = live::agent(
            scope,
            &self.snapshot.t3.value,
            now(),
            binding.stuck_after_seconds,
        );
        let stuck = scope.stuck || agent.as_ref().is_some_and(|a| a.stuck);

        let mut meta = h_flex()
            .gap_2()
            .min_w_0()
            .flex_wrap()
            .text_size(px(11.))
            .text_color(colors.text_muted)
            .child(
                h_flex()
                    .gap_1()
                    .px_1()
                    .py_0p5()
                    .rounded(px(4.))
                    .bg(appearance::tint(tone))
                    .text_color(tone)
                    .child(appearance::icon(glyph, tone))
                    .child(kind),
            )
            .child(format!("#{number}"));
        if let Some(origin) = appearance::origin(issue) {
            meta = meta.child(
                h_flex()
                    .gap_1()
                    .child(appearance::icon("user-round", colors.text_muted))
                    .child(origin),
            );
        }
        let mut card = v_flex()
            .id(("board-card", number as usize))
            .debug_selector(move || format!("board-card-{number}"))
            .w_full()
            .min_w_0()
            .gap_2()
            .border_1()
            .border_color(if stuck {
                colors.warning
            } else if selected {
                colors.accent
            } else {
                colors.border_subtle
            })
            .rounded(px(8.))
            .bg(if stuck {
                colors.surface.blend(appearance::tint(colors.warning))
            } else {
                colors.surface_raised
            })
            .overflow_hidden()
            .child(
                Button::new(("board-expand", number as usize))
                    .debug_selector(move || format!("board-expand-{number}"))
                    .accessibility_label(format!("Expand issue {number}"))
                    .ghost()
                    .w_full()
                    .min_w_0()
                    .h_auto()
                    .p_3()
                    .child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .items_start()
                            .child(meta)
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(scope.expected.clone()),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = (this.selected != Some(number)).then_some(number);
                        cx.notify();
                    })),
            )
            .child(if let Some(agent) = &agent {
                let (url, progress) = self
                    .manager
                    .as_ref()
                    .map(|manager| {
                        let manager = manager.read(cx);
                        (
                            manager
                                .identity
                                .as_ref()
                                .map(|identity| identity.thread_url(&agent.thread.thread_id)),
                            live::progress(scope, agent.thread, manager.chat.as_ref()),
                        )
                    })
                    .unwrap_or_default();
                let mut text = v_flex().w_full().min_w_0().gap_1().child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .child(appearance::icon(
                            if agent.working() {
                                "circle-dot"
                            } else {
                                "sparkles"
                            },
                            if stuck {
                                colors.warning
                            } else if agent.working() {
                                colors.info
                            } else {
                                colors.text_muted
                            },
                        ))
                        .child(
                            div()
                                .debug_selector(move || format!("board-agent-{number}"))
                                .font_weight(FontWeight::MEDIUM)
                                .child(scope.executor.clone()),
                        )
                        .child(
                            div()
                                .debug_selector(move || format!("board-state-{number}"))
                                .min_w_0()
                                .truncate()
                                .child(agent.label()),
                        ),
                );
                if let Some(progress) = progress {
                    text = text.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .child(progress),
                    );
                }
                Button::new(("board-agent-link", number as usize))
                    .debug_selector(move || format!("board-agent-link-{number}"))
                    .small()
                    .ghost()
                    .w_full()
                    .min_w_0()
                    .h_auto()
                    .px_3()
                    .py_1()
                    .text_color(if stuck {
                        colors.warning
                    } else {
                        colors.text_muted
                    })
                    .disabled(url.is_none())
                    .tooltip("Open the explicitly bound agent thread")
                    .child(text)
                    .on_click(move |_, _, cx| {
                        if let Some(url) = &url {
                            cx.open_url(url);
                        }
                    })
                    .into_any_element()
            } else {
                h_flex()
                    .min_w_0()
                    .gap_2()
                    .px_3()
                    .text_size(px(12.))
                    .text_color(if stuck {
                        colors.warning
                    } else {
                        colors.text_muted
                    })
                    .child(appearance::icon(
                        if scope.executor == "Tests" {
                            "flask-conical"
                        } else {
                            "sparkles"
                        },
                        colors.text_muted,
                    ))
                    .child(
                        div()
                            .debug_selector(move || format!("board-agent-{number}"))
                            .font_weight(FontWeight::MEDIUM)
                            .child(scope.executor.clone()),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("board-state-{number}"))
                            .min_w_0()
                            .truncate()
                            .child(if stuck { "Blocked" } else { column.label() }),
                    )
                    .into_any_element()
            })
            .child(div().h(px(2.)));
        // A single explicit PR link is a link, not a fabricated CI/merge status.
        if let Some(pr) = scope.pull_requests.first() {
            let url = binding.pull_url(*pr);
            card = card.child(
                h_flex().px_2().pb_2().child(
                    Button::new(("board-pr-badge", number as usize))
                        .ghost()
                        .small()
                        .min_w_0()
                        .child(appearance::icon("git-pull-request", colors.accent))
                        .label(format!("PR #{pr}"))
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                ),
            );
        }
        if self.selected == Some(number) {
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
    fn note(&mut self, target: &str, cx: &mut Context<Self>) {
        if okilum_t3::board::https_link(target) {
            cx.open_url(target);
        } else if okilum_t3::board::relative_note(target) {
            cx.emit(Action::Note(target.to_owned()));
        } else {
            self.notice = Some("This card has an invalid related link.".into());
            cx.notify();
        }
    }
    fn context(&self, scope: &okilum_t3::board::Scope) -> String {
        let binding = self.binding.as_ref().expect("rendered configured card");
        let issue = self.snapshot.forgejo.value.get(&scope.number);
        format!("Board: {}\nIssue: {}\nExpected result: {}\nExecutor: {}\nLast issue state: {}\nStage note: {}\n\nQuestion for Manager: ",
            binding.title, binding.issue_url(scope.number), scope.expected, scope.executor,
            issue.map(|i| i.state.as_str()).unwrap_or("not read"), self.area.join(&binding.stage_note).display())
    }
}
impl Render for Board {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.editor.is_some() {
            return self.render_management(window, cx);
        }
        let colors = brand::palette(cx);
        let mut root = v_flex()
            .id("project-board")
            .debug_selector(|| "project-board".into())
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .p_4()
            .gap_3()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .bg(colors.canvas)
            .text_color(colors.text)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && this.selected.take().is_some() {
                    cx.stop_propagation();
                    cx.notify();
                }
            }));
        if let Some(problem) = self.problem(now()) {
            root = root.child(
                div()
                    .debug_selector(|| "board-source-problem".into())
                    .text_size(px(12.))
                    .text_color(colors.text_muted)
                    .child(problem),
            );
        }
        let Some(binding) = self.binding.as_ref() else {
            if !self.binding_loading && self.binding_error.is_none() {
                root = root.child(
                    v_flex().flex_1().items_center().justify_center().gap_3()
                        .debug_selector(|| "board-empty".into())
                        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Keep project work in view"))
                        .child(div().text_size(px(13.)).text_color(colors.text_muted).child("A Board brings the project's issues, expected results and executors together."))
                        .child(Button::new("board-create").debug_selector(|| "board-create".into()).primary().label("Create board")
                            .disabled(self.editor_loading).on_click(cx.listener(|this, _, window, cx| this.open_management(window, cx))))
                );
            }
            return root.into_any_element();
        };
        if let Some(attention) = self.attention(binding, cx) {
            root = root.child(attention);
        }
        if let Some(notice) = &self.notice {
            root = root.child(notice.clone());
        }
        let mut columns = h_flex()
            .id("board-columns")
            .debug_selector(|| "board-columns".into())
            .items_start()
            .gap_3()
            .w_full()
            .min_w_0();
        let mut stages = vec![
            Column::Queue,
            Column::Working,
            Column::Checks,
            Column::Ready,
        ];
        if self.verified_open {
            stages.push(Column::Verified);
        }
        for column in stages {
            let scopes: Vec<_> = binding
                .issues
                .iter()
                .filter(|scope| {
                    self.filter.as_ref().is_none_or(|f| f == &scope.executor)
                        && okilum_t3::board::column(
                            scope,
                            self.snapshot.forgejo.value.get(&scope.number),
                            &self.snapshot.t3.value,
                        ) == column
                })
                .collect();
            let (color, glyph) = appearance::stage(column, cx);
            let mut lane = v_flex()
                .id(("board-lane", column as usize))
                .debug_selector(move || format!("board-lane-{column:?}"))
                .flex_1()
                .flex_basis(px(0.))
                .min_w_0()
                .gap_3()
                .border_t_2()
                .border_color(color)
                .pt_3()
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_2()
                        .flex_wrap()
                        .child(appearance::icon(glyph, color))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(13.))
                                .child(column.label()),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(colors.text_muted)
                                .child(
                                    (scopes.len()
                                        + if column == Column::Queue {
                                            binding.card_problems.len()
                                        } else {
                                            0
                                        })
                                    .to_string(),
                                ),
                        ),
                );
            for scope in scopes {
                lane = lane.child(self.card(scope, column, binding, cx));
            }
            if column == Column::Queue {
                for (index, problem) in binding.card_problems.iter().enumerate() {
                    let title = problem
                        .number
                        .map(|n| format!("#{n} · needs attention"))
                        .unwrap_or_else(|| "Card needs attention".into());
                    lane = lane.child(
                        v_flex()
                            .debug_selector(move || format!("board-card-problem-{index}"))
                            .min_w_0()
                            .gap_1()
                            .p_3()
                            .rounded_md()
                            .bg(colors.surface_raised)
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(colors.warning)
                                    .child(format!("{}: {}", problem.field, problem.detail)),
                            ),
                    );
                }
            }
            columns = columns.child(lane);
        }
        let verified = binding
            .issues
            .iter()
            .filter(|scope| {
                scope.stage == Some(okilum_t3::board::HumanStage::Verified)
                    && self.filter.as_ref().is_none_or(|f| f == &scope.executor)
            })
            .count();
        // A narrow native strip leaves all working columns inside the center.
        // Its full accessible label/tooltip remain readable when space is tight.
        let toggle = Button::new("board-verified-toggle")
            .debug_selector(|| "board-verified-toggle".into())
            .accessibility_label(if self.verified_open {
                "Collapse verified results"
            } else {
                "Expand verified results"
            })
            .tooltip("Verified by a person on a specific build")
            .ghost()
            .w(px(36.))
            .min_w(px(36.))
            .h(px(100.))
            .p_1()
            .child(
                v_flex()
                    .gap_3()
                    .items_center()
                    .child(appearance::icon("circle-check", colors.text_muted))
                    .child(verified.to_string())
                    .child(appearance::icon(
                        if self.verified_open {
                            "chevron-left"
                        } else {
                            "chevron-right"
                        },
                        colors.text_muted,
                    )),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.verified_open = !this.verified_open;
                cx.notify();
            }));
        let mut end = v_flex().w(px(36.)).flex_none().gap_2();
        if binding.attention.is_empty() {
            end = end.child(self.agent_filter(binding, true, cx));
        }
        end = end.child(
            Button::new("board-manage")
                .debug_selector(|| "board-manage".into())
                .ghost()
                .small()
                .child(appearance::icon("settings-2", colors.text_muted))
                .tooltip("Manage board")
                .accessibility_label("Manage board")
                .disabled(self.editor_loading)
                .on_click(cx.listener(|this, _, window, cx| this.open_management(window, cx))),
        );
        root.child(columns.child(end.child(toggle)))
            .into_any_element()
    }
}

impl Reader {
    pub(crate) fn ensure_project_board(
        &mut self,
        area: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_project_board_using(area, okilum_t3::board::read_issues, window, cx);
    }
    fn ensure_project_board_using(
        &mut self,
        area: PathBuf,
        read_issues: IssueReader,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.project_navigation.board_visible = true;
        if self.project_navigation.boards.contains_key(&area) {
            return;
        }
        let board = cx.new(|cx| Board::with_reader(area.clone(), read_issues, window, cx));
        let owner = area.clone();
        cx.subscribe_in(&board, window, move |this, board, action: &Action, window, cx| {
            if this.project_navigation.active.as_ref().map(|p| &p.folder) != Some(&owner) { return; }
            match action {
                Action::Note(note) => {
                    let target = owner.join(note);
                    let opened = (|| -> anyhow::Result<String> {
                        anyhow::ensure!(okilum_t3::board::relative_note(note), "Invalid Board note path");
                        let canonical = target.canonicalize()?;
                        anyhow::ensure!(canonical.starts_with(owner.canonicalize()?), "Board note escapes the project");
                        let relative = canonical.strip_prefix(this.vault_root.canonicalize()?)?;
                        Ok(okilum_core::vault::note_path(relative))
                    })();
                    match opened {
                        Ok(relative) => this.open_note(&relative, None, window, cx),
                        Err(error) => { this.link_notice = Some(format!("Board note unavailable: {error:#}").into()); cx.notify(); }
                    }
                }
                Action::Discuss(text) => {
                    let appended = this.project.as_ref().is_some_and(|manager| manager.update(cx, |manager, cx|
                        manager.append_board_context(&owner, text.clone(), window, cx)));
                    if appended { this.panels.open(reader_layout::Panel::Backlinks); }
                    board.update(cx, |board, cx| {
                        board.notice = Some(if appended { "Context appended to the existing Manager draft. Review and send when ready; Undo removes the block." }
                            else { "Manager is still loading; the draft was not changed. Try again." }.into());
                        cx.notify();
                    });
                }
            }
        }).detach();
        cx.observe(&board, |_, _, cx| cx.notify()).detach();
        self.project_navigation.boards.insert(area, board);
    }
    pub(crate) fn connect_project_board(
        &self,
        manager: Entity<ProjectView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(board) = self
            .project_navigation
            .active
            .as_ref()
            .and_then(|p| self.project_navigation.boards.get(&p.folder))
        {
            board.update(cx, |board, cx| board.set_manager(manager, window, cx));
        }
    }
    pub(crate) fn sync_project_board_visibility(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.project_navigation.active.as_ref().map(|p| &p.folder);
        for (area, board) in &self.project_navigation.boards {
            let visible = active == Some(area)
                && self.project_navigation.board_visible
                && self.project_navigation.section == reader_project_navigation::Section::Reader;
            board.update(cx, |board, cx| board.set_visible(visible, window, cx));
        }
    }
    pub(crate) fn render_project_center(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let board = self
            .project_navigation
            .active
            .as_ref()
            .and_then(|p| self.project_navigation.boards.get(&p.folder));
        if self.project_navigation.section == reader_project_navigation::Section::Reader {
            if let Some(board) = board {
                if self.project_navigation.board_visible {
                    return div().size_full().child(board.clone()).into_any_element();
                }
                return v_flex()
                    .size_full()
                    .min_h_0()
                    .child(
                        h_flex().px_3().py_1().child(
                            Button::new("project-return-board")
                                .debug_selector(|| "project-return-board".into())
                                .ghost()
                                .label("← Board")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.project_navigation.board_visible = true;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .child(self.render_document_surface(window, cx)),
                    )
                    .into_any_element();
            }
        }
        self.render_document_surface(window, cx)
    }
}

#[cfg(test)]
mod tests;
