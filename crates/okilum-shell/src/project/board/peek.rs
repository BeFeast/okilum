//! Issue details share the existing right dock. The Manager entity and composer
//! remain owned by Reader; switching this presentation never changes a draft.
use super::*;
use gpui_component::tab::{Tab, TabBar};
use okilum_t3::board::peek::Comment;

type CommentsReader = Arc<dyn Fn(&Binding, u64) -> Result<Vec<Comment>, String> + Send + Sync>;
#[derive(Default)]
pub(super) struct State {
    pub open: bool,
    pub manager_tab: bool,
    generation: u64,
    target: Option<Target>,
    loading: bool,
    pub(super) loaded: bool,
    comments: Vec<Comment>,
    error: Option<String>,
    pub(super) reader: Option<CommentsReader>,
}
#[derive(Clone, PartialEq, Eq)]
enum Target {
    Issue(u64),
    Local(String),
}
#[derive(Clone)]
struct Ticket {
    area: PathBuf,
    repository: String,
    issue: u64,
    generation: u64,
}
impl State {
    #[cfg(test)]
    pub fn set_test_reader(&mut self) {
        self.reader = Some(Arc::new(|_, _| Ok(vec![])));
    }
}
impl Board {
    pub(super) fn close_peek(&mut self, cx: &mut Context<Self>) {
        self.peek.open = false;
        self.peek.target = None;
        self.peek.manager_tab = false;
        self.peek.generation = self.peek.generation.wrapping_add(1);
        self.peek.comments.clear();
        self.peek.error = None;
        cx.notify();
    }
    pub(super) fn open_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self
            .selected
            .map(Target::Issue)
            .or_else(|| self.selected_local.clone().map(Target::Local))
        else {
            return;
        };
        if self.peek.open && self.peek.target.as_ref() == Some(&target) {
            self.peek.manager_tab = false;
            cx.emit(Action::Peek);
            cx.notify();
            return;
        }
        self.peek.target = Some(target);
        self.peek.open = true;
        self.peek.manager_tab = false;
        self.peek.generation = self.peek.generation.wrapping_add(1);
        self.peek.loaded = false;
        self.peek.comments.clear();
        self.peek.error = None;
        self.load_peek_comments(window, cx);
        cx.emit(Action::Peek);
        cx.notify();
    }
    fn load_peek_comments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.peek.open || self.peek.loading || self.peek.loaded {
            return;
        }
        let Some(number) = self.selected else { return };
        let Some(binding) = self.binding.clone().filter(|b| !b.repository.is_empty()) else {
            return;
        };
        let ticket = Ticket {
            area: self.area.clone(),
            repository: binding.repository.clone(),
            issue: number,
            generation: self.peek.generation,
        };
        self.peek.loading = true;
        let reader = self
            .peek
            .reader
            .clone()
            .unwrap_or_else(|| Arc::new(okilum_t3::board::peek::comments));
        cx.spawn_in(window, {
            let access = reader_features::access(reader_features::Feature::Agents, cx);
            async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(okilum_t3::scope_network(access, async move {
                        reader(&binding, number)
                    }))
                    .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.complete_peek_comments(&ticket, result, window, cx);
                });
            }
        })
        .detach();
    }
    fn complete_peek_comments(
        &mut self,
        ticket: &Ticket,
        result: Result<Vec<Comment>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.peek.loading = false;
        if self.peek_matches(ticket) {
            self.peek.loaded = true;
            match result {
                Ok(comments) => self.peek.comments = comments,
                Err(error) => self.peek.error = Some(error),
            }
            cx.notify();
        } else {
            // At most one request runs. Rapid selection changes coalesce
            // to the latest issue after the old request finishes.
            self.load_peek_comments(window, cx);
        }
    }
    fn peek_matches(&self, ticket: &Ticket) -> bool {
        self.peek.open
            && self.area == ticket.area
            && self.peek.generation == ticket.generation
            && self.selected == Some(ticket.issue)
            && self
                .binding
                .as_ref()
                .is_some_and(|b| b.repository == ticket.repository)
    }
    pub(super) fn dismiss_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.peek.open {
            self.close_peek(cx);
            self.focus.focus(window, cx);
        } else {
            cx.propagate();
        }
    }
    pub(super) fn peek_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" && self.peek.open {
            self.close_peek(cx);
            self.focus.focus(window, cx);
            cx.stop_propagation();
        }
    }
    pub(super) fn render_peek_dock(
        &mut self,
        manager: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.peek.open {
            return manager;
        }
        let p = brand::palette(cx);
        let tabs = TabBar::new("board-peek-tabs")
            .segmented()
            .small()
            .selected_index(usize::from(self.peek.manager_tab))
            .child(
                Tab::new()
                    .label("Задача")
                    .debug_selector(|| "board-peek-tab".into()),
            )
            .child(
                Tab::new()
                    .label("Manager")
                    .debug_selector(|| "board-peek-manager-tab".into()),
            )
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                this.peek.manager_tab = *index == 1;
                cx.notify();
            }));
        let content = if self.peek.manager_tab {
            manager
        } else {
            self.render_peek(cx)
        };
        v_flex()
            .id("board-peek-dock")
            .debug_selector(|| "board-peek-dock".into())
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(p.surface)
            .text_color(p.text)
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss_peek(window, cx)))
            .on_key_down(cx.listener(|this, event, window, cx| this.peek_key(event, window, cx)))
            .child(
                h_flex()
                    .flex_none()
                    .min_h(px(48.))
                    .px_3()
                    .gap_2()
                    .child(tabs)
                    .child(div().flex_1())
                    .child(
                        Button::new("board-peek-close")
                            .debug_selector(|| "board-peek-close".into())
                            .ghost()
                            .small()
                            .label("Esc закрыть")
                            .accessibility_label("Close issue details")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_peek(cx);
                                this.focus.focus(window, cx);
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().min_w_0().child(content))
            .into_any_element()
    }
    fn render_peek(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let mut body = v_flex()
            .id("board-peek-content")
            .debug_selector(|| "board-peek-content".into())
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .p_4()
            .gap_3()
            .text_size(px(13.))
            .line_height(px(20.));
        if let Some(id) = &self.selected_local {
            if let Some(card) = self
                .local_cards
                .value
                .cards
                .iter()
                .find(|c| &c.id == id)
                .cloned()
            {
                return body
                    .h_full()
                    .child(self.local_details(&card, cx))
                    .into_any_element();
            }
        }
        let projection = self.filtered_projection();
        let binding = &projection.unfiltered;
        let Some(scope) = self
            .selected
            .and_then(|n| binding.issues.iter().find(|scope| scope.number == n))
        else {
            return body
                .child("Задача больше не входит в Board. Закройте панель или выберите другую.")
                .into_any_element();
        };
        let number = scope.number;
        let issue = self.snapshot.forgejo.value.get(&number);
        let mut meta = h_flex()
            .gap_2()
            .flex_wrap()
            .min_w_0()
            .child(div().text_color(p.text_muted).child(format!("#{number}")))
            .child(row_bits::type_tag(issue, cx));
        if let Some(issue) = issue {
            for label in &issue.labels {
                meta = meta.child(row_bits::label_pill(label.name.clone(), p.text_muted, cx));
            }
        }
        body = body.child(meta).child(
            div()
                .debug_selector(|| "board-peek-title".into())
                .text_size(px(17.))
                .line_height(px(23.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(
                    issue
                        .map(|i| i.title.clone())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| scope.expected.clone()),
                ),
        );
        let agent = live::agent(scope, &self.snapshot.t3.value, now(), 4 * 3600);
        let agent_url = agent.as_ref().and_then(|a| {
            self.manager.as_ref().and_then(|m| {
                m.read(cx)
                    .identity
                    .as_ref()
                    .map(|i| i.thread_url(&a.thread.thread_id))
            })
        });
        if let Some(agent) = &agent {
            let tone = if agent.stuck { p.warning } else { p.info };
            let progress = self
                .manager
                .as_ref()
                .and_then(|m| live::progress(scope, agent.thread, m.read(cx).chat.as_ref()));
            body = body.child(
                v_flex()
                    .debug_selector(|| "board-peek-agent".into())
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(tone.opacity(0.4))
                    .bg(appearance::tint(tone))
                    .child(
                        h_flex()
                            .gap_2()
                            .min_w_0()
                            .child(row_bits::agent_avatar(&scope.executor, agent.working(), cx))
                            .child(
                                div()
                                    .text_color(tone)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(format!("Агент · {}", agent.label())),
                            ),
                    )
                    .child(div().text_color(p.text_muted).child(format!(
                        "{} · {}",
                        scope.executor,
                        progress.unwrap_or_else(|| "Текущий шаг не опубликован".into())
                    ))),
            );
        }
        let column = self.delivery_column(scope);
        let (tone, icon) = appearance::stage(column, cx);
        body = body.child(property(
            "Стадия",
            h_flex()
                .gap_1()
                .child(appearance::icon(icon, tone))
                .child(list::label(column)),
            cx,
        ));
        let mut prs = v_flex().gap_2().w_full().min_w_0();
        for pr_number in &scope.pull_requests {
            let pr_number = *pr_number;
            let pull = issue.and_then(|i| i.live_pulls.iter().find(|p| p.number == pr_number));
            let status = pull
                .map(|p| format!("{} · {}", p.state, row_bits::ci_label(Some(&p.checks))))
                .unwrap_or_else(|| "состояние не прочитано".into());
            let title = pull.map(|p| p.title.clone()).unwrap_or_default();
            prs = prs.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .min_w_0()
                            .child(row_bits::ci_dot(pull.map(|p| p.checks.as_str()), cx))
                            .child(link_button(
                                format!("peek-pr-{pr_number}"),
                                format!("#{pr_number} ↗"),
                                binding.pull_url(pr_number),
                            ))
                            .child(div().min_w_0().whitespace_normal().child(status)),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("board-peek-pr-title-{pr_number}"))
                            .w_full()
                            .min_w_0()
                            .whitespace_normal()
                            .line_height(px(20.))
                            .child(title),
                    ),
            );
        }
        if scope.pull_requests.is_empty() {
            prs = prs.child("ещё нет");
        }
        body = body.child(property("PR", prs, cx));
        let beta = beta_rows(self, scope, binding.refresh_seconds);
        body = body.child(property(
            "В beta",
            v_flex()
                .gap_1()
                .children(
                    beta.iter()
                        .map(|b| div().child(format!("{} · {}", b.platform.label(), b.build))),
                )
                .when(beta.is_empty(), |v| v.child("ещё нет")),
            cx,
        ));
        let mut has_related = false;
        let mut related = v_flex().gap_1().min_w_0();
        for (index, note) in scope
            .notes
            .iter()
            .filter(|note| *note != &binding.stage_note)
            .chain((!binding.stage_note.is_empty()).then_some(&binding.stage_note))
            .enumerate()
        {
            has_related = true;
            let note = note.clone();
            related = related.child(
                Button::new(("peek-note", index))
                    .debug_selector(move || format!("board-note-{number}-{index}"))
                    .ghost()
                    .small()
                    .h_auto()
                    .min_h(px(24.))
                    .w_full()
                    .justify_start()
                    .child(
                        div()
                            .whitespace_normal()
                            .line_height(px(20.))
                            .child(note.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.note(&note, cx))),
            );
        }
        if let Some(issue) = issue {
            for referenced in okilum_t3::board::peek::mentions(&issue.body, number) {
                has_related = true;
                related = related.child(link_button(
                    format!("peek-mentioned-{referenced}"),
                    format!("Упомянуто: #{referenced} ↗"),
                    binding.issue_url(referenced),
                ));
            }
        }
        if has_related {
            body = body.child(property("Связано", related, cx));
        }
        body = body
            .child(div().text_color(p.text_muted).child("Ожидаемый результат"))
            .child(div().child(scope.expected.clone()));
        if let Some(problem) = self
            .binding
            .as_ref()
            .and_then(|binding| okilum_t3::board::live::mapping_problem(binding, number))
        {
            body = body.child(
                div()
                    .debug_selector(move || format!("board-mapping-{number}"))
                    .text_color(p.warning)
                    .child(problem),
            );
        }
        let mut timeline = Vec::<(String, String)>::new();
        if let Some(issue) = issue {
            timeline.push((issue.created_at.clone(), "Задача создана".into()));
            for pull in &issue.live_pulls {
                timeline.push((
                    pull.created_at.clone(),
                    format!("PR #{} открыт", pull.number),
                ));
                if pull.updated_at != pull.created_at {
                    timeline.push((
                        pull.updated_at.clone(),
                        format!("PR #{} обновлён", pull.number),
                    ));
                }
                if let Some(time) = pull
                    .merged_at
                    .and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0))
                {
                    timeline.push((time.to_rfc3339(), format!("PR #{} merged", pull.number)));
                }
            }
        }
        for comment in &self.peek.comments {
            timeline.push((
                comment.created_at.clone(),
                format!(
                    "{}: {}",
                    comment.user.login,
                    comment
                        .body
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            ));
        }
        for build in &beta {
            if let Some(date) = &build.published_at {
                timeline.push((
                    date.clone(),
                    format!("В beta · {} · {}", build.platform.label(), build.build),
                ));
            }
        }
        timeline.retain(|(date, _)| chrono::DateTime::parse_from_rfc3339(date).is_ok());
        timeline.sort_by_key(|(date, _)| chrono::DateTime::parse_from_rfc3339(date).ok());
        body = body.child(
            div()
                .text_color(p.text_muted)
                .font_weight(FontWeight::SEMIBOLD)
                .child("Что было"),
        );
        for (date, text) in timeline
            .into_iter()
            .rev()
            .take(16)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            let date = chrono::DateTime::parse_from_rfc3339(&date)
                .unwrap()
                .with_timezone(&chrono::Local)
                .format("%d.%m %H:%M")
                .to_string();
            body = body.child(div().child(format!("• {date} · {text}")));
        }
        if let Some(message) = self
            .peek
            .error
            .clone()
            .or_else(|| (!self.peek.loaded).then(|| "Комментарии загружаются…".into()))
        {
            body = body.child(
                div()
                    .text_size(px(12.))
                    .text_color(p.text_muted)
                    .child(message),
            );
        }
        body = body.child(link_button(
            "peek-full-history".into(),
            "Полная история — в Issue ↗".into(),
            binding.issue_url(number),
        ));
        if let Some(issue) = issue.filter(|i| !i.body.is_empty()) {
            body = body
                .child(div().text_color(p.text_muted).child("Описание"))
                .child(div().child(issue.body.chars().take(6000).collect::<String>()));
        }
        if let Some(delivery) = self.delivery_details(scope, binding, cx) {
            body = body.child(delivery);
        }
        for (index, build) in scope.builds.iter().enumerate() {
            body = body.child(link_button(
                format!("peek-build-{index}"),
                build.label.clone(),
                build.url.clone(),
            ));
        }
        let context = self.context(scope);
        let actions = h_flex()
            .flex_wrap()
            .gap_2()
            .p_4()
            .flex_none()
            .child(
                Button::new("peek-discuss")
                    .debug_selector(move || format!("board-discuss-{number}"))
                    .primary()
                    .small()
                    .label("Обсудить с Manager")
                    .tooltip("Append to the Manager draft; Undo available; never sends")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.peek.manager_tab = true;
                        cx.emit(Action::Discuss(context.clone()));
                        cx.notify();
                    })),
            )
            .child(link_button(
                "peek-issue".into(),
                "Issue ↗".into(),
                binding.issue_url(number),
            ))
            .child(optional_link(
                "peek-open-pr",
                "PR ↗",
                scope.pull_requests.first().map(|pr| binding.pull_url(*pr)),
            ))
            .child(optional_link("peek-open-agent", "Агент ↗", agent_url));
        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(body)
            .child(actions)
            .into_any_element()
    }
}
fn property(label: &'static str, value: impl IntoElement, cx: &App) -> AnyElement {
    h_flex()
        .debug_selector(move || format!("board-peek-property-{label}"))
        .items_start()
        .gap_2()
        .min_w_0()
        .child(
            div()
                .line_height(px(20.))
                .min_h(px(20.))
                .w(px(88.))
                .flex_none()
                .text_color(brand::palette(cx).text_muted)
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(value))
        .into_any_element()
}
fn link_button(id: String, text: String, url: String) -> Button {
    Button::new(SharedString::from(id))
        .ghost()
        .small()
        .h_auto()
        .min_h(px(24.))
        .child(
            div()
                .whitespace_normal()
                .text_size(px(12.))
                .line_height(px(20.))
                .child(text),
        )
        .on_click(move |_, _, cx| cx.open_url(&url))
}
fn optional_link(id: &'static str, label: &'static str, url: Option<String>) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .label(label)
        .disabled(url.is_none())
        .on_click(move |_, _, cx| {
            if let Some(url) = &url {
                cx.open_url(url);
            }
        })
}
fn beta_rows(
    board: &Board,
    scope: &okilum_t3::board::Scope,
    interval: u64,
) -> Vec<okilum_t3::board::delivery::Build> {
    use okilum_t3::board::delivery as d;
    d::Platform::ALL
        .into_iter()
        .flat_map(|platform| board.delivery.rows(scope, platform, now(), interval))
        .filter(|row| row.track.1 == d::Channel::Beta && row.ready)
        .filter_map(|row| row.build)
        .collect()
}
impl Reader {
    pub(crate) fn project_issue_peek_visible(&self, cx: &App) -> bool {
        self.project_navigation
            .active
            .as_ref()
            .and_then(|p| self.project_navigation.boards.get(&p.folder))
            .is_some_and(|board| {
                let board = board.read(cx);
                board.peek.open && !board.peek.manager_tab
            })
    }
    pub(crate) fn wrap_project_peek(
        &self,
        manager: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(board) = self
            .project_navigation
            .active
            .as_ref()
            .and_then(|p| self.project_navigation.boards.get(&p.folder))
        {
            board.update(cx, |board, cx| board.render_peek_dock(manager, cx))
        } else {
            manager
        }
    }
}
#[cfg(test)]
mod tests;

/// The same dock renderer around a synthetic Board, for native test windows.
#[cfg(any(test, feature = "project-ui-harness"))]
pub(super) struct Harness {
    pub board: Entity<Board>,
}
#[cfg(any(test, feature = "project-ui-harness"))]
impl Harness {
    pub fn new(board: Entity<Board>, cx: &mut Context<Self>) -> Self {
        cx.observe(&board, |_, _, cx| cx.notify()).detach();
        Self { board }
    }
}
#[cfg(any(test, feature = "project-ui-harness"))]
impl Render for Harness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.board.read(cx).peek.open;
        let mut view = h_flex()
            .size_full()
            .min_h_0()
            .child(div().flex_1().min_w_0().h_full().child(self.board.clone()));
        if open {
            let dock = self.board.update(cx, |b, cx| {
                b.render_peek_dock(
                    div()
                        .debug_selector(|| "fixture-manager".into())
                        .child("Manager draft")
                        .into_any_element(),
                    cx,
                )
            });
            view = view.child(div().w(px(420.)).h_full().flex_none().child(dock));
        }
        view
    }
}
