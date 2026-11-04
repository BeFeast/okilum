//! Native project center. Source reads are independent of the existing Manager poller.
use super::*;
use okilum_t3::board::{Binding, Column, Snapshot, Source};
use std::time::{SystemTime, UNIX_EPOCH};

mod appearance;
mod card;
mod columns;
mod controls;
mod delivery;
mod filters;
#[cfg(feature = "project-ui-harness")]
pub(super) mod fixture;
mod kanban;
#[cfg(test)]
mod layout_work;
mod list;
mod live;
mod local;
mod manage;
mod notes;
mod row_bits;
pub(super) mod waiting;

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
    _manager_observer: Option<Subscription>,
    #[cfg(target_os = "linux")]
    _manager_feed_observer: Option<Subscription>,
    #[cfg(target_os = "linux")]
    manager_publication: Option<(Option<ServerIdentity>, Option<String>, u64)>,
    #[cfg(target_os = "linux")]
    manager_display: live::ManagerDisplay,
    active: bool,
    ticker: Option<Task<()>>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    resizing: bool,
    verified_open: bool,
    filter: filters::Model,
    filter_ui: filters::Controls,
    filter_cache: filters::Cache,
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
    kanban: kanban::State,
    #[cfg(test)]
    pub(in crate::project) card_calls: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(in crate::project) render_calls: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(in crate::project) frame_work: std::cell::RefCell<Vec<usize>>,
    #[cfg(test)]
    pub(in crate::project) frame_layout_work: std::cell::RefCell<Vec<usize>>,
    #[cfg(test)]
    pub(in crate::project) lane_preparations: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(in crate::project) card_focus:
        std::cell::RefCell<std::collections::BTreeMap<u64, FocusHandle>>,
    waiting: okilum_t3::board::waiting::Projection,
    waiting_page: usize,
}
#[derive(Clone)]
pub(crate) enum Action {
    Note(String),
    Discuss(String),
}
impl EventEmitter<Action> for Board {}
impl Board {
    fn card_rendered(&self) {
        #[cfg(test)]
        {
            self.card_calls.set(self.card_calls.get() + 1);
            *self
                .frame_work
                .borrow_mut()
                .last_mut()
                .expect("Board render frame") += 1;
        }
    }
    fn track_card_layout(&self, card: AnyElement, cx: &Context<Self>) -> AnyElement {
        #[cfg(test)]
        let card = layout_work::Row {
            child: card,
            board: cx.entity().downgrade(),
        }
        .into_any_element();
        #[cfg(not(test))]
        let _ = cx;
        card
    }
}
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let owner = area.to_string_lossy().into_owned();
        let list = list::State::load(&area, cx);
        let (filter, filter_ui) = filters::Controls::new(&area, window, cx);
        Self {
            area,
            binding: None,
            read_issues: Arc::new(read_issues),
            binding_error: None,
            binding_read: None,
            binding_loading: false,
            snapshot: Snapshot::new(owner),
            manager: None,
            _manager_observer: None,
            #[cfg(target_os = "linux")]
            _manager_feed_observer: None,
            #[cfg(target_os = "linux")]
            manager_publication: None,
            #[cfg(target_os = "linux")]
            manager_display: live::ManagerDisplay::default(),
            active: false,
            ticker: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            resizing: false,
            verified_open: false,
            filter,
            filter_ui,
            filter_cache: Default::default(),
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
            kanban: kanban::State::default(),
            #[cfg(test)]
            card_calls: std::cell::Cell::new(0),
            #[cfg(test)]
            render_calls: std::cell::Cell::new(0),
            #[cfg(test)]
            frame_work: std::cell::RefCell::new(Vec::new()),
            #[cfg(test)]
            frame_layout_work: std::cell::RefCell::new(Vec::new()),
            #[cfg(test)]
            lane_preparations: std::cell::Cell::new(0),
            #[cfg(test)]
            card_focus: std::cell::RefCell::new(std::collections::BTreeMap::new()),
            waiting: Default::default(),
            waiting_page: 0,
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
                                this.filter_cache.invalidate();
                                this.binding = binding;
                                this.refresh_waiting();
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
        self.filter_cache.invalidate();
        self.delivery_loading = false;
        self.notes_loading = false;
        self.manager = Some(manager.clone());
        self._manager_observer = Some(cx.observe(&manager, |this, _, cx| {
            #[cfg(target_os = "linux")]
            this.sync_manager_display(cx);
            #[cfg(not(target_os = "linux"))]
            cx.notify();
        }));
        #[cfg(target_os = "linux")]
        {
            self.manager_publication = None;
            let feed = manager.read(cx).feed.clone();
            self._manager_feed_observer =
                Some(cx.observe(&feed, |this, _, cx| this.sync_manager_display(cx)));
            self.sync_manager_display(cx);
        }
        if self.active {
            self.refresh(window, cx);
        }
    }
    #[cfg(target_os = "linux")]
    fn sync_manager_display(&mut self, cx: &mut Context<Self>) {
        let Some(manager) = &self.manager else {
            return;
        };
        let manager = manager.read(cx);
        let publication = (
            manager.identity.clone(),
            manager.chat.as_ref().map(|chat| chat.thread_id.clone()),
            manager.feed.read(cx).typed_revision(),
        );
        if self.manager_publication.as_ref() == Some(&publication) {
            return;
        }
        let display = live::ManagerDisplay::read(manager.identity.as_ref(), manager.chat.as_ref());
        self.manager_publication = Some(publication);
        if self.manager_display != display {
            self.manager_display = display;
            cx.notify();
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
                        this.filter_cache.invalidate();
                        this.refresh_waiting();
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
            self.filter_cache.invalidate();
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
                        this.filter_cache.invalidate();
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
        self.projection_time_at(now())
    }
    fn projection_time_at(&self, at: u64) -> u64 {
        if self.snapshot.forgejo.error.is_some() {
            self.snapshot.forgejo.last_success.unwrap_or(at)
        } else {
            at
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
                        .child(div().flex_1()),
                )
                .child(cards)
                .into_any_element(),
        )
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
        format!(
            "Board: {}\nIssue: {}\nExpected result: {}\nExecutor: {}\nLast issue state: {}\nStage note: {}\n\nQuestion for Manager: ",
            binding.title,
            binding.issue_url(scope.number),
            scope.expected,
            scope.executor,
            issue.map(|i| i.state.as_str()).unwrap_or("not read"),
            self.area.join(&binding.stage_note).display()
        )
    }
}
impl Render for Board {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(all(target_os = "linux", feature = "project-ui-harness"))]
        let _work = super::performance::Work::start(0);
        #[cfg(test)]
        {
            self.render_calls.set(self.render_calls.get() + 1);
            self.frame_work.borrow_mut().push(0);
            self.frame_layout_work.borrow_mut().push(0);
        }
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
            }));
        let Some(_) = self.binding.as_ref() else {
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
        let root = self.render_kanban(window, cx);
        #[cfg(all(target_os = "linux", feature = "project-ui-harness"))]
        let root = super::performance::Trace::new(root, 0).into_any_element();
        root
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
                    let resizing = self.resizing_panel.is_some();
                    board.update(cx, |board, cx| {
                        if board.resizing != resizing {
                            board.resizing = resizing;
                            cx.notify();
                        }
                    });
                    return div()
                        .size_full()
                        .child(board.clone().cached(StyleRefinement::default().size_full()))
                        .into_any_element();
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

#[cfg(all(target_os = "linux", any(test, feature = "project-ui-harness")))]
impl Board {
    #[cfg(test)]
    pub(in crate::project) fn performance_lane_offset(&self, column: Column) -> Point<Pixels> {
        self.kanban.lanes[column as usize]
            .0
            .borrow()
            .base_handle
            .offset()
    }
    #[cfg(test)]
    pub(in crate::project) fn performance_resizing(&self) -> bool {
        self.resizing
    }
    #[cfg(test)]
    pub(in crate::project) fn performance_selected(&self) -> Option<u64> {
        self.selected
    }
    pub(in crate::project) fn performance_fixture(
        area: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut board = Self::with_reader(
            area,
            |_| panic!("synthetic Board must never read a server"),
            window,
            cx,
        );
        board.list.preferences.mode = list::Mode::Board;
        let mut source: serde_json::Value = serde_json::from_str(include_str!(
            "../../../okilum-t3/tests/fixtures/board-legacy.yaml"
        ))
        .unwrap();
        source["issues"] = serde_json::json!([]);
        source["attention"] = serde_json::json!([]);
        source["stage_note"] = serde_json::json!("home.md");
        let mut binding = Binding::parse(&source.to_string()).unwrap();
        for number in 1..=240 {
            let value = serde_json::json!({
                "number":number,"expected":format!("Synthetic task {number}: retain source identity and verify the next delivery with a realistic wrapping title"),
                "executor":"Fixture", "notes":["home.md"],"pull_requests":[],
                "stage": if number % 4 == 0 { serde_json::json!("ready") } else { serde_json::Value::Null }
            });
            binding
                .issues
                .push(serde_json::from_value(value.clone()).unwrap());
            source["issues"].as_array_mut().unwrap().push(value);
            board.snapshot.forgejo.value.insert(
                number,
                okilum_t3::board::Issue {
                    number,
                    title: format!("Synthetic task {number}"),
                    updated_at: chrono::DateTime::from_timestamp(1_700_000_000 - number as i64, 0)
                        .unwrap()
                        .to_rfc3339(),
                    state: "open".into(),
                    body: String::new(),
                    labels: vec![],
                    open_pull_requests: if number % 4 == 2 {
                        vec![1000 + number]
                    } else {
                        vec![]
                    },
                    live_pulls: if matches!(number % 4, 2 | 3) {
                        vec![okilum_t3::board::live::Pull {
                            number: 1000 + number,
                            merged_at: None,
                            checks: if number % 4 == 2 {
                                "success"
                            } else {
                                "pending"
                            }
                            .into(),
                        }]
                    } else {
                        vec![]
                    },
                },
            );
        }
        let issues: Vec<_> = board.snapshot.forgejo.value.values().cloned().collect();
        board.read_issues = Arc::new(move |_| Ok(issues.clone()));
        // Reader's initial note load may hide the Board. Re-enter through the
        // real Board action and retain the same synthetic sources on refresh.
        std::fs::create_dir_all(board.area.join("ops")).unwrap();
        std::fs::write(board.area.join("ops/board.yaml"), source.to_string()).unwrap();
        board.snapshot.forgejo.last_success = Some(now());
        board.snapshot.t3.last_success = Some(now());
        board.local_cards.last_success = Some(now());
        board.binding = Some(binding);
        board.active = true;
        board
    }
}
