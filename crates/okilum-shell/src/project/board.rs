//! Native project center. Source reads are independent of the existing Manager poller.
use super::*;
use okilum_t3::board::{Binding, Column, Snapshot, Source};
use std::time::{SystemTime, UNIX_EPOCH};

mod appearance;
mod columns;
mod delivery;
mod list;
mod live;
mod local;
mod manage;
mod notes;
mod row_bits;

type IssueReader =
    Arc<dyn Fn(&Binding) -> Result<Vec<okilum_t3::board::Issue>, String> + Send + Sync>;

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
    local_cards: okilum_t3::board::local::Snapshot,
    local_loading: bool,
    local_generation: u64,
    selected_local: Option<String>,
    local_editor: Option<local::Editor>,
    delivery: okilum_t3::board::delivery::Snapshot,
    delivery_loading: bool,
    project_notes: okilum_t3::board::context::Snapshot,
    notes_loading: bool,
    notes_expanded: bool,
    next_stage_open: bool,
    columns: columns::State,
    list: list::State,
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
    if let Some(error) = &source.error {
        Some((
            format!("{name}: {}", status_detail(error)),
            source.last_success,
        ))
    } else if stale {
        Some((format!("{name} out of date"), source.last_success))
    } else {
        None
    }
}
fn status_detail(detail: &str) -> String {
    detail.split_whitespace().collect::<Vec<_>>().join(" ")
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
    #[cfg(test)]
    fn with_kanban_reader(
        area: PathBuf,
        read_issues: impl Fn(&Binding) -> Result<Vec<okilum_t3::board::Issue>, String>
            + Send
            + Sync
            + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut board = Self::with_reader(area, read_issues, window, cx);
        board.list.preferences.mode = list::Mode::Board;
        board
    }
    fn with_reader(
        area: PathBuf,
        read_issues: impl Fn(&Binding) -> Result<Vec<okilum_t3::board::Issue>, String>
            + Send
            + Sync
            + 'static,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let owner = area.to_string_lossy().into_owned();
        let list = list::State::load(&area, cx);
        Self {
            area,
            binding: None,
            read_issues: Arc::new(read_issues),
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
            local_cards: Default::default(),
            local_loading: false,
            local_generation: 0,
            selected_local: None,
            local_editor: None,
            delivery: Default::default(),
            delivery_loading: false,
            project_notes: Default::default(),
            notes_loading: false,
            notes_expanded: false,
            next_stage_open: false,
            columns: columns::State::default(),
            list,
        }
    }
    fn reload_binding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.active || self.binding_loading {
            return;
        }
        self.binding_loading = true;
        let area = self.area.clone();
        cx.spawn_in(window, {
            let module_access =
                crate::reader_features::access(crate::reader_features::Feature::Agents, cx);
            async move |this, cx| {
                okilum_t3::scope_network(module_access, async move {
                    let result = cx
                        .background_executor()
                        .spawn(okilum_t3::scope_current_network(async move {
                            Binding::read(&area)
                        }))
                        .await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.binding_loading = false;
                        this.list.invalidate();
                        match result {
                            Ok(binding) => {
                                let same_source = this
                                    .binding
                                    .as_ref()
                                    .zip(binding.as_ref())
                                    .is_some_and(|(old, new)| old.same_issue_source(new));
                                if this.binding != binding && !same_source {
                                    this.columns.invalidate();
                                    this.snapshot.invalidate();
                                    this.snapshot.forgejo = Source::default();
                                    this.selected = None;
                                    this.delivery = Default::default();
                                    this.delivery_loading = false;
                                    this.project_notes = Default::default();
                                    this.notes_loading = false;
                                    this.notes_expanded = false;
                                    this.next_stage_open = false;
                                }
                                if let Some(next) = &binding {
                                    if next.registry_error.as_ref()
                                        != this
                                            .binding
                                            .as_ref()
                                            .and_then(|old| old.registry_error.as_ref())
                                    {
                                        if let Some(detail) = &next.registry_error {
                                            eprintln!("Board executor registry error: {detail}");
                                        }
                                    }
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
                .await
            }
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
        self.list.invalidate();
        self.delivery_loading = false;
        self.notes_loading = false;
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
        self.local_loading = false;
        self.local_generation = self.local_generation.wrapping_add(1);
        self.delivery_loading = false;
        self.notes_loading = false;
        self.ticker = None; // Dropping the task cancels the visible-only timer.
        self.snapshot.invalidate();
        if visible {
            window.focus(&self.focus, cx);
            self.reload_binding(window, cx);
            self.ticker = Some(cx.spawn_in(window, async move |this, cx| loop {
                let seconds = match this.update(cx, |this, _| {
                    this.binding
                        .as_ref()
                        .map(|b| b.refresh_seconds.clamp(15, 30))
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
        self.refresh_local_cards(window, cx);
        let Some(ticket) = self.snapshot.begin() else {
            return;
        }; // Coalesce overlapping refreshes.
        self.refresh_project_notes(ticket.clone(), window, cx);
        let forgejo_ticket = ticket.clone();
        let read_issues = self.read_issues.clone();
        let needs_threads = binding
            .issues
            .iter()
            .any(|scope| live::thread_id(scope).is_some());
        cx.spawn_in(window, {
            let module_access =
                crate::reader_features::access(crate::reader_features::Feature::Agents, cx);
            async move |this, cx| {
                okilum_t3::scope_network(module_access, async move {
                    let result = cx
                        .background_executor()
                        .spawn(okilum_t3::scope_current_network(async move {
                            read_issues(&binding)
                        }))
                        .await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.list.invalidate();
                        this.snapshot.live_issues(&forgejo_ticket, result, now());
                        if this.snapshot.matches(&forgejo_ticket) {
                            this.refresh_delivery(forgejo_ticket.clone(), window, cx);
                        }
                        cx.notify();
                    });
                })
                .await
            }
        })
        .detach();
        // A project without registered agents needs only Forgejo/notes. Do not
        // report a transport failure for a source the Board does not use.
        if !needs_threads {
            self.snapshot.threads(&ticket, Ok(Vec::new()), now());
            cx.notify();
            return;
        }
        let connection =
            self.manager
                .as_ref()
                .ok_or_else(|| "The Manager connection is still loading".to_owned())
                .and_then(|manager| {
                    let manager = manager.read(cx);
                    let binding = manager.binding.as_ref().ok_or_else(|| {
                        "No T3 project is configured for Board executors".to_owned()
                    })?;
                    let project = if binding.executor.project_id.is_empty() {
                        binding
                            .manager
                            .project_id
                            .clone()
                            .filter(|id| !id.is_empty())
                            .ok_or_else(|| {
                                "No T3 project is configured for Board executors".to_owned()
                            })?
                    } else {
                        binding.executor.project_id.clone()
                    };
                    Ok((
                        manager.gateway.clone().ok_or_else(|| {
                            manager.error.clone().unwrap_or_else(|| {
                                "The Manager connection is not connected; open Settings › T3".into()
                            })
                        })?,
                        project,
                    ))
                });
        cx.spawn_in(window, {
            let module_access =
                crate::reader_features::access(crate::reader_features::Feature::Agents, cx);
            async move |this, cx| {
                okilum_t3::scope_network(module_access, async move {
                    let result = cx
                        .background_executor()
                        .spawn(okilum_t3::scope_current_network(async move {
                            connection.and_then(|(gateway, project)| {
                                gateway.list_threads(&project).map_err(|e| e.to_string())
                            })
                        }))
                        .await;
                    let _ = this.update_in(cx, |this, _, cx| {
                        this.list.invalidate();
                        let previous = this.snapshot.t3.error.clone();
                        this.snapshot.threads(&ticket, result, now());
                        if this.snapshot.t3.error != previous {
                            if let Some(detail) = &this.snapshot.t3.error {
                                eprintln!("Board T3 error: {detail}");
                            }
                        }
                        cx.notify();
                    });
                })
                .await
            }
        })
        .detach();
        cx.notify();
    }
    fn projection_time(&self) -> u64 {
        if self.snapshot.forgejo.error.is_some() {
            self.snapshot.forgejo.last_success.unwrap_or_else(now)
        } else {
            now()
        }
    }
    fn problem(&self, at: u64) -> Option<String> {
        let mut problems = Vec::new();
        if let Some(detail) = &self.binding_error {
            problems.push((format!("Board: {detail}"), self.binding_read));
        }
        if let Some(detail) = self
            .binding
            .as_ref()
            .and_then(|binding| binding.registry_error.as_ref())
        {
            problems.push((
                format!("Executor registry: {}", status_detail(detail)),
                self.binding_read,
            ));
        }
        if let Some(detail) = &self.local_cards.error {
            problems.push((
                format!("Local cards: {}", status_detail(detail)),
                self.local_cards.last_success,
            ));
        }
        for detail in &self.local_cards.value.problems {
            problems.push((detail.clone(), self.local_cards.last_success));
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
    fn source_indicator(&self, cx: &mut Context<Self>) -> AnyElement {
        self.source_indicator_for(self.problem(now()), cx)
    }
    fn source_indicator_for(&self, problem: Option<String>, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::popover::Popover;
        let has_problem = problem.is_some();
        let colors = brand::palette(cx);
        let detail = problem.unwrap_or_else(|| "No source errors reported.".into());
        // Reserve the same control in every state. Polling may change its color,
        // never the position of the Board's cards or the user's scroll anchor.
        Popover::new("board-source-status-popover")
            .anchor(Anchor::TopRight)
            .trigger(
                Button::new("board-source-status")
                    .debug_selector(|| "board-source-status".into())
                    .ghost()
                    .size(px(28.))
                    .flex_none()
                    .accessibility_label(if has_problem {
                        "Board source error. Show diagnostics"
                    } else {
                        "Board source status. Show diagnostics"
                    })
                    .tooltip(if has_problem {
                        "Board needs attention — show details"
                    } else {
                        "Board source status"
                    })
                    .child(
                        div()
                            .debug_selector(|| "board-source-status-dot".into())
                            .size(px(8.))
                            .rounded_full()
                            .bg(if has_problem {
                                colors.danger
                            } else {
                                colors.text_muted.opacity(0.4)
                            }),
                    ),
            )
            .content(move |_, _, _| {
                v_flex()
                    .id("board-source-details-scroll")
                    .debug_selector(|| "board-source-details".into())
                    .w(px(360.))
                    .max_w_full()
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .gap_2()
                    .p_3()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Board sources"),
                    )
                    .child(
                        div()
                            .debug_selector(|| "board-source-problem".into())
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(px(12.))
                            .child(detail.clone()),
                    )
            })
            .into_any_element()
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
                        .when(self.list.preferences.mode == list::Mode::Board, |header| {
                            header.child(self.agent_filter(binding, false, cx))
                        }),
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
                        this.selected_local = None;
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
        if let Some(problem) = self
            .binding
            .as_ref()
            .and_then(|binding| okilum_t3::board::live::mapping_problem(binding, number))
        {
            card = card.child(
                div()
                    .debug_selector(move || format!("board-mapping-{number}"))
                    .px_3()
                    .pb_2()
                    .text_size(px(12.))
                    .text_color(colors.text_muted)
                    .child(problem),
            );
        }
        if self.selected == Some(number) {
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
        if self.local_editor.is_some() {
            return self.render_local_management(window, cx);
        }
        if self.editor.is_some() {
            return self.render_management(window, cx);
        }
        if self.binding.is_some() && self.list.preferences.mode == list::Mode::List {
            return self.render_list(window, cx);
        }
        let colors = brand::palette(cx);
        let mut root = v_flex()
            .id("project-board")
            .relative()
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
                if event.keystroke.key == "escape" {
                    let issue = this.selected.take().is_some();
                    let local = this.selected_local.take().is_some();
                    if issue || local {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }
            }));
        let Some(binding) = self.binding.as_ref() else {
            root = root.child(
                div()
                    .absolute()
                    .top_4()
                    .right_4()
                    .child(self.source_indicator(cx)),
            );
            if !self.binding_loading && self.binding_error.is_none() {
                root = root.child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .debug_selector(|| "board-empty".into())
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Keep project work in view"),
                        )
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(colors.text_muted)
                                .child(
                                    "A Board keeps local tasks and linked project issues in view.",
                                ),
                        )
                        .child(
                            Button::new("board-create")
                                .debug_selector(|| "board-create".into())
                                .primary()
                                .label("Create board")
                                .disabled(self.editor_loading)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_management(window, cx)
                                })),
                        ),
                );
            }
            return root.into_any_element();
        };
        let projected;
        let binding = if self.snapshot.forgejo.last_success.is_some() {
            projected = okilum_t3::board::live::project(
                binding,
                &self.snapshot.forgejo.value,
                self.projection_time(),
            );
            &projected
        } else {
            binding
        };
        root = root.child(self.view_toggle(cx));
        if binding.repository.is_empty() {
            root = root.child(div().debug_selector(|| "board-local".into())
                .text_size(px(12.)).text_color(colors.text_muted)
                .child("Local Board · PR and CI links are inactive. Add Repository in Manage board → Board details when needed."));
        }
        if let Some(attention) = self.attention(binding, cx) {
            root = root.child(attention);
        }
        if self.delivery.enabled() {
            root = root.child(self.delivery_summary(binding, cx));
        }
        if let Some(notes) = self.render_project_notes(binding, cx) {
            root = root.child(notes);
        }
        if let Some(notice) = &self.notice {
            root = root.child(notice.clone());
        }
        let mut counts = self.column_counts(binding);
        let ready = self.column_sources_ready(binding);
        self.columns.observe(counts, ready);
        let mut compact = h_flex()
            .debug_selector(|| "board-empty-columns".into())
            .flex_wrap()
            .items_center()
            .gap_2();
        let mut collapsed = 0;
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
                        && self.delivery_column(scope) == column
                })
                .collect();
            let local_cards: Vec<_> = self
                .local_cards
                .value
                .cards
                .iter()
                .filter(|card| self.filter.is_none() && card.state.column() == column)
                .collect();
            let count = scopes.len()
                + local_cards.len()
                + if column == Column::Queue {
                    binding.card_problems.len()
                } else {
                    0
                };
            counts[column as usize] = count;
            // Never fold a lane that actually has a rendered card, even if a
            // delivery freshness boundary changed since the count snapshot.
            if column != Column::Verified && self.columns.collapsed(column, count) {
                compact = compact.child(self.expand_empty_column(column, cx));
                collapsed += 1;
                continue;
            }
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
                                .child(count.to_string()),
                        )
                        .when(count == 0 && self.columns.known_empty(column), |header| {
                            header.child(self.collapse_empty_column(column, cx))
                        }),
                );
            if count == 0 && self.columns.known_empty(column) {
                lane = lane.child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.text_muted)
                        .child("No cards"),
                );
            }
            for scope in scopes {
                lane = lane.child(self.card(scope, column, binding, cx));
            }
            for card in local_cards {
                lane = lane.child(self.local_card(card, cx));
            }
            if column == Column::Queue {
                lane = lane.child(self.add_card_button(cx));
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
        if collapsed > 0 {
            if self
                .columns
                .collapsed(Column::Queue, counts[Column::Queue as usize])
            {
                compact = compact.child(self.add_card_button(cx));
            }
            root = root.child(compact);
        }
        if counts.iter().all(|count| *count == 0)
            && [
                Column::Queue,
                Column::Working,
                Column::Checks,
                Column::Ready,
            ]
            .iter()
            .all(|column| self.columns.collapsed(*column, 0))
        {
            root = root.child(
                div()
                    .debug_selector(|| "board-empty-state".into())
                    .p_3()
                    .rounded_md()
                    .bg(colors.surface_raised)
                    .text_color(colors.text_muted)
                    .child(if self.filter.is_some() {
                        "No cards for this executor. Expand a column to view it."
                    } else {
                        "No cards yet. Add a card to get started, or expand a column."
                    }),
            );
        }
        let verified = binding
            .issues
            .iter()
            .filter(|scope| {
                self.delivery_column(scope) == Column::Verified
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
        let forgejo_problem = source_problem("Forgejo", &self.snapshot.forgejo, 30, now());
        let line = problem_line(forgejo_problem.into_iter().collect()).unwrap_or_default();
        // Keep the quiet source line at the viewport's lower edge. Its reserved
        // bottom padding preserves card geometry without restoring a header.
        root.pb_8()
            .child(columns.child(end.child(self.source_indicator(cx)).child(toggle)))
            .child(
                div()
                    .debug_selector(|| "board-live-status".into())
                    .absolute()
                    .bottom_2()
                    .left_4()
                    .right_4()
                    .h(px(16.))
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.))
                    .text_color(colors.text_muted)
                    .child(line),
            )
            .into_any_element()
    }
}

impl Reader {
    fn return_to_project_board(&mut self, cx: &mut Context<Self>) {
        if !self.current_rel.is_empty() {
            let position = self.retained_document_position(cx);
            self.navigation.places.insert(
                (self.vault_root.clone(), self.current_rel.clone()),
                position,
            );
        }
        self.cancel_pending_landing();
        self.navigation.preparation_generation =
            self.navigation.preparation_generation.wrapping_add(1);
        self.project_navigation.board_visible = true;
        cx.notify();
    }

    pub(crate) fn ensure_project_board(
        &mut self,
        area: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_project_board_using(area, okilum_t3::board::live::reader(), window, cx);
    }
    fn ensure_project_board_using(
        &mut self,
        area: PathBuf,
        read_issues: impl Fn(&Binding) -> Result<Vec<okilum_t3::board::Issue>, String>
            + Send
            + Sync
            + 'static,
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
                    #[cfg(target_os = "linux")]
                    let result = this.project.clone().ok_or(okilum_t3::draft_handoff::Refusal::Unavailable)
                        .and_then(|manager| {
                            if manager.read(cx).area.as_deref() != Some(owner.as_path()) {
                                return Err(okilum_t3::draft_handoff::Refusal::WrongTarget);
                            }
                            // Discuss explicitly opens the existing panel first. It never
                            // selects/creates a thread or inserts into a hidden fallback.
                            this.panels.open(reader_layout::Panel::Backlinks);
                            let available = if this.body_bounds.size.width > px(0.) {
                                f32::from((this.body_bounds.size.width + window.viewport_size().width
                                    - this.body_viewport_width).max(px(0.)))
                            } else { f32::from(window.viewport_size().width) };
                            let visible = this.project_open && this.project_navigation.active.is_some()
                                && this.workspace_panels().visible(reader_layout::Panel::Backlinks, available);
                            manager.update(cx, |manager, cx| {
                                manager.draft_handoff_surface_visible(visible, cx);
                                manager.append_board_context(&owner, text.clone(), window, cx)
                            })
                        });
                    #[cfg(target_os = "linux")]
                    let notice = match result {
                        Ok(()) => {
                            "Context appended to the existing Manager draft. Review and send when ready; Undo removes the block."
                        }
                        Err(okilum_t3::draft_handoff::Refusal::StaleAcknowledgement) =>
                            "Manager target changed. Check its draft before trying again.",
                        Err(_) => "Manager draft unavailable. Open the Manager and try again.",
                    };
                    // No supported atomic web composer bridge is published. Never
                    // report success after changing an invisible native fallback.
                    #[cfg(target_os = "macos")]
                    let notice = "Draft handoff to the Manager webview is unavailable. No draft was changed.";
                    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                    let notice = "Draft handoff to this Manager panel is unavailable. No draft was changed.";
                    board.update(cx, |board, cx| {
                        board.notice = Some(notice.into());
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
                                    this.return_to_project_board(cx);
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

#[cfg(all(test, target_os = "linux"))]
mod discuss_tests;
