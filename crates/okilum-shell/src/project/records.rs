//! Variant A project tabs and read-only Markdown records (#1892).
use super::*;
use okilum_core::agent_rules::Refresh;
use okilum_t3::records::{Kind, Record, Snapshot};

pub(crate) struct RecordsView {
    area: PathBuf,
    pub(crate) kind: Option<Kind>,
    visible: bool,
    refresh: Refresh,
    snapshot: Snapshot,
    selected: Option<String>,
    query: Entity<InputState>,
    _query: Subscription,
    _publication: Option<Subscription>,
    content: Entity<TextViewState>,
    list_scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    pub(super) notice: Option<String>,
}
impl EventEmitter<board::Action> for RecordsView {}

impl Reader {
    pub(crate) fn ensure_project_records(
        &mut self,
        area: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project_navigation.records.contains_key(area) {
            return;
        }
        let records = cx.new(|cx| RecordsView::new(area.to_owned(), window, cx));
        let mut published = self.vault.clone();
        let mut epoch = self.source_publication_epoch;
        let root = self.vault_root.clone();
        let refreshed = records.downgrade();
        let publication = cx.observe(&cx.entity(), move |this, _, cx| {
            if this.vault_root != root
                || (Arc::ptr_eq(&published, &this.vault) && epoch == this.source_publication_epoch)
            {
                return;
            }
            published = this.vault.clone();
            epoch = this.source_publication_epoch;
            let _ = refreshed.update(cx, |records, cx| records.changed(cx));
        });
        records.update(cx, |records, _| records._publication = Some(publication));
        let owner = area.to_owned();
        cx.subscribe(&records, move |this, _, action: &board::Action, cx| {
            if this.project_navigation.active.as_ref().map(|p| &p.folder) != Some(&owner) {
                return;
            }
            if let Some(board) = this.project_navigation.boards.get(&owner) {
                board.update(cx, |_, cx| cx.emit(action.clone()));
            }
        })
        .detach();
        cx.observe(&records, |this, _, cx| this.project_board_changed(cx))
            .detach();
        self.project_navigation
            .records
            .insert(area.to_owned(), records);
    }
    pub(crate) fn render_project_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(area) = self
            .project_navigation
            .active
            .as_ref()
            .map(|p| p.folder.clone())
        else {
            return div().into_any_element();
        };
        let selected = self
            .project_navigation
            .records
            .get(&area)
            .and_then(|r| r.read(cx).kind);
        let mut tabs = h_flex()
            .w_full()
            .flex_shrink_0()
            .min_w_0()
            .px_3()
            .py_1()
            .gap_1()
            .flex_wrap()
            .debug_selector(|| "project-view-tabs".into())
            .border_b_1()
            .border_color(cx.theme().border);
        for kind in std::iter::once(None).chain(Kind::ALL.into_iter().map(Some)) {
            let label = kind.map_or("Board", Kind::label);
            let owner = area.clone();
            tabs = tabs.child(
                Button::new(SharedString::from(format!("project-tab-{label}")))
                    .debug_selector(move || format!("project-tab-{label}"))
                    .ghost()
                    .small()
                    .label(label)
                    .selected(selected == kind)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.project_navigation.active.as_ref().map(|p| &p.folder)
                            != Some(&owner)
                        {
                            return;
                        }
                        if let Some(records) = this.project_navigation.records.get(&owner) {
                            records.update(cx, |records, cx| records.select(kind, window, cx));
                        }
                        this.sync_project_board_visibility(window, cx);
                        cx.notify();
                    })),
            );
        }
        tabs.into_any_element()
    }
}

impl ProjectView {
    /// Records are also useful before pairing. Append to this project's visible
    /// editable input, preserving its Undo; no thread is created and no send is prepared.
    pub(super) fn append_record_context(
        &mut self,
        area: &Path,
        text: String,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), okilum_t3::draft_handoff::Refusal> {
        use okilum_t3::draft_handoff::Refusal;
        if self.area.as_deref() != Some(area) {
            return Err(Refusal::WrongTarget);
        }
        if !visible
            || !self.active
            || self.section != ProjectSection::Manager
            || self.executors_open
            || self.local_setup.is_some()
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
        {
            return Err(Refusal::Unavailable);
        }
        if text.trim().is_empty() {
            return Err(Refusal::Empty);
        }
        #[cfg(native_manager)]
        {
            if self.draft_handoff_target(cx).is_some() {
                return self.append_board_context(area, text, window, cx);
            }
            // An established typed conversation uses its existing exact-owner
            // handoff. A disconnected, never-bound composer has no remote owner.
            if !self.draft_unbound_context_allowed()
                || self.composer.read(cx).typed_scope().is_some()
            {
                return Err(Refusal::Unavailable);
            }
        }
        let input = self.compose.read(cx);
        if !input.is_editable()
            || input.presentation().is_disabled()
            || input.ime_settling(cx)
            || input.completion_active(cx)
        {
            return Err(Refusal::Unavailable);
        }
        self.compose.update(cx, |input, cx| {
            let end = input.value().len();
            input.set_selected_range(end..end, cx);
            input.insert(
                format!("{}---\n{text}", if end == 0 { "" } else { "\n\n" }),
                window,
                cx,
            );
            window.focus(&input.focus_handle(cx), cx);
        });
        #[cfg(native_manager)]
        self.draft_changed(window, cx);
        cx.notify();
        Ok(())
    }
}

impl RecordsView {
    pub(crate) fn new(area: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search records"));
        let subscription = cx.observe(&query, |_, _, cx| cx.notify());
        Self {
            area,
            kind: None,
            visible: false,
            refresh: Refresh::default(),
            snapshot: Snapshot::default(),
            selected: None,
            query,
            _query: subscription,
            _publication: None,
            content: cx.new(|cx| TextViewState::markdown("", cx)),
            list_scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            notice: None,
        }
    }
    pub(crate) fn select(
        &mut self,
        kind: Option<Kind>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.kind == kind {
            return;
        }
        self.refresh.deactivate();
        self.kind = kind;
        self.snapshot = Snapshot::default();
        self.selected = None;
        self.notice = None;
        self.query
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.list_scroll.set_offset(point(px(0.), px(0.)));
        self.detail_scroll.set_offset(point(px(0.), px(0.)));
        self.visible = false;
        cx.notify();
    }
    pub(crate) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        let visible = visible && self.kind.is_some();
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.refresh.bind(self.area.clone(), self.area.clone());
            self.start(cx);
        } else {
            self.refresh.deactivate();
        }
        cx.notify();
    }
    pub(crate) fn changed(&mut self, cx: &mut Context<Self>) {
        if self.visible {
            self.refresh.request();
            self.start(cx);
        }
    }
    fn start(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = self.kind else {
            return;
        };
        let Some(ticket) = self.refresh.begin() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let area = ticket.project.clone();
            let snapshot = cx
                .background_executor()
                .spawn(async move { okilum_t3::records::read(&area, kind) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.refresh.finish(&ticket) {
                    this.install(snapshot, cx);
                }
                this.start(cx);
                cx.notify();
            });
        })
        .detach();
    }
    fn install(&mut self, mut snapshot: Snapshot, cx: &mut Context<Self>) {
        if !snapshot.problems.is_empty() {
            for previous in &self.snapshot.records {
                if !snapshot.records.iter().any(|r| r.path == previous.path) {
                    snapshot.records.push(previous.clone());
                    snapshot.problems.push(format!(
                        "{}: previous snapshot retained; current source unavailable",
                        previous.path
                    ));
                }
            }
        }
        self.snapshot = snapshot;
        if !self
            .snapshot
            .records
            .iter()
            .any(|r| Some(&r.path) == self.selected.as_ref())
        {
            self.selected = self.snapshot.records.first().map(|r| r.path.clone());
        }
        self.update_content(cx);
    }
    fn current(&self) -> Option<&Record> {
        self.snapshot
            .records
            .iter()
            .find(|r| Some(&r.path) == self.selected.as_ref())
    }
    fn update_content(&self, cx: &mut Context<Self>) {
        let body = self.current().map(|r| r.body.clone()).unwrap_or_default();
        self.content
            .update(cx, |content, cx| content.set_text(&body, cx));
    }
}

impl Render for RecordsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(kind) = self.kind else {
            return div().into_any_element();
        };
        let colors = brand::palette(cx);
        let query = self.query.read(cx).value().to_lowercase();
        let mut list = v_flex()
            .id("record-list")
            .debug_selector(|| "project-record-list".into())
            .w(px(240.))
            .flex_shrink_0()
            .min_h_0()
            .gap_2()
            .p_3()
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll);
        let mut count = 0;
        for record in &self.snapshot.records {
            if !query.is_empty()
                && !format!("{} {} {}", record.title, record.path, record.body)
                    .to_lowercase()
                    .contains(&query)
            {
                continue;
            }
            count += 1;
            let path = record.path.clone();
            let selected = self.selected.as_ref() == Some(&path);
            list = list.child(
                Button::new(SharedString::from(format!("record-{}", record.path)))
                    .debug_selector(move || format!("project-record-{count}"))
                    .ghost()
                    .selected(selected)
                    .w_full()
                    .h_auto()
                    .py_2()
                    .child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .w_full()
                                    .whitespace_normal()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(record.title.clone()),
                            )
                            .child(
                                div().text_xs().text_color(colors.text_muted).child(
                                    record
                                        .date
                                        .clone()
                                        .unwrap_or_else(|| "Date not specified".into()),
                                ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .whitespace_normal()
                                    .text_xs()
                                    .text_color(colors.text_muted)
                                    .child(record.path.clone()),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(path.clone());
                        this.notice = None;
                        this.detail_scroll.set_offset(point(px(0.), px(0.)));
                        this.update_content(cx);
                        cx.notify();
                    })),
            );
        }
        if count == 0 {
            list = list.child(if query.is_empty() {
                if self.snapshot.problems.is_empty() {
                    "No records yet."
                } else {
                    "Records unavailable."
                }
            } else {
                "No matching records."
            });
        }
        let mut detail = v_flex().flex_1().min_w_0().min_h_0().gap_3().p_4();
        if let Some(record) = self.current().cloned() {
            let open = record.path.clone();
            let context = record.draft(kind);
            detail = detail
                .child(h_flex().gap_2().flex_wrap().text_sm().text_color(colors.text_muted)
                    .child(record.date.clone().unwrap_or_else(|| "Date not specified".into()))
                    .when_some(record.status.clone(), |row, status| row.child(status)))
                .when(kind == Kind::Handovers, |view| view.child(div().text_sm().text_color(colors.text_muted)
                    .child("Historical handover. Check its date and compare with current decisions before continuing.")))
                .when(kind == Kind::Reports, |view| view.child(div().text_sm().text_color(colors.text_muted)
                    .child("Saved report. Build numbers and statuses describe the recorded date, not your installed app.")))
                .child(div().id("record-detail").debug_selector(|| "project-record-detail".into())
                    .flex_1().min_h_0().w_full().overflow_y_scroll().track_scroll(&self.detail_scroll)
                    .child(TextView::new(&self.content).selectable(true).style(reader_text_style(cx.theme()))
                        .on_link_click(|url, _, _, cx| {
                            // Source-relative links remain available through the ordinary Notes reader.
                            if let Some(url) = reader_manager_links::external_url(url) { cx.open_url(url.as_str()); }
                        })))
                .child(h_flex().gap_2().flex_wrap()
                    .child(Button::new("record-discuss").debug_selector(|| "project-record-discuss".into()).primary()
                        .label("Обсудить в Manager").on_click(cx.listener(move |_, _, _, cx| cx.emit(board::Action::RecordDiscuss(context.clone())))))
                    .child(Button::new("record-open").debug_selector(|| "project-record-open".into()).ghost()
                        .label("Open in Notes").on_click(cx.listener(move |_, _, _, cx| cx.emit(board::Action::Note(open.clone()))))))
                .child(div().w_full().whitespace_normal().text_xs().text_color(colors.text_muted).child(record.path));
        } else {
            detail = detail.child(div().text_lg().child(if self.refresh.is_pending() { "Loading records…" } else if !self.snapshot.problems.is_empty() { "Records unavailable" } else { "No project records" }))
                .child(div().whitespace_normal().child(format!("Read-only Markdown from {}. Create or edit records in Files; refresh to read them here.", kind.directories().join(", "))));
        }
        let mut root = v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(colors.canvas)
            .text_color(colors.text)
            .debug_selector(|| "project-records".into())
            .child(
                h_flex()
                    .p_3()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(kind.label()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors.text_muted)
                            .child("Records · Read only"),
                    )
                    .child(div().flex_1())
                    .child(div().w(px(200.)).child(Input::new(&self.query)))
                    .child(
                        Button::new("records-refresh")
                            .ghost()
                            .label("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.changed(cx))),
                    ),
            )
            .when(
                self.refresh.is_pending() && !self.snapshot.records.is_empty(),
                |view| {
                    view.child(
                        div()
                            .px_3()
                            .text_sm()
                            .child("Refreshing; showing the previous snapshot."),
                    )
                },
            );
        let mut problems = v_flex()
            .id("record-source-problems")
            .max_h(px(120.))
            .overflow_y_scroll();
        for problem in &self.snapshot.problems {
            problems = problems.child(
                div()
                    .px_3()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(problem.clone()),
            );
        }
        root = root.when(!self.snapshot.problems.is_empty(), |root| {
            root.child(problems)
        });
        root.when_some(self.notice.clone(), |view, notice| {
            view.child(div().px_3().text_sm().whitespace_normal().child(notice))
        })
        .child(
            h_flex()
                .items_stretch()
                .flex_1()
                .min_h_0()
                .child(list)
                .child(detail),
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests;
