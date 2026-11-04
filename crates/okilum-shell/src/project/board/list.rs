//! One virtualised, fixed-height stream. Only snapshot publication, filter and
//! collapse changes rebuild the row index; scrolling never projects all issues.
use super::*;
#[cfg(feature = "project-ui-harness")]
mod fixture;
mod preferences;
mod waiting;
pub(super) use preferences::Mode;
use preferences::Preferences;

const HEIGHT: f32 = 38.;
const STAGES: [Column; 5] = [
    Column::Working,
    Column::Checks,
    Column::Ready,
    Column::Queue,
    Column::Verified,
];
pub(super) fn label(stage: Column) -> &'static str {
    match stage {
        Column::Working => "В работе",
        Column::Checks => "Проверки",
        Column::Ready => "Можно попробовать",
        Column::Queue => "Очередь",
        Column::Verified => "Проверено",
    }
}
#[derive(Clone, Copy, Debug)]
pub(super) enum Row {
    WaitingGroup,
    Waiting(usize),
    Group(Column),
    Issue(usize),
    Local(usize),
    Problem(usize),
}
pub(super) struct Prepared {
    pub binding: Arc<Binding>,
    pub rows: Vec<Row>,
    pub counts: [usize; 5],
    pub groups: [Arc<[Row]>; 5],
    pub projection: Arc<filters::Projection>,
    collapsed: [bool; 5],
    waiting_collapsed: bool,
    pub local_indices: std::collections::BTreeMap<String, usize>,
    pub source_problem: Option<String>,
    pub sources_ready: bool,
    pub mappings: std::collections::BTreeMap<u64, Option<&'static str>>,
    invalid_annotations: std::collections::BTreeSet<u64>,
    pub issue_indices: std::collections::BTreeMap<u64, usize>,
}
pub(super) struct State {
    pub preferences: Preferences,
    path: Option<PathBuf>,
    prepared: Option<Arc<Prepared>>,
    pub scroll: gpui::UniformListScrollHandle,
    #[cfg(test)]
    publications: usize,
    #[cfg(test)]
    rendered_rows: usize,
}
impl State {
    pub fn load(area: &Path, cx: &App) -> Self {
        let project = area.canonicalize().unwrap_or_else(|_| area.to_path_buf());
        let path = preferences::path(&project, cx);
        let preferences = path
            .as_ref()
            .and_then(|p| match Preferences::load(p, project.clone()) {
                Ok(prefs) => Some(prefs),
                Err(error) => {
                    eprintln!("Board presentation: {error:#}");
                    None
                }
            })
            .unwrap_or_else(|| Preferences::new(project));
        Self {
            preferences,
            path,
            prepared: None,
            scroll: gpui::UniformListScrollHandle::new(),
            #[cfg(test)]
            publications: 0,
            #[cfg(test)]
            rendered_rows: 0,
        }
    }
    #[cfg(test)]
    pub(super) fn publication_count(&self) -> usize {
        self.publications
    }
    pub(super) fn contains_issue(&self, number: u64) -> bool {
        self.prepared
            .as_ref()
            .is_some_and(|p| p.issue_indices.contains_key(&number))
    }
    pub fn invalidate(&mut self) {
        self.prepared = None;
    }
    fn save(&self) -> anyhow::Result<()> {
        if let Some(path) = &self.path {
            self.preferences.save(path)?;
        }
        Ok(())
    }
}
impl Board {
    pub(super) fn save_presentation(&mut self) {
        if let Err(error) = self.list.save() {
            self.notice = Some(format!("Could not save Board presentation: {error:#}"));
        }
    }
    pub(super) fn view_toggle(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let mut control = h_flex()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(7.))
            .bg(p.surface_raised);
        for (mode, caption, selector) in [
            (Mode::List, "Список", "board-view-list"),
            (Mode::Board, "Доска", "board-view-board"),
        ] {
            control = control.child(
                controls::compact_option(selector, self.list.preferences.mode == mode, window, cx)
                    .debug_selector(move || selector.into())
                    .child(caption)
                    .accessibility_label(format!("{caption} view"))
                    .when(self.list.preferences.mode == mode, |button| {
                        button.bg(p.surface).text_color(p.text)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.list.preferences.mode = mode;
                        this.save_presentation();
                        this.focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        control.into_any_element()
    }
    pub(super) fn prepare_list(&mut self) -> Arc<Prepared> {
        let projection = self.filtered_projection();
        if let Some(prepared) = &self.list.prepared {
            if Arc::ptr_eq(&prepared.projection, &projection)
                && prepared.collapsed == self.list.preferences.collapsed
                && prepared.waiting_collapsed == self.list.preferences.waiting_collapsed
            {
                return prepared.clone();
            }
        }
        let binding = self.binding.as_ref().expect("configured Board");
        let mut annotation_counts = std::collections::BTreeMap::<u64, usize>::new();
        for scope in &binding.issues {
            *annotation_counts.entry(scope.number).or_default() += 1;
        }
        let mut invalid_annotations = binding
            .card_problems
            .iter()
            .filter_map(|p| p.number)
            .collect::<std::collections::BTreeSet<_>>();
        invalid_annotations.extend(
            annotation_counts
                .iter()
                .filter_map(|(n, count)| (*count > 1).then_some(*n)),
        );
        let binding = projection.binding.clone();
        let mut groups: [Vec<Row>; 5] = std::array::from_fn(|column| {
            projection.scopes[column]
                .iter()
                .copied()
                .map(Row::Issue)
                .chain(projection.locals[column].iter().copied().map(Row::Local))
                .collect()
        });

        for i in 0..binding.card_problems.len() {
            groups[Column::Queue as usize].push(Row::Problem(i));
        }
        let counts = projection.counts;
        let kanban_groups = groups.clone().map(Arc::from);
        let mut rows = Vec::new();
        if self.waiting.visible() {
            rows.push(Row::WaitingGroup);
            if !self.list.preferences.waiting_collapsed {
                rows.extend((0..self.waiting.items.len()).map(Row::Waiting));
            }
        }
        for stage in STAGES {
            rows.push(Row::Group(stage));
            if !self.list.preferences.collapsed[stage as usize] {
                rows.append(&mut groups[stage as usize]);
            }
        }
        let mappings = binding
            .issues
            .iter()
            .map(|scope| {
                (
                    scope.number,
                    okilum_t3::board::live::mapping_problem(
                        self.binding.as_ref().unwrap(),
                        scope.number,
                    ),
                )
            })
            .collect();
        let source_problem = self.problem(now());
        let sources_ready = self.column_sources_ready(&binding);
        let prepared = Arc::new(Prepared {
            source_problem,
            sources_ready,
            mappings,
            invalid_annotations,
            issue_indices: binding
                .issues
                .iter()
                .enumerate()
                .map(|(i, s)| (s.number, i))
                .collect(),
            local_indices: projection
                .locals
                .iter()
                .flatten()
                .map(|i| (self.local_cards.value.cards[*i].id.clone(), *i))
                .collect(),
            binding,
            rows,
            counts,
            groups: kanban_groups,
            projection: projection.clone(),
            collapsed: self.list.preferences.collapsed,
            waiting_collapsed: self.list.preferences.waiting_collapsed,
        });
        self.list.prepared = Some(prepared.clone());
        #[cfg(test)]
        {
            self.list.publications += 1;
            self.lane_preparations.set(self.lane_preparations.get() + 1);
        }
        prepared
    }
    pub(super) fn list_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .filter_ui
            .search
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            return;
        }
        match event.keystroke.key.as_str() {
            "up" | "down" => {
                let prepared = self.prepare_list();
                let eligible = |row: &Row| matches!(row, Row::Issue(_) | Row::Local(_));
                let current = prepared.rows.iter().position(|row| match row {
                    Row::Issue(i) => self.selected == Some(prepared.binding.issues[*i].number),
                    Row::Local(i) => {
                        self.selected_local.as_ref() == Some(&self.local_cards.value.cards[*i].id)
                    }
                    _ => false,
                });
                let index = if event.keystroke.key == "down" {
                    (current.map_or(0, |i| i + 1)..prepared.rows.len())
                        .find(|i| eligible(&prepared.rows[*i]))
                } else {
                    (0..current.unwrap_or(prepared.rows.len()))
                        .rev()
                        .find(|i| eligible(&prepared.rows[*i]))
                };
                if let Some(index) = index {
                    match prepared.rows[index] {
                        Row::Issue(i) => {
                            self.selected = Some(prepared.binding.issues[i].number);
                            self.selected_local = None;
                        }
                        Row::Local(i) => {
                            self.selected_local = Some(self.local_cards.value.cards[i].id.clone());
                            self.selected = None;
                        }
                        _ => unreachable!(),
                    }
                    if self.peek.open {
                        self.open_peek(window, cx);
                    }
                    self.list
                        .scroll
                        .scroll_to_item(index, gpui::ScrollStrategy::Nearest);
                }
                self.focus.focus(window, cx);
                cx.stop_propagation();
                cx.notify();
            }
            "enter" | "space" if self.focus.is_focused(window) => {
                if self.selected.is_some() || self.selected_local.is_some() {
                    self.open_peek(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }
            }
            "escape" if self.peek.open => {
                self.close_peek(cx);
                cx.stop_propagation();
                cx.notify();
            }
            _ => {}
        }
    }
    pub(super) fn render_list(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = brand::palette(cx);
        let prepared = self.prepare_list();
        let mut root = v_flex()
            .id("project-board")
            .debug_selector(|| "project-board".into())
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .bg(p.canvas)
            .text_color(p.text)
            .capture_key_down(cx.listener(|this, event, window, cx| {
                this.filter_key(event, window, cx);
            }))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss_peek(window, cx)))
            .on_key_down(cx.listener(|this, event, window, cx| this.list_key(event, window, cx)))
            .child(
                h_flex()
                    .min_h(px(48.))
                    .flex_wrap()
                    .flex_none()
                    .px(px(18.))
                    .gap(px(10.))
                    .border_b_1()
                    .border_color(p.border_subtle)
                    .child(
                        div()
                            .text_size(px(15.))
                            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Задачи"),
                    )
                    .child(div().flex_1().min_w_0().child(self.filter_bar(
                        &prepared.projection,
                        window,
                        cx,
                    )))
                    .child(self.add_card_button(cx))
                    .child(
                        Button::new("board-manage")
                            .debug_selector(|| "board-manage".into())
                            .ghost()
                            .small()
                            .child(appearance::icon("settings-2", p.text_muted))
                            .tooltip("Manage board")
                            .accessibility_label("Manage board")
                            .disabled(self.editor_loading)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_management(window, cx)),
                            ),
                    )
                    .child(self.source_indicator_for(prepared.source_problem.clone(), cx)),
            );
        if let Some(attention) = self.attention(&prepared.binding, cx) {
            root = root.child(div().px_4().py_2().child(attention));
        }
        if let Some(delivery) = self.delivery_summary(&prepared.binding, cx) {
            root = root.child(div().px_4().child(delivery));
        }
        if let Some(notes) = self.render_project_notes(&prepared.binding, cx) {
            root = root.child(div().px_4().child(notes));
        }
        if let Some(notice) = &self.notice {
            root = root.child(
                div()
                    .px_4()
                    .text_size(px(12.))
                    .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                    .child(notice.clone()),
            );
        }
        if self.filter.is_active()
            && (!self.binding_loading || self.binding_read.is_some())
            && (!self.local_loading || self.local_cards.last_success.is_some())
            && (!self.snapshot.busy() || self.snapshot.forgejo.last_success.is_some())
            && prepared.counts.iter().all(|count| *count == 0)
            && !self.waiting.visible()
        {
            return root
                .child(
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .px_4()
                        .child(self.filter_empty(cx)),
                )
                .into_any_element();
        }
        if prepared.counts.iter().all(|count| *count == 0)
            && prepared.sources_ready
            && !self.waiting.visible()
        {
            root = root.child(
                div()
                    .debug_selector(|| "board-empty-state".into())
                    .px_4()
                    .py_2()
                    .text_size(px(12.))
                    .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                    .text_color(p.text_muted)
                    .child("No cards yet. Add a card to get started."),
            );
        }
        let rows = prepared.clone();
        root = root.child(
            div()
                .debug_selector(|| "board-list-viewport".into())
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(
                    gpui::uniform_list(
                        "board-list",
                        prepared.rows.len(),
                        cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            #[cfg(test)]
                            {
                                this.list.rendered_rows += range.len();
                            }
                            range
                                .map(|index| this.list_row(rows.rows[index], &rows, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.list.scroll)
                    .size_full(),
                ),
        );
        let problem = self.waiting.problem.clone().or_else(|| {
            problem_line(
                source_problem("Forgejo", &self.snapshot.forgejo, 30, now())
                    .into_iter()
                    .collect(),
            )
        });
        root.child(
            div()
                .debug_selector(|| "board-live-status".into())
                .h(px(20.))
                .flex_none()
                .px_4()
                .text_size(px(12.))
                .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                .text_color(p.text_muted)
                .truncate()
                .child(
                    div()
                        .when(self.waiting.problem.is_some(), |line| {
                            line.debug_selector(|| "board-waiting-problem".into())
                        })
                        .child(problem.unwrap_or_default()),
                ),
        )
        .into_any_element()
    }
    fn list_row(&self, row: Row, prepared: &Prepared, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        match row {
            Row::WaitingGroup => self.waiting_list_group(cx),
            Row::Waiting(index) => self.waiting_list_row(index, cx),
            Row::Group(stage) => {
                let count = prepared.counts[stage as usize];
                let collapsed = self.list.preferences.collapsed[stage as usize];
                let (tone, icon) = appearance::stage(stage, cx);
                Button::new(("board-list-group", stage as usize))
                    .debug_selector(move || format!("board-list-group-{stage:?}"))
                    .ghost()
                    .w_full()
                    .h(px(HEIGHT))
                    .px(px(18.))
                    .rounded_none()
                    .bg(p.surface)
                    .accessibility_label(format!(
                        "{} {count}, {}",
                        label(stage),
                        if collapsed { "collapsed" } else { "expanded" }
                    ))
                    .child(
                        h_flex()
                            .w_full()
                            .gap(px(9.))
                            .text_size(px(13.))
                            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                            .child(appearance::icon(
                                if collapsed {
                                    "chevron-right"
                                } else {
                                    "chevron-down"
                                },
                                p.text_muted,
                            ))
                            .child(appearance::icon(icon, tone))
                            .child(
                                div()
                                    .debug_selector(move || {
                                        format!("board-list-group-label-{stage:?}")
                                    })
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(label(stage)),
                            )
                            .child(
                                div()
                                    .debug_selector(move || {
                                        format!("board-list-count-{stage:?}-{count}")
                                    })
                                    .text_color(p.text_muted)
                                    .child(count.to_string()),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.list.preferences.collapsed[stage as usize] = !collapsed;
                        this.save_presentation();
                        this.focus.focus(window, cx);
                        cx.notify();
                    }))
                    .into_any_element()
            }
            Row::Issue(index) => self.issue_row(
                &prepared.binding.issues[index],
                &prepared.binding,
                prepared
                    .mappings
                    .get(&prepared.binding.issues[index].number)
                    .copied()
                    .flatten()
                    .filter(|_| {
                        prepared
                            .invalid_annotations
                            .contains(&prepared.binding.issues[index].number)
                    }),
                cx,
            ),
            Row::Local(index) => {
                let card = &self.local_cards.value.cards[index];
                let id = card.id.clone();
                let selector = format!("board-list-local-{id}");
                Button::new(("board-list-local", index))
                    .debug_selector(move || selector.clone())
                    .ghost()
                    .w_full()
                    .h(px(HEIGHT))
                    .rounded_none()
                    .px(px(37.))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .text_size(px(13.))
                            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                            .child(row_bits::type_tag(None, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(card.title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                                    .text_color(p.text_muted)
                                    .child("Local"),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.selected = None;
                        this.selected_local = Some(id.clone());
                        this.open_peek(window, cx);
                        this.focus.focus(window, cx);
                        cx.notify();
                    }))
                    .into_any_element()
            }
            Row::Problem(index) => {
                let problem = &prepared.binding.card_problems[index];
                div()
                    .debug_selector(move || format!("board-card-problem-{index}"))
                    .h(px(HEIGHT))
                    .px(px(37.))
                    .flex()
                    .items_center()
                    .text_size(px(12.))
                    .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                    .text_color(p.warning)
                    .truncate()
                    .child(format!(
                        "{} · {}: {}",
                        problem
                            .number
                            .map(|n| format!("#{n}"))
                            .unwrap_or_else(|| "Card".into()),
                        problem.field,
                        problem.detail
                    ))
                    .into_any_element()
            }
        }
    }
    fn issue_row(
        &self,
        scope: &okilum_t3::board::Scope,
        binding: &Binding,
        mapping: Option<&'static str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = brand::palette(cx);
        let number = scope.number;
        let issue = self.snapshot.forgejo.value.get(&number);
        let agent = live::agent(scope, &self.snapshot.t3.value, now(), 4 * 3600);
        let selected = self.selected == Some(number);
        let mut row = h_flex()
            .w_full()
            .min_w_0()
            .gap(px(10.))
            .text_size(px(13.))
            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                    .text_color(p.text_muted)
                    .child(format!("#{number}")),
            )
            .child(row_bits::type_tag(issue, cx))
            .child(
                div()
                    .debug_selector(move || format!("board-list-title-{number}"))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(
                        issue
                            .map(|i| i.title.clone())
                            .filter(|t| !t.is_empty())
                            .unwrap_or_else(|| scope.expected.clone()),
                    ),
            );
        if let Some(origin) = appearance::origin(issue) {
            row = row.child(row_bits::label_pill(origin, p.warning, cx));
        }
        if let Some(epic) = issue.and_then(|issue| {
            issue.labels.iter().find(|l| {
                l.name == "epic" || l.name.starts_with("epic:") || l.name.starts_with("epic/")
            })
        }) {
            row = row.child(row_bits::label_pill(
                epic.name.clone(),
                brand::reader_palette(cx).callout_important,
                cx,
            ));
        }
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
        let state = if scope.stuck {
            "Blocked".into()
        } else {
            agent
                .as_ref()
                .map(|a| a.state_label().to_owned())
                .unwrap_or_default()
        };

        row = row.child(
            div()
                .debug_selector(move || format!("board-state-{number}"))
                .min_w_0()
                .max_w(px(230.))
                .truncate()
                .text_size(px(12.))
                .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                .text_color(if mapping.is_some() {
                    p.warning
                } else {
                    p.text_muted
                })
                .child(mapping.map(str::to_owned).or(progress).unwrap_or(state)),
        );
        if let Some(pr) = scope.pull_requests.first() {
            let checks = issue
                .and_then(|i| i.live_pulls.iter().find(|pull| pull.number == *pr))
                .map(|p| p.checks.as_str());
            let url = binding.pull_url(*pr);
            row = row.child(
                Button::new(("board-list-pr", number as usize))
                    .accessibility_label(format!("PR {pr} · {}", row_bits::ci_label(checks)))
                    .tooltip(format!("Open PR {pr} · {}", row_bits::ci_label(checks)))
                    .ghost()
                    .small()
                    .flex_none()
                    .child(
                        h_flex()
                            .gap(px(5.))
                            .text_size(px(12.))
                            .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                            .child(row_bits::ci_dot(checks, cx))
                            .child(format!("PR {pr}")),
                    )
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        cx.open_url(&url);
                    }),
            );
        }
        if let Some(agent) = &agent {
            let active = matches!(
                agent.thread.status.as_str(),
                "preparing" | "starting" | "running" | "waiting"
            ) && agent
                .thread
                .active_run_id
                .as_ref()
                .is_some_and(|id| !id.is_empty())
                && !agent.thread.settled;
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
            row = row.child(
                Button::new(("board-agent-link", number as usize))
                    .debug_selector(move || format!("board-agent-link-{number}"))
                    .small()
                    .ghost()
                    .size(px(24.))
                    .p_0()
                    .flex_none()
                    .disabled(url.is_none())
                    .tooltip(format!("{} · {}", scope.executor, agent.label()))
                    .accessibility_label(format!("Agent {}: {}", scope.executor, agent.label()))
                    .child(row_bits::agent_avatar(&scope.executor, active, cx))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        if let Some(url) = &url {
                            cx.open_url(url);
                        }
                    }),
            );
        }
        let stale = scope.stuck || agent.as_ref().is_some_and(|a| a.stuck);
        let time = issue
            .and_then(|i| row_bits::relative_time(&i.updated_at, now()))
            .unwrap_or_else(|| "—".into());
        row = row.child(
            div()
                .debug_selector(move || {
                    if stale {
                        format!("board-list-stale-{number}")
                    } else {
                        format!("board-list-time-{number}")
                    }
                })
                .w(px(if stale { 108. } else { 52. }))
                .flex_none()
                .text_size(px(12.))
                .line_height(relative(row_bits::TEXT_LINE_HEIGHT))
                .text_color(if stale { p.warning } else { p.text_muted })
                .child(if stale {
                    format!("{time} ⚠ долго")
                } else {
                    time
                }),
        );
        Button::new(("board-list-row", number as usize))
            .debug_selector(move || format!("board-list-row-{number}"))
            .ghost()
            .w_full()
            .min_w_0()
            .h(px(HEIGHT))
            .px(px(18.))
            .pl(px(37.))
            .rounded_none()
            .bg(if selected {
                appearance::tint(p.accent)
            } else {
                p.canvas
            })
            .border_b_1()
            .border_color(p.border_subtle.opacity(0.5))
            .accessibility_label(format!(
                "Issue {number}: {}",
                issue.map(|i| i.title.as_str()).unwrap_or(&scope.expected)
            ))
            .child(row)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.selected_local = None;
                this.selected = Some(number);
                this.open_peek(window, cx);
                this.focus.focus(window, cx);
                cx.notify();
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
