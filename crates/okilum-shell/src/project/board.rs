//! Native project center. Source reads are independent of the existing Manager poller.
use super::*;
use okilum_t3::board::{Binding, Column, Snapshot, Source};
use std::time::{SystemTime, UNIX_EPOCH};

mod appearance;
mod columns;
mod controls;
mod delivery;
mod filters;
#[cfg(feature = "project-ui-harness")]
pub(super) mod fixture;
mod list;
mod live;
mod local;
mod manage;
mod notes;
mod row_bits;
mod virtual_cards;

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
    geometry: Entity<virtual_cards::Geometry>,
    height_refinement_pending: bool,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let owner = area.to_string_lossy().into_owned();
        let list = list::State::load(&area, cx);
        let (filter, filter_ui) = filters::Controls::new(&area, window, cx);
        let geometry = cx.new(|_| virtual_cards::Geometry);
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
            geometry,
            height_refinement_pending: false,
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
    fn card(
        &self,
        scope: &okilum_t3::board::Scope,
        column: Column,
        binding: &Binding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mapping = self
            .binding
            .as_ref()
            .and_then(|binding| okilum_t3::board::live::mapping_problem(binding, scope.number));
        self.card_with_mapping(scope, column, binding, mapping, window, cx)
    }
    fn card_with_mapping(
        &self,
        scope: &okilum_t3::board::Scope,
        column: Column,
        binding: &Binding,
        mapping: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        #[cfg(all(target_os = "linux", feature = "project-ui-harness"))]
        let _work = super::performance::Work::start(1);
        #[cfg(test)]
        {
            self.card_calls.set(self.card_calls.get() + 1);
            *self
                .frame_work
                .borrow_mut()
                .last_mut()
                .expect("Board render frame") += 1;
        }
        let colors = brand::palette(cx);
        let number = scope.number;
        let (disclosure, _focus) =
            controls::disclosure(("board-expand", number as usize), window, cx);
        #[cfg(test)]
        self.card_focus.borrow_mut().insert(number, _focus);
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
        // The lane already resolves this card's width. A vertical block flow
        // preserves natural heights without another flex intrinsic-size pass.
        let mut card = div()
            .block()
            .id(("board-card", number as usize))
            .debug_selector(move || format!("board-card-{number}"))
            .w_full()
            .min_w_0()
            .min_h_0()
            .flex_shrink_0()
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
                disclosure
                    .debug_selector(move || format!("board-expand-{number}"))
                    .accessibility_label(format!("Expand issue {number}"))
                    .w_full()
                    .min_w_0()
                    .h_auto()
                    // The control itself owns the full-width block flow.
                    .block()
                    .p_3()
                    .whitespace_normal()
                    .text_size(px(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(
                        meta.mb_2()
                            .font_weight(FontWeight::NORMAL)
                            .whitespace_nowrap(),
                    )
                    .child(scope.expected.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_local = None;
                        this.selected = (this.selected != Some(number)).then_some(number);
                        cx.notify();
                    })),
            )
            .child(if let Some(agent) = &agent {
                #[cfg(target_os = "linux")]
                let (url, progress) = (
                    self.manager_display
                        .identity
                        .as_ref()
                        .map(|identity| identity.thread_url(&agent.thread.thread_id)),
                    self.manager_display.progress(scope, agent.thread),
                );
                #[cfg(not(target_os = "linux"))]
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
                controls::agent_link(
                    ("board-agent-link", number as usize),
                    url.is_none(),
                    window,
                    cx,
                )
                .mt_2()
                .debug_selector(move || format!("board-agent-link-{number}"))
                .w_full()
                .min_w_0()
                .text_color(if stuck {
                    colors.warning
                } else {
                    colors.text_muted
                })
                .managed_tooltip(|window, cx| {
                    gpui_component::tooltip::Tooltip::new("Open the explicitly bound agent thread")
                        .build(window, cx)
                })
                .child(text)
                .on_click(move |_, _, cx| {
                    if let Some(url) = &url {
                        cx.open_url(url);
                    }
                })
                .into_any_element()
            } else {
                h_flex()
                    .mt_2()
                    .min_w_0()
                    .gap_2()
                    .px_3()
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .debug_selector(move || format!("board-agent-{number}"))
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
                    .child(scope.executor.clone())
                    .child(
                        div()
                            .debug_selector(move || format!("board-state-{number}"))
                            .font_weight(FontWeight::NORMAL)
                            .min_w_0()
                            .truncate()
                            .child(if stuck { "Blocked" } else { column.label() }),
                    )
                    .into_any_element()
            })
            .child(div().mt_2().h(px(2.)));
        // A single explicit PR link is a link, not a fabricated CI/merge status.
        if let Some(pr) = scope.pull_requests.first() {
            let url = binding.pull_url(*pr);
            let label: SharedString = format!("PR #{pr}").into();
            card = card.child(
                h_flex().mt_2().child(
                    controls::compact_action(("board-pr-badge", number as usize), window, cx)
                        .debug_selector(move || format!("board-pr-badge-{number}"))
                        .self_start()
                        .ml_2()
                        .mb_2()
                        .min_w_0()
                        .accessibility_label(label.clone())
                        .child(
                            div()
                                .min_w_0()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .line_height(gpui::relative(1.))
                                .child(label),
                        )
                        .child(appearance::icon("git-pull-request", colors.accent))
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                ),
            );
        }
        if let Some(problem) = mapping {
            card = card.child(
                div()
                    .mt_2()
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
                card = card.child(
                    div()
                        .mt_2()
                        .child(self.delivery_details(scope, binding, cx)),
                );
            }
            let url = binding.issue_url(number);
            let context = self.context(scope);
            let mut actions = h_flex()
                .mt_2()
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
                        .mt_2()
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
        let projection = self.filtered_projection();
        let binding = &projection.binding;
        root = root.child(self.filter_bar(&projection, window, cx));
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
        if self.filter.is_active()
            && (!self.binding_loading || self.binding_read.is_some())
            && (!self.local_loading || self.local_cards.last_success.is_some())
            && (!self.snapshot.busy() || self.snapshot.forgejo.last_success.is_some())
            && projection.counts.iter().all(|n| *n == 0)
        {
            return root.child(self.filter_empty(cx)).into_any_element();
        }
        let counts = projection.counts;
        let shared_binding = projection.binding.clone();
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
            // The row is scroll content, so it must keep its natural height.
            // Avoid asking flexbox for the min-content height of every rich
            // card while trying (and failing) to shrink the entire Board.
            .flex_shrink_0()
            .min_h_0()
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
            let scopes = &projection.scopes[column as usize];
            let local_cards = projection.locals[column as usize]
                .iter()
                .map(|index| &self.local_cards.value.cards[*index]);
            let count = counts[column as usize];
            // Counts and membership come from the same cached projection.
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
                        .text_size(px(12.))
                        .text_color(colors.text_muted)
                        .gap_2()
                        .flex_wrap()
                        .child(appearance::icon(glyph, color))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(13.))
                                .text_color(colors.text)
                                .child(column.label()),
                        )
                        .child(count.to_string())
                        .when(count == 0 && self.columns.known_empty(column), |header| {
                            header.child(self.collapse_empty_column(column, cx))
                        })
                        .when(column == Column::Queue, |header| {
                            header.child(
                                div()
                                    .debug_selector(|| "board-queue-order-hint".into())
                                    .text_size(px(12.))
                                    .text_color(colors.text_muted)
                                    .child("от Олега сначала"),
                            )
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
            let mut cards = Vec::new();
            for &index in scopes {
                let scope = &binding.issues[index];
                cards.push(virtual_cards::Card::issue(
                    &cx.entity(),
                    scope,
                    index,
                    column,
                    shared_binding.clone(),
                    self.selected == Some(scope.number),
                ));
            }
            for card in local_cards {
                cards.push(virtual_cards::Card::local(
                    &cx.entity(),
                    card,
                    self.selected_local.as_ref() == Some(&card.id),
                ));
            }
            if !cards.is_empty() {
                lane = lane.child(virtual_cards::Lane::new(column, cards, projection.clone()));
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
                    .child("No cards yet. Add a card to get started, or expand a column."),
            );
        }
        let verified = counts[Column::Verified as usize];

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
        let root = root
            .pb_8()
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
            .into_any_element();
        let root = div()
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(root)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size(px(1.))
                    .child(self.geometry.clone()),
            )
            .into_any_element();
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
