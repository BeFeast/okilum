//! Express form. The owner handles Requested on its serial background worker
//! using okilum_t3::express_session::Request and the existing Manager journal.
use super::*;
use okilum_t3::express_session::{
    CloseReason, ClosureRecord, Draft, Platform, Recovered, RestartRequest, RestartState,
    RetainedCursor, RetainedPage, SavedChoice, SelectionRevision, SessionSelection, SessionTarget,
    SourceContext, Submission,
};

#[derive(Clone)]
pub(super) struct Requested(pub Draft);

#[derive(Clone)]
pub(super) struct RefreshRequested;

#[derive(Clone)]
pub(super) struct ContinueHistoryRequested;

pub(super) use okilum_t3::express_session::ClosureIntent as ClosureAction;

#[derive(Clone)]
pub(super) struct SessionsRequested {
    pub target: SessionTarget,
    pub cursor: Option<RetainedCursor>,
    pub generation: u64,
}

#[derive(Clone)]
pub(super) struct SessionChosen {
    pub selection: SessionSelection,
    pub generation: u64,
}

#[derive(Clone)]
pub(super) struct ClosureRequested {
    pub request_id: String,
    pub action: ClosureAction,
    pub selection: Option<SessionChosen>,
}

#[derive(Clone)]
pub(super) struct SelectionRestoreRequested(pub RestartRequest);

#[derive(Clone)]
pub(super) struct SelectionPersistenceRequested {
    pub chosen: SessionChosen,
    pub expected: SelectionRevision,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct SelectionForgetRequested {
    pub target: SessionTarget,
    pub expected: SelectionRevision,
    pub generation: u64,
}

pub(super) struct Form {
    goal: Entity<InputState>,
    recipient: Entity<InputState>,
    platforms: Vec<Platform>,
    source: Option<SourceContext>,
    locked: bool,
    refreshing: bool,
    notice: Option<String>,
    conversation_link: Option<String>,
    build_links: Vec<(String, String)>,
    executor_link: Option<String>,
    request_id: Option<String>,
    request_acknowledged: bool,
    partial: bool,
    follow_up_pr: Option<String>,
    closed: Option<CloseReason>,
    discard_confirm: bool,
    discard_reason: Entity<InputState>,
    session_page: Option<RetainedPage>,
    session_selection: Option<SessionSelection>,
    chooser_target: Option<SessionTarget>,
    chooser_generation: u64,
    catalog_loading: bool,
    parked_draft: Option<Draft>,
    restore_request: Option<RestartRequest>,
    selection_revision: Option<SelectionRevision>,
    selection_saving: bool,
    pending_forget: Option<SelectionForgetRequested>,
    _inputs: Vec<Subscription>,
}

impl EventEmitter<Requested> for Form {}
impl EventEmitter<RefreshRequested> for Form {}
impl EventEmitter<ContinueHistoryRequested> for Form {}
impl EventEmitter<ClosureRequested> for Form {}
impl EventEmitter<SessionsRequested> for Form {}
impl EventEmitter<SessionChosen> for Form {}
impl EventEmitter<SelectionRestoreRequested> for Form {}
impl EventEmitter<SelectionPersistenceRequested> for Form {}
impl EventEmitter<SelectionForgetRequested> for Form {}

impl Form {
    /// Source must come from explicit selection with original-source revision.
    /// Never pass read_note's rendered output or the entire vault.
    pub(super) fn new(
        source: Option<SourceContext>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let goal = cx.new(|cx| InputState::new(window, cx).placeholder("Issue or urgent goal"));
        let recipient = cx.new(|cx| InputState::new(window, cx).placeholder("Recipient"));
        let discard_reason =
            cx.new(|cx| InputState::new(window, cx).placeholder("Reason for explicit discard"));
        let inputs = [&goal, &recipient, &discard_reason]
            .into_iter()
            .map(|input| cx.subscribe(input, |_, _, _: &InputEvent, cx| cx.notify()))
            .collect();
        Self {
            goal,
            recipient,
            platforms: Vec::new(),
            source,
            locked: false,
            refreshing: false,
            notice: None,
            conversation_link: None,
            build_links: Vec::new(),
            executor_link: None,
            request_id: None,
            request_acknowledged: false,
            partial: false,
            follow_up_pr: None,
            closed: None,
            discard_confirm: false,
            discard_reason,
            session_page: None,
            session_selection: None,
            chooser_target: None,
            chooser_generation: 0,
            catalog_loading: false,
            parked_draft: None,
            restore_request: None,
            selection_revision: None,
            selection_saving: false,
            pending_forget: None,
            _inputs: inputs,
        }
    }

    /// Owner handles this local-only read on startup or explicit restore. It
    /// must supply the current approved target and retain its binding guard.
    pub(super) fn begin_restore_selection(
        &mut self,
        target: SessionTarget,
        cx: &mut Context<Self>,
    ) {
        if !self.locked && self.parked_draft.is_none() {
            self.parked_draft = Some(Draft {
                goal: self.goal.read(cx).value().to_string(),
                recipient: self.recipient.read(cx).value().to_string(),
                platforms: self.platforms.clone(),
                source: self.source.clone(),
            });
        }
        self.chooser_generation = self.chooser_generation.wrapping_add(1);
        let request = RestartRequest::new(target.clone(), self.chooser_generation);
        self.chooser_target = Some(target);
        self.session_selection = None;
        self.session_page = None;
        self.request_id = None;
        self.conversation_link = None;
        self.closed = None;
        self.catalog_loading = false;
        self.selection_saving = false;
        self.pending_forget = None;
        self.selection_revision = None;
        self.begin_reconnect(cx);
        self.notice = Some("Restoring saved explicit selection locally…".into());
        self.restore_request = Some(request.clone());
        cx.emit(SelectionRestoreRequested(request));
    }

    pub(super) fn selection_restored(
        &mut self,
        state: &RestartState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(request) = &self.restore_request else {
            return;
        };
        if !request.accepts(state)
            || request.generation != self.chooser_generation
            || self.chooser_target.as_ref() != Some(&state.saved.target)
        {
            return;
        }
        self.restore_request = None;
        self.selection_revision = Some(state.saved.revision.clone());
        self.refreshing = false;
        if let Some(held) = &state.held {
            self.failed(held.clone(), true, cx);
            return;
        }
        if let (Some(selection), Some(recovered)) = (&state.saved.selection, &state.recovered) {
            if selection.matches(recovered) {
                self.session_selection = Some(selection.clone());
                self.recovered(recovered, window, cx);
                // No SessionChosen event here: restart does not initiate history
                // reads, dispatch, receipt refresh, closure, or any timer.
                return;
            }
            self.failed(
                "Saved selection identity is held; choose explicitly again.".into(),
                true,
                cx,
            );
        } else if state.saved.selection.is_none() && state.recovered.is_none() {
            self.notice = Some("No saved explicit selection. Choose a retained session; none was selected automatically.".into());
            cx.notify();
        } else {
            self.failed(
                "Saved selection/source identity is incomplete; restart remains held.".into(),
                true,
                cx,
            );
        }
    }

    pub(super) fn selection_restore_failed(
        &mut self,
        request: &RestartRequest,
        detail: String,
        cx: &mut Context<Self>,
    ) {
        if self.restore_request.as_ref() == Some(request)
            && request.generation == self.chooser_generation
        {
            self.restore_request = None;
            self.failed(
                format!("Saved selection held: {detail}. No other session was restored."),
                true,
                cx,
            );
        }
    }

    pub(super) fn selection_persisted(
        &mut self,
        request: &SelectionPersistenceRequested,
        saved: &SavedChoice,
        cx: &mut Context<Self>,
    ) {
        if !self.selection_saving
            || self.chooser_generation != request.chosen.generation
            || self.session_selection.as_ref() != Some(&request.chosen.selection)
            || self.selection_revision.as_ref() != Some(&request.expected)
            || saved.target != *request.chosen.selection.target()
            || !saved.follows(&request.expected)
            || saved.selection.as_ref() != Some(&request.chosen.selection)
        {
            return;
        }
        self.selection_revision = Some(saved.revision.clone());
        self.selection_saving = false;
        // Continuation of this explicit chooser click, never a restart effect.
        cx.emit(request.chosen.clone());
        cx.notify();
    }

    pub(super) fn selection_persistence_failed(
        &mut self,
        request: &SelectionPersistenceRequested,
        detail: String,
        cx: &mut Context<Self>,
    ) {
        if self.selection_saving
            && self.chooser_generation == request.chosen.generation
            && self.session_selection.as_ref() == Some(&request.chosen.selection)
        {
            self.selection_saving = false;
            self.session_selection = None;
            self.selection_revision = None;
            self.failed(format!("Choice was not confirmed saved: {detail}. Restore saved state before choosing again."), true, cx);
        }
    }

    pub(super) fn selection_forgotten(
        &mut self,
        request: &SelectionForgetRequested,
        saved: &SavedChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_forget.as_ref() != Some(request)
            || request.generation != self.chooser_generation
            || saved.target != request.target
            || saved.selection.is_some()
            || !saved.follows(&request.expected)
        {
            return;
        }
        self.pending_forget = None;
        self.selection_revision = Some(saved.revision.clone());
        self.session_selection = None;
        self.resume_draft_unchecked(window, cx);
    }

    pub(super) fn selection_forget_failed(
        &mut self,
        request: &SelectionForgetRequested,
        detail: String,
        cx: &mut Context<Self>,
    ) {
        if self.pending_forget.as_ref() == Some(request)
            && request.generation == self.chooser_generation
        {
            self.pending_forget = None;
            self.selection_revision = None;
            self.failed(format!("Leaving saved selection is unconfirmed: {detail}. Restore saved state; journal and draft are kept."), true, cx);
        }
    }

    /// Parent loads the approved exact target on its serial worker first.
    /// Listing/selecting never overwrites this form's independent draft.
    pub(super) fn begin_session_chooser(&mut self, target: SessionTarget, cx: &mut Context<Self>) {
        if self.chooser_target.as_ref() != Some(&target) {
            self.selection_revision = None;
        }
        self.chooser_target = Some(target);
        self.request_session_page(None, cx);
    }

    fn request_session_page(&mut self, cursor: Option<RetainedCursor>, cx: &mut Context<Self>) {
        let Some(target) = self.chooser_target.clone() else {
            return;
        };
        if !self.locked && self.parked_draft.is_none() {
            self.parked_draft = Some(Draft {
                goal: self.goal.read(cx).value().to_string(),
                recipient: self.recipient.read(cx).value().to_string(),
                platforms: self.platforms.clone(),
                source: self.source.clone(),
            });
        }
        self.chooser_generation = self.chooser_generation.wrapping_add(1);
        self.restore_request = None;
        self.selection_saving = false;
        self.pending_forget = None;
        self.session_selection = None;
        self.session_page = None;
        self.request_id = None;
        self.request_acknowledged = false;
        self.conversation_link = None;
        self.closed = None;
        self.discard_confirm = false;
        self.begin_reconnect(cx);
        self.catalog_loading = true;
        cx.emit(SessionsRequested {
            target,
            cursor,
            generation: self.chooser_generation,
        });
    }

    pub(super) fn present_sessions(
        &mut self,
        request: &SessionsRequested,
        page: RetainedPage,
        cx: &mut Context<Self>,
    ) {
        if !self.catalog_loading
            || self.chooser_generation != request.generation
            || self.chooser_target.as_ref() != Some(&request.target)
            || page.target != request.target
        {
            return;
        }
        self.catalog_loading = false;
        self.refreshing = false;
        self.notice = Some(if page.total == 0 {
            "No retained Express sessions. Start a new request explicitly.".into()
        } else {
            "Choose the exact retained request to reconcile. No session has been selected.".into()
        });
        self.session_page = Some(page);
        cx.notify();
    }

    pub(super) fn sessions_failed(
        &mut self,
        request: &SessionsRequested,
        detail: String,
        cx: &mut Context<Self>,
    ) {
        if self.catalog_loading
            && self.chooser_generation == request.generation
            && self.chooser_target.as_ref() == Some(&request.target)
        {
            self.catalog_loading = false;
            self.failed(format!("Chooser held: {detail}"), true, cx);
        }
    }

    fn choose_session(&mut self, selection: SessionSelection, cx: &mut Context<Self>) {
        if self.refreshing || self.chooser_target.as_ref() != Some(selection.target()) {
            return;
        }
        let Some(expected) = self.selection_revision.clone() else {
            self.notice = Some(
                "Restore saved-selection state before choosing; no unsaved choice was applied."
                    .into(),
            );
            cx.notify();
            return;
        };
        self.chooser_generation = self.chooser_generation.wrapping_add(1);
        self.session_selection = Some(selection.clone());
        self.session_page = None;
        self.request_id = None;
        self.request_acknowledged = false;
        self.conversation_link = None;
        self.closed = None;
        self.discard_confirm = false;
        self.begin_reconnect(cx);
        self.catalog_loading = false;
        self.selection_saving = true;
        cx.emit(SelectionPersistenceRequested {
            chosen: SessionChosen {
                selection,
                generation: self.chooser_generation,
            },
            expected,
        });
    }

    pub(super) fn selected_session(&self) -> Option<SessionChosen> {
        if self.selection_saving || self.restore_request.is_some() {
            return None;
        }
        self.session_selection
            .clone()
            .map(|selection| SessionChosen {
                selection,
                generation: self.chooser_generation,
            })
    }

    pub(super) fn recovered_selected(
        &mut self,
        chosen: &SessionChosen,
        recovered: &Recovered,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.chooser_generation == chosen.generation
            && self.session_selection.as_ref() == Some(&chosen.selection)
            && chosen.selection.matches(recovered)
        {
            self.recovered(recovered, window, cx);
        }
    }

    pub(super) fn selection_failed(
        &mut self,
        chosen: &SessionChosen,
        detail: String,
        cx: &mut Context<Self>,
    ) {
        if self.chooser_generation == chosen.generation
            && self.session_selection.as_ref() == Some(&chosen.selection)
        {
            self.failed(format!("Selected session held: {detail}"), true, cx);
        }
    }

    fn resume_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.chooser_target.clone() {
            let Some(expected) = self.selection_revision.clone() else {
                self.failed("Restore saved-selection state before leaving it; no durable choice was forgotten.".into(), true, cx);
                return;
            };
            self.chooser_generation = self.chooser_generation.wrapping_add(1);
            let request = SelectionForgetRequested {
                target,
                expected,
                generation: self.chooser_generation,
            };
            self.pending_forget = Some(request.clone());
            self.refreshing = true;
            cx.emit(request);
            cx.notify();
        } else {
            self.resume_draft_unchecked(window, cx);
        }
    }

    fn resume_draft_unchecked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parked = self.parked_draft.take();
        self.no_retained_request(cx);
        if let Some(draft) = parked {
            self.goal
                .update(cx, |input, cx| input.set_value(draft.goal, window, cx));
            self.recipient
                .update(cx, |input, cx| input.set_value(draft.recipient, window, cx));
            self.platforms = draft.platforms;
            self.source = draft.source;
            cx.notify();
        }
    }

    pub(super) fn closure_applied_selected(
        &mut self,
        chosen: &SessionChosen,
        record: &ClosureRecord,
        cx: &mut Context<Self>,
    ) {
        if self.chooser_generation == chosen.generation
            && self.session_selection.as_ref() == Some(&chosen.selection)
            && chosen.selection.matches_closure(record)
        {
            self.closure_applied(record, cx);
        }
    }

    /// Call only for this form's still-current project/connection generation.
    pub(super) fn accepted(&mut self, submission: &Submission, cx: &mut Context<Self>) {
        self.locked = true;
        self.refreshing = false;
        self.request_acknowledged = true;
        self.conversation_link = Some(submission.conversation_link.clone());
        self.notice = Some(
            "Request accepted by Manager. Executor, build and delivery are not yet confirmed."
                .into(),
        );
        cx.notify();
    }

    /// A retained or uncertain intent stays locked; Retry belongs to Manager's
    /// existing durable operation. Unsupported gateway errors are shown verbatim.
    pub(super) fn failed(&mut self, detail: String, retained: bool, cx: &mut Context<Self>) {
        self.locked = retained;
        self.refreshing = false;
        self.build_links.clear();
        self.executor_link = None;
        self.follow_up_pr = None;
        self.partial = false;
        self.notice = Some(detail);
        cx.notify();
    }

    /// Lock before the serial worker starts so reconnect cannot race a new intent.
    pub(super) fn begin_reconnect(&mut self, cx: &mut Context<Self>) {
        self.locked = true;
        self.refreshing = true;
        self.build_links.clear();
        self.executor_link = None;
        self.follow_up_pr = None;
        self.partial = false;
        self.notice = Some("Checking retained express request and result…".into());
        cx.notify();
    }

    /// Only after reconnect returned Ok(None) for the still-current binding.
    pub(super) fn no_retained_request(&mut self, cx: &mut Context<Self>) {
        if self.selection_revision.is_some() && self.session_selection.is_some() {
            self.failed(
                "Leave the saved choice explicitly before starting a new draft.".into(),
                true,
                cx,
            );
            return;
        }
        self.locked = false;
        self.refreshing = false;
        self.build_links.clear();
        self.executor_link = None;
        self.conversation_link = None;
        self.request_id = None;
        self.request_acknowledged = false;
        self.partial = false;
        self.follow_up_pr = None;
        self.closed = None;
        self.discard_confirm = false;
        self.notice = None;
        self.session_page = None;
        self.session_selection = None;
        self.chooser_target = None;
        self.chooser_generation = self.chooser_generation.wrapping_add(1);
        self.catalog_loading = false;
        self.restore_request = None;
        self.selection_saving = false;
        self.pending_forget = None;
        self.selection_revision = None;
        cx.notify();
    }

    /// Apply only after the owner checks connection generation and binding.
    /// No network or resend occurs while restoring the independent form draft.
    pub(super) fn recovered(
        &mut self,
        recovered: &Recovered,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.chooser_target.is_some()
            && self
                .session_selection
                .as_ref()
                .is_none_or(|selection| !selection.matches(recovered))
        {
            return;
        }
        self.locked = true;
        self.refreshing = false;
        self.request_id = Some(recovered.request.request_id.clone());
        self.request_acknowledged = recovered.record.receipt.is_some();
        self.partial = recovered.continuation.is_some();
        self.follow_up_pr = None;
        self.closed = recovered
            .closure
            .as_ref()
            .map(|closure| closure.reason.clone());
        self.discard_confirm = false;
        let draft = &recovered.request.draft;
        self.goal.update(cx, |input, cx| {
            input.set_value(draft.goal.clone(), window, cx)
        });
        self.recipient.update(cx, |input, cx| {
            input.set_value(draft.recipient.clone(), window, cx)
        });
        self.platforms = draft.platforms.clone();
        self.source = draft.source.clone();
        self.conversation_link =
            (!recovered.conversation_link.is_empty()).then(|| recovered.conversation_link.clone());
        self.build_links.clear();
        self.executor_link = None;
        self.notice = Some(if let Some(error) = &recovered.result_error {
            format!("Result unconfirmed: {error}. Retained request was not resent.")
        } else if let Some(result) = &recovered.result {
            self.executor_link = Some(result.executor_link.clone());
            self.follow_up_pr = result.follow_up_pr.clone();
            self.build_links = result
                .receipt
                .builds
                .iter()
                .map(|build| {
                    (
                        format!(
                            "{} · {} · {}",
                            build.platform.label(),
                            build.build,
                            build.source
                        ),
                        build.artifact.clone(),
                    )
                })
                .collect();
            format!("Manager reported {} of {} platform builds for {} at {}. Availability, installation and delivery remain unverified. Receipt: {} · {}",
                self.build_links.len(), draft.platforms.len(), result.receipt.recipient,
                result.receipt.recorded_at, result.manager_item_id, result.manager_item_revision)
        } else if recovered.record.receipt.is_some() {
            "Request accepted by Manager; no build result is confirmed.".into()
        } else {
            "Delivery is unconfirmed. Use Manager Retry for the retained request; reconnect did not resend it.".into()
        });
        cx.notify();
    }

    /// Apply only to the same request after durable closure publication succeeds.
    pub(super) fn closure_applied(&mut self, record: &ClosureRecord, cx: &mut Context<Self>) {
        if self
            .session_selection
            .as_ref()
            .is_some_and(|selection| !selection.matches_closure(record))
        {
            return;
        }
        if self.request_id.as_deref() != Some(record.request_id.as_str()) {
            return;
        }
        self.closed = Some(record.reason.clone());
        self.refreshing = false;
        self.discard_confirm = false;
        self.locked = true;
        cx.notify();
    }

    fn close(&mut self, action: ClosureAction, cx: &mut Context<Self>) {
        if self.refreshing || self.closed.is_some() {
            return;
        }
        let Some(request_id) = self.request_id.clone() else {
            return;
        };
        if let ClosureAction::Discard(reason) = &action {
            if !self.discard_confirm
                || reason.trim().is_empty()
                || reason.len() > 1024
                || reason.chars().any(char::is_control)
            {
                self.notice = Some(
                    "Enter a discard reason of at most 1024 bytes without control characters."
                        .into(),
                );
                cx.notify();
                return;
            }
        } else if self.follow_up_pr.is_none() || self.partial {
            return;
        }
        self.refreshing = true;
        cx.emit(ClosureRequested {
            request_id,
            action,
            selection: self.selected_session(),
        });
        cx.notify();
    }

    fn request(&mut self, cx: &mut Context<Self>) {
        if self.locked {
            return;
        }
        let draft = Draft {
            goal: self.goal.read(cx).value().to_string(),
            recipient: self.recipient.read(cx).value().to_string(),
            platforms: self.platforms.clone(),
            source: self.source.clone(),
        };
        match draft.validate() {
            Ok(()) => {
                self.locked = true;
                self.notice = Some("Requesting express session…".into());
                cx.emit(Requested(draft));
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
        cx.notify();
    }
}

impl Render for Form {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut platforms = h_flex().gap_2().flex_wrap();
        for (id, platform, label) in [
            ("express-linux", Platform::Linux, "Linux"),
            ("express-mac", Platform::Mac, "Mac"),
            ("express-windows", Platform::Windows, "Windows"),
        ] {
            platforms = platforms.child(
                Button::new(id)
                    .ghost()
                    .label(label)
                    .selected(self.platforms.contains(&platform))
                    .disabled(self.locked)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.locked {
                            return;
                        }
                        if this.platforms.contains(&platform) {
                            this.platforms.retain(|p| *p != platform);
                        } else {
                            this.platforms.push(platform);
                        }
                        cx.notify();
                    })),
            );
        }
        let mut panel = v_flex()
            .id("express-session-form")
            .gap_2()
            .child("Express session");
        if let Some(target) = &self.chooser_target {
            let restore_target = target.clone();
            panel = panel.child(
                Button::new("express-restore-saved-choice")
                    .ghost()
                    .label("Restore saved selection")
                    .disabled(self.refreshing)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.begin_restore_selection(restore_target.clone(), cx)
                    })),
            );
            panel = panel
                .child(format!(
                    "Target: {} · environment {} · Manager {}/{} · executor project {}",
                    target.origin,
                    target.environment_id,
                    target.manager_project_id,
                    target.manager_thread_id,
                    target.executor_project_id
                ))
                .child(
                    Button::new("express-refresh-chooser")
                        .ghost()
                        .label("Choose retained session…")
                        .disabled(self.refreshing)
                        .on_click(
                            cx.listener(|this, _, _, cx| this.request_session_page(None, cx)),
                        ),
                );
        }
        if let Some(page) = &self.session_page {
            let mut rows = v_flex()
                .id("express-retained-sessions")
                .max_h(px(320.))
                .overflow_y_scroll()
                .gap_2();
            for (index, row) in page.rows.iter().enumerate() {
                let selection = row.selection.clone();
                let state = if let Some(reason) = &row.held {
                    format!("Held: {reason}")
                } else if row.closure.is_some() {
                    "Explicitly closed; delivery/result are separate".into()
                } else if row.acknowledged {
                    "Manager accepted; build result unconfirmed".into()
                } else {
                    "Delivery unconfirmed; not resent".into()
                };
                rows = rows.child(format!("{} · {} · {:?} · {}", row.goal, row.recipient, row.platforms, state))
                    .child(format!("Request {} · operation {} · recorded {} · environment {} · Manager {}/{} · operation thread {}",
                        row.request_id.as_deref().unwrap_or("unknown"), row.operation_id, row.created_at_millis,
                        row.environment_id.as_deref().unwrap_or("unknown"), row.manager_project_id.as_deref().unwrap_or("unknown"),
                        row.manager_thread_id, row.operation_thread_id))
                    .child(Button::new(("express-select-session", index)).ghost().label("Reconcile this exact request")
                        .disabled(self.refreshing || selection.is_none()).on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(selection) = &selection { this.choose_session(selection.clone(), cx); }
                        })));
            }
            panel = panel
                .child(format!(
                    "{} retained sessions · {} on this page",
                    page.total,
                    page.rows.len()
                ))
                .child(rows);
            if let Some(cursor) = &page.next {
                let cursor = cursor.clone();
                panel = panel.child(
                    Button::new("express-next-session-page")
                        .ghost()
                        .label("Next retained sessions")
                        .disabled(self.refreshing)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.request_session_page(Some(cursor.clone()), cx)
                        })),
                );
            }
            if page.total == 0 {
                panel = panel.child(
                    Button::new("express-new-empty-catalog")
                        .label("New express request")
                        .on_click(cx.listener(|this, _, window, cx| this.resume_draft(window, cx))),
                );
            }
        }
        panel = panel
            .child(Input::new(&self.goal).disabled(self.locked))
            .child(platforms)
            .child(Input::new(&self.recipient).disabled(self.locked));
        if let Some(source) = &self.source {
            panel = panel.child(format!(
                "Selected source: {} · {} bytes · {}",
                source.path,
                source.text.len(),
                source.revision
            ));
            panel = panel.child(
                Button::new("express-remove-source")
                    .ghost()
                    .label("Remove source context")
                    .disabled(self.locked)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if !this.locked {
                            this.source = None;
                            cx.notify();
                        }
                    })),
            );
        } else {
            panel = panel.child("No source context selected");
        }
        panel = panel
            .child(
                "Manager request only. Build, install and delivery require separate confirmation.",
            )
            .child(
                Button::new("express-request")
                    .label("Request express session")
                    .disabled(self.locked)
                    .on_click(cx.listener(|this, _, _, cx| this.request(cx))),
            );
        if let Some(notice) = &self.notice {
            panel = panel.child(notice.clone());
        }
        if let Some(link) = &self.conversation_link {
            let link = link.clone();
            panel = panel.child(
                Button::new("express-open-conversation")
                    .ghost()
                    .label("Open request in T3")
                    .on_click(move |_, _, cx| cx.open_url(&link)),
            );
        }
        if let Some(link) = &self.executor_link {
            let link = link.clone();
            panel = panel.child(
                Button::new("express-open-executor")
                    .ghost()
                    .label("Open result executor in T3")
                    .on_click(move |_, _, cx| cx.open_url(&link)),
            );
        }
        for (index, (label, link)) in self.build_links.iter().enumerate() {
            let link = link.clone();
            panel = panel.child(
                Button::new(("express-build-result", index))
                    .ghost()
                    .label(label.clone())
                    .on_click(move |_, _, cx| cx.open_url(&link)),
            );
        }
        if let Some(url) = &self.follow_up_pr {
            let url = url.clone();
            panel = panel.child(
                Button::new("express-follow-up-pr")
                    .ghost()
                    .label("Open executor follow-up PR")
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        if let Some(reason) = &self.closed {
            panel = panel.child(match reason {
                CloseReason::Discard { reason } => format!("Closed by explicit discard: {reason}. This does not stop or retract remote work."),
                CloseReason::FollowUpPr { url, .. } => format!("Closed with recorded follow-up PR: {url}. Merge, build and delivery remain separate."),
            });
            if self.request_acknowledged {
                panel = panel.child(
                    Button::new("express-new-after-closure")
                        .ghost()
                        .label("New express request")
                        .disabled(self.refreshing)
                        .on_click(cx.listener(|this, _, window, cx| {
                            if !this.refreshing {
                                this.resume_draft(window, cx);
                            }
                        })),
                );
            } else {
                panel = panel.child("Held: Manager delivery is unconfirmed. Closing local tracking did not retract or resend the request.");
            }
        } else if self.request_id.is_some() {
            panel = panel.child(
                Button::new("express-close-follow-up")
                    .ghost()
                    .label("Close with follow-up PR")
                    .disabled(self.refreshing || self.partial || self.follow_up_pr.is_none())
                    .on_click(
                        cx.listener(|this, _, _, cx| this.close(ClosureAction::FollowUpPr, cx)),
                    ),
            );
            if self.discard_confirm {
                panel = panel.child("Discard local tracking? This keeps the request and draft, and does not stop remote work.")
                    .child(Input::new(&self.discard_reason).disabled(self.refreshing))
                    .child(Button::new("express-confirm-discard").label("Confirm explicit discard").disabled(self.refreshing)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let reason = this.discard_reason.read(cx).value().to_string();
                            this.close(ClosureAction::Discard(reason), cx);
                        })))
                    .child(Button::new("express-cancel-discard").ghost().label("Keep session open").disabled(self.refreshing)
                        .on_click(cx.listener(|this, _, _, cx| { this.discard_confirm = false; cx.notify(); })));
            } else {
                panel = panel.child(
                    Button::new("express-discard")
                        .ghost()
                        .label("Discard…")
                        .disabled(self.refreshing)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.discard_confirm = true;
                            cx.notify();
                        })),
                );
            }
        }
        if self.partial && !self.refreshing {
            panel = panel.child(
                Button::new("express-continue-history")
                    .label("Read next history page")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.refreshing || !this.partial {
                            return;
                        }
                        this.refreshing = true;
                        cx.emit(ContinueHistoryRequested);
                        cx.notify();
                    })),
            );
        }
        if self.locked && (self.chooser_target.is_none() || self.session_selection.is_some()) {
            panel = panel.child(
                Button::new("express-refresh-result")
                    .ghost()
                    .label("Refresh result")
                    .disabled(self.refreshing)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.refreshing {
                            return;
                        }
                        this.begin_reconnect(cx);
                        cx.emit(RefreshRequested);
                    })),
            );
        }
        panel
    }
}
