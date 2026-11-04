//! Express form. The owner handles Requested on its serial background worker
//! using okilum_t3::express_session::Request and the existing Manager journal.
use super::*;
use okilum_t3::express_session::{
    CloseReason, ClosureRecord, Draft, Platform, Recovered, RestartRequest, RestartState,
    RetainedCursor, RetainedPage, SavedChoice, SelectionRevision, SessionSelection, SessionTarget,
    SourceContext, Submission,
};

#[derive(Clone)]
pub(super) struct Requested(pub Draft, pub String);

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
    request_nonce: Option<String>,
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
            request_nonce: None,
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
        self.request_nonce = None;
        self.chooser_generation = self
            .chooser_generation
            .checked_add(1)
            .expect("Express chooser generation exhausted");
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
    #[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
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
        self.request_nonce = None;
        self.chooser_generation = self
            .chooser_generation
            .checked_add(1)
            .expect("Express chooser generation exhausted");
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
        self.request_nonce = None;
        self.chooser_generation = self
            .chooser_generation
            .checked_add(1)
            .expect("Express chooser generation exhausted");
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
            self.request_nonce = None;
            self.chooser_generation = self
                .chooser_generation
                .checked_add(1)
                .expect("Express chooser generation exhausted");
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
        self.request_nonce = None;
        self.chooser_generation = self
            .chooser_generation
            .checked_add(1)
            .expect("Express chooser generation exhausted");
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
                let nonce = uuid::Uuid::new_v4().to_string();
                self.request_nonce = Some(nonce.clone());
                cx.emit(Requested(draft, nonce));
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

// Parent integration stays here so the shared mount contains no second
// controller, transport, lifecycle policy, or background polling loop.
use okilum_t3::express_session::{self as model, Request, SelectionStore};
use okilum_t3::stream::NativeClient;
use std::sync::atomic::{AtomicBool, Ordering};

type ExpressResult<T> = std::result::Result<T, String>;

struct RetainedForm {
    target: SessionTarget,
    form: Entity<Form>,
    recovered: Option<Recovered>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Default)]
pub(super) struct State {
    forms: Vec<RetainedForm>,
    active: Option<usize>,
    epoch: u64,
    // A menu can outlive a closed form. Remember that a ticket was captured so
    // hide/rebind also invalidates it, without churning on repeated empty paints.
    lifecycle_captured: AtomicBool,
    authority: Option<Arc<AtomicBool>>,
    // One outstanding background operation, including after revocation. A new
    // menu open cannot queue a second job behind an obsolete one.
    pending: Option<String>,
}
impl State {
    pub(super) fn revoke(&mut self) {
        if let Some(authority) = self.authority.take() {
            authority.store(false, Ordering::Release);
        }
        let was_open = self.active.take().is_some();
        let had_ticket = self.lifecycle_captured.swap(false, Ordering::AcqRel);
        if !was_open && !had_ticket {
            return;
        }
        self.epoch = self
            .epoch
            .checked_add(1)
            .expect("Express lifecycle exhausted");
    }
    pub(super) fn is_open(&self) -> bool {
        self.active.is_some() && self.authority.is_some()
    }
}

#[derive(Clone)]
struct OwnerTicket {
    target: SessionTarget,
    generation: u64,
    connection: u64,
    epoch: u64,
    scope: composer::TypedScope,
    client: Arc<NativeClient>,
    state: PathBuf,
    area: PathBuf,
    binding: AreaBinding,
}
impl OwnerTicket {
    fn capture(view: &ProjectView, cx: &App) -> ExpressResult<Self> {
        if !view.active
            || view.section != ProjectSection::Manager
            || view.executors_open
            || !view.draft_handoff_surface_is_visible()
            || !view.draft_handoff_owner_ready(cx)
            || view.native.deleted()
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
        {
            return Err("Open the current ready Manager before opening Express".into());
        }
        let client = view
            .native
            .model_client()
            .ok_or("Manager client is unavailable")?;
        if !client.access_allowed()
            || view
                .threads
                .handoff_revoked
                .as_ref()
                .is_some_and(|old| Arc::ptr_eq(old, &client))
        {
            return Err("Manager access was revoked".into());
        }
        let environment = view.environment.as_ref().ok_or("No Manager environment")?;
        if client.origin() != environment.origin() {
            return Err("Manager client belongs to another origin".into());
        }
        if reader_t3_settings::signed_out(&environment.origin(), cx) {
            return Err("Manager is signed out".into());
        }
        let binding = view.binding.clone().ok_or("No Manager binding")?;
        let target = SessionTarget {
            origin: environment.origin(),
            environment_id: view
                .identity
                .as_ref()
                .ok_or("Manager server identity is not verified; open Diagnostics to reconnect it")?
                .environment_id
                .clone(),
            manager_project_id: binding
                .manager
                .project_id
                .clone()
                .ok_or("No Manager project")?,
            manager_thread_id: binding
                .manager
                .thread_id
                .clone()
                .ok_or("No Manager thread")?,
            executor_project_id: binding.executor.project_id.clone(),
        };
        let scope = view
            .typed_scope()
            .ok_or("Manager draft owner is not ready")?;
        if view.chat.as_ref().map(|chat| chat.thread_id.as_str())
            != Some(target.manager_thread_id.as_str())
            || scope.environment != target.origin
            || scope.project != target.manager_project_id
            || scope.thread != target.manager_thread_id
        {
            return Err("Manager identity differs from its binding".into());
        }
        let state = view
            .state
            .clone()
            .ok_or("Durable application state is unavailable")?;
        SelectionStore::new(&state, &target).map_err(|e| e.to_string())?;
        view.express
            .lifecycle_captured
            .store(true, Ordering::Release);
        Ok(Self {
            target,
            generation: view.generation,
            connection: view.native.connection_epoch,
            epoch: view.express.epoch,
            scope: scope.clone(),
            client,
            state,
            area: view.area.clone().ok_or("Project source is unavailable")?,
            binding,
        })
    }
    fn current(&self, view: &ProjectView, cx: &App) -> bool {
        Self::capture(view, cx).is_ok_and(|now| {
            now.target == self.target
                && now.generation == self.generation
                && now.connection == self.connection
                && now.epoch == self.epoch
                && now.scope == self.scope
                && now.state == self.state
                && now.area == self.area
                && now.binding == self.binding
                && Arc::ptr_eq(&now.client, &self.client)
        })
    }
    fn open(&self, view: &mut ProjectView, window: &mut Window, cx: &mut Context<ProjectView>) {
        if self.current(view, cx) {
            view.open_express_session(window, cx);
        } else {
            view.error = Some(
                "Manager changed or access was revoked. Reopen its menu before opening Express."
                    .into(),
            );
            cx.notify();
        }
    }
    fn local_current(&self, authority: &AtomicBool) -> anyhow::Result<()> {
        anyhow::ensure!(
            authority.load(Ordering::Acquire) && self.client.access_allowed(),
            "Express owner was revoked; no action was applied"
        );
        let current = okilum_t3::projects::Project::read(&self.area)?.native_binding()?;
        anyhow::ensure!(
            current.as_ref() == Some(&self.binding),
            "Project binding changed; Express remains held"
        );
        Ok(())
    }
}

pub(super) fn menu_item(
    owner: WeakEntity<ProjectView>,
    cx: &mut App,
) -> gpui_component::menu::PopupMenuItem {
    let ticket = owner
        .upgrade()
        .ok_or("Project view closed".into())
        .and_then(|view| OwnerTicket::capture(view.read(cx), cx));
    gpui_component::menu::PopupMenuItem::new("Express session…").on_click(move |_, window, cx| {
        let _ = owner.update(cx, |view, cx| {
            match &ticket {
                Ok(ticket) => ticket.open(view, window, cx),
                _ => {
                    view.error = Some("Manager changed or access was revoked. Reopen its menu before opening Express.".into());
                    cx.notify();
                }
            }
        });
    })
}

struct Work {
    owner: OwnerTicket,
    authority: Arc<AtomicBool>,
    environment: Environment,
    journal: MessageJournal,
    // Captured explicitly. Native-only connections refuse these gateway reads;
    // there is no bridge, newly created connection, or hidden fallback here.
    gateway: Option<Arc<dyn ManagerGateway>>,
}
impl Work {
    fn current(&self) -> anyhow::Result<()> {
        self.owner.local_current(&self.authority)
    }
    fn store(&self) -> anyhow::Result<SelectionStore> {
        self.current()?;
        SelectionStore::new(&self.owner.state, &self.owner.target)
    }
    fn gateway(&self) -> anyhow::Result<&dyn ManagerGateway> {
        self.current()?;
        self.gateway.as_deref().ok_or_else(|| anyhow::anyhow!("This Manager has no supported paired gateway for Express; nothing was connected or sent"))
    }
}

impl ProjectView {
    fn express_form_current(&self, form: &Entity<Form>) -> bool {
        self.express
            .active
            .and_then(|i| self.express.forms.get(i))
            .is_some_and(|session| session.form == *form)
    }

    fn open_express_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = match OwnerTicket::capture(self, cx) {
            Ok(ticket) => ticket.target,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        if self.express.pending.is_some() {
            self.error = Some("The previous Express operation is still completing; no second operation was queued.".into());
            cx.notify();
            return;
        }
        self.express.revoke();
        let index = self
            .express
            .forms
            .iter()
            .position(|session| session.target == target)
            .unwrap_or_else(|| {
                let form = cx.new(|cx| Form::new(None, window, cx));
                let subscriptions = vec![
                    cx.subscribe_in(
                        &form,
                        window,
                        |view, form, event: &SelectionRestoreRequested, window, cx| {
                            view.express_restore(form.clone(), event.clone(), window, cx)
                        },
                    ),
                    cx.subscribe_in(
                        &form,
                        window,
                        |view, form, event: &SelectionPersistenceRequested, window, cx| {
                            view.express_remember(form.clone(), event.clone(), window, cx)
                        },
                    ),
                    cx.subscribe_in(
                        &form,
                        window,
                        |view, form, event: &SelectionForgetRequested, window, cx| {
                            view.express_forget(form.clone(), event.clone(), window, cx)
                        },
                    ),
                    cx.subscribe_in(
                        &form,
                        window,
                        |view, form, event: &SessionsRequested, window, cx| {
                            view.express_sessions(form.clone(), event.clone(), window, cx)
                        },
                    ),
                    cx.subscribe_in(
                        &form,
                        window,
                        |view, form, event: &SessionChosen, window, cx| {
                            view.express_selected(form.clone(), event.clone(), false, window, cx)
                        },
                    ),
                    cx.subscribe_in(
                        &form,
                        window,
                        |view, form, _: &RefreshRequested, window, cx| {
                            if let Some(chosen) = form.read(cx).selected_session() {
                                view.express_selected(form.clone(), chosen, false, window, cx);
                            } else {
                                view.express_hold(
                                    form,
                                    "Choose an exact retained session before refreshing",
                                    cx,
                                );
                            }
                        },
                    ),
                    cx.subscribe_in(
                        &form,
                        window,
                        |view, form, _: &ContinueHistoryRequested, window, cx| {
                            if let Some(chosen) = form.read(cx).selected_session() {
                                view.express_selected(form.clone(), chosen, true, window, cx);
                            } else {
                                view.express_hold(
                                    form,
                                    "Choose an exact retained session before reading history",
                                    cx,
                                );
                            }
                        },
                    ),
                    cx.subscribe_in(
                        &form,
                        window,
                        |view, form, event: &ClosureRequested, window, cx| {
                            view.express_close_session(form.clone(), event.clone(), window, cx)
                        },
                    ),
                    cx.subscribe_in(
                        &form,
                        window,
                        |view, form, event: &Requested, window, cx| {
                            view.express_request(form.clone(), event.clone(), window, cx)
                        },
                    ),
                ];
                self.express.forms.push(RetainedForm {
                    target: target.clone(),
                    form,
                    recovered: None,
                    _subscriptions: subscriptions,
                });
                self.express.forms.len() - 1
            });
        self.express.active = Some(index);
        self.express.authority = Some(Arc::new(AtomicBool::new(true)));
        let form = self.express.forms[index].form.clone();
        // Explicit menu open restores only local state. No chooser/history read,
        // acknowledgement lookup or resend is emitted by this restore.
        form.update(cx, |form, cx| form.begin_restore_selection(target, cx));
        cx.notify();
    }

    fn express_hold(&mut self, form: &Entity<Form>, detail: &str, cx: &mut Context<Self>) {
        if self.express_form_current(form) {
            form.update(cx, |form, cx| form.failed(detail.into(), true, cx));
            self.error = Some(detail.into());
            cx.notify();
        }
    }

    // One serial lane on the existing background executor, not another worker
    // or controller. Reject overlap rather than queuing stale user requests.
    fn express_work<T: Send + 'static>(
        &mut self,
        form: Entity<Form>,
        window: &mut Window,
        cx: &mut Context<Self>,
        run: impl FnOnce(&Work) -> anyhow::Result<T> + Send + 'static,
        apply: impl FnOnce(&mut Self, Entity<Form>, ExpressResult<T>, &mut Window, &mut Context<Self>)
            + 'static,
    ) {
        if !self.express_form_current(&form) {
            return;
        }
        if self.express.pending.is_some() {
            self.express_hold(
                &form,
                "An Express operation is already pending; no second operation was queued",
                cx,
            );
            return;
        }
        let owner = match OwnerTicket::capture(self, cx) {
            Ok(owner) => owner,
            Err(error) => {
                self.express_hold(&form, &error, cx);
                return;
            }
        };
        let Some(authority) = self.express.authority.clone() else {
            return;
        };
        let Some(environment) = self.environment.clone() else {
            return;
        };
        let Some(journal) = self.journal() else {
            return;
        };
        let gateway = self.gateway.clone();
        let work = Work {
            owner: owner.clone(),
            authority: authority.clone(),
            environment,
            journal,
            gateway: gateway.clone(),
        };
        let nonce = uuid::Uuid::new_v4().to_string();
        self.express.pending = Some(nonce.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    work.current()?;
                    let result = run(&work)?;
                    work.current()?;
                    Ok::<_, anyhow::Error>(result)
                })
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = this.update_in(cx, |view, window, cx| {
                if view.express.pending.as_ref() != Some(&nonce) {
                    return;
                }
                view.express.pending = None;
                cx.notify();
                let same_gateway = match (&gateway, &view.gateway) {
                    (Some(expected), Some(current)) => Arc::ptr_eq(expected, current),
                    (None, None) => true,
                    _ => false,
                };
                if !authority.load(Ordering::Acquire)
                    || !owner.current(view, cx)
                    || !same_gateway
                    || !view.express_form_current(&form)
                {
                    return;
                }
                apply(view, form, result, window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn express_restore(
        &mut self,
        form: Entity<Form>,
        event: SelectionRestoreRequested,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if form.read(cx).restore_request.as_ref() != Some(&event.0) {
            return;
        }
        let request = event.0.clone();
        self.express_work(
            form,
            window,
            cx,
            move |work| {
                work.store()?
                    .restore(&work.journal, &work.environment, &request)
            },
            move |view, form, result, window, cx| match result {
                Ok(restored) => {
                    if let Some(index) = view.express.active {
                        view.express.forms[index].recovered = restored.recovered.clone();
                    }
                    form.update(cx, |form, cx| {
                        form.selection_restored(&restored, window, cx)
                    });
                }
                Err(error) => form.update(cx, |form, cx| {
                    form.selection_restore_failed(&event.0, error, cx)
                }),
            },
        );
    }

    fn express_remember(
        &mut self,
        form: Entity<Form>,
        event: SelectionPersistenceRequested,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !form.read(cx).selection_saving
            || form.read(cx).chooser_generation != event.chosen.generation
            || form.read(cx).selection_revision.as_ref() != Some(&event.expected)
            || form.read(cx).session_selection.as_ref() != Some(&event.chosen.selection)
        {
            return;
        }
        let request = event.clone();
        self.express_work(
            form,
            window,
            cx,
            move |work| {
                anyhow::ensure!(
                    request.chosen.selection.target() == &work.owner.target,
                    "Chosen target changed"
                );
                work.store()?.remember_if(
                    &work.journal,
                    &work.environment,
                    &request.expected,
                    &request.chosen.selection,
                    || work.current(),
                )
            },
            move |_, form, result, _, cx| {
                form.update(cx, |form, cx| match result {
                    Ok(saved) => form.selection_persisted(&event, &saved, cx),
                    Err(error) => form.selection_persistence_failed(&event, error, cx),
                })
            },
        );
    }

    fn express_forget(
        &mut self,
        form: Entity<Form>,
        event: SelectionForgetRequested,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if form.read(cx).pending_forget.as_ref() != Some(&event) {
            return;
        }
        let request = event.clone();
        self.express_work(
            form,
            window,
            cx,
            move |work| {
                anyhow::ensure!(
                    request.target == work.owner.target,
                    "Forgotten target changed"
                );
                work.store()?
                    .forget_if(&request.expected, || work.current())
            },
            move |view, form, result, window, cx| {
                if result.is_ok() {
                    if let Some(index) = view.express.active {
                        view.express.forms[index].recovered = None;
                    }
                }
                form.update(cx, |form, cx| match result {
                    Ok(saved) => form.selection_forgotten(&event, &saved, window, cx),
                    Err(error) => form.selection_forget_failed(&event, error, cx),
                });
            },
        );
    }

    fn express_sessions(
        &mut self,
        form: Entity<Form>,
        event: SessionsRequested,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !form.read(cx).catalog_loading
            || form.read(cx).chooser_generation != event.generation
            || form.read(cx).chooser_target.as_ref() != Some(&event.target)
        {
            return;
        }
        let request = event.clone();
        self.express_work(
            form,
            window,
            cx,
            move |work| {
                anyhow::ensure!(
                    request.target == work.owner.target,
                    "Chooser target changed"
                );
                model::list_retained(
                    work.gateway()?,
                    &work.journal,
                    &work.owner.state,
                    &work.environment,
                    &request.target,
                    request.cursor.as_ref(),
                )
            },
            move |_, form, result, _, cx| {
                form.update(cx, |form, cx| match result {
                    Ok(page) => form.present_sessions(&event, page, cx),
                    Err(error) => form.sessions_failed(&event, error, cx),
                })
            },
        );
    }

    fn express_selected(
        &mut self,
        form: Entity<Form>,
        chosen: SessionChosen,
        history: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !form.read(cx).selected_session().is_some_and(|now| {
            now.generation == chosen.generation && now.selection == chosen.selection
        }) {
            return;
        }
        let previous = self
            .express
            .active
            .and_then(|i| self.express.forms[i].recovered.clone());
        let request = chosen.clone();
        self.express_work(
            form,
            window,
            cx,
            move |work| {
                anyhow::ensure!(
                    request.selection.target() == &work.owner.target,
                    "Selected target changed"
                );
                if history {
                    model::continue_selected_history(
                        work.gateway()?,
                        &work.journal,
                        &work.owner.state,
                        &work.environment,
                        &request.selection,
                        previous.as_ref().ok_or_else(|| {
                            anyhow::anyhow!("No exact retained history to continue")
                        })?,
                    )
                } else {
                    model::reconnect_selected(
                        work.gateway()?,
                        &work.journal,
                        &work.owner.state,
                        &work.environment,
                        &request.selection,
                    )
                }
            },
            move |view, form, result, window, cx| match result {
                Ok(recovered) => {
                    // Form validates exact request, operation, source snapshot and
                    // chooser generation before presenting; keep the same token.
                    if form.read(cx).selected_session().is_some_and(|now| {
                        now.generation == chosen.generation && now.selection == chosen.selection
                    }) && chosen.selection.matches(&recovered)
                    {
                        if let Some(index) = view.express.active {
                            view.express.forms[index].recovered = Some(recovered.clone());
                        }
                    }
                    form.update(cx, |form, cx| {
                        form.recovered_selected(&chosen, &recovered, window, cx)
                    });
                }
                Err(error) => form.update(cx, |form, cx| form.selection_failed(&chosen, error, cx)),
            },
        );
    }

    fn express_close_session(
        &mut self,
        form: Entity<Form>,
        event: ClosureRequested,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.selection.as_ref().is_some_and(|chosen| {
            form.read(cx).selected_session().is_some_and(|now| {
                now.generation == chosen.generation && now.selection == chosen.selection
            })
        }) {
            return;
        }
        let recovered = self
            .express
            .active
            .and_then(|i| self.express.forms[i].recovered.clone());
        let request = event.clone();
        self.express_work(
            form,
            window,
            cx,
            move |work| {
                let chosen = request.selection.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("Choose the exact retained request before closing")
                })?;
                anyhow::ensure!(
                    chosen.selection.target() == &work.owner.target
                        && chosen.selection.request_id() == request.request_id,
                    "Closure target/request changed"
                );
                model::close_selected_if(
                    work.gateway()?,
                    &work.journal,
                    &work.owner.state,
                    &work.environment,
                    &chosen.selection,
                    recovered
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("No exact selected result"))?,
                    &request.action,
                    || work.current(),
                )
            },
            move |_, form, result, _, cx| {
                form.update(cx, |form, cx| match (result, &event.selection) {
                    (Ok(record), Some(chosen)) => {
                        form.closure_applied_selected(chosen, &record, cx)
                    }
                    (Err(error), _) => form.failed(error, true, cx),
                    _ => form.failed("Closure identity was not confirmed".into(), true, cx),
                })
            },
        );
    }

    fn express_request(
        &mut self,
        form: Entity<Form>,
        event: Requested,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if form.read(cx).request_nonce.as_ref() != Some(&event.1) {
            return;
        }
        // Express is an independent form, not the chat composer's typed draft.
        // Its absent explicit metadata means Chat; never borrow a type token
        // from the unrelated composer or clear that composer's draft.
        self.express_work(form, window, cx, move |work| {
            let target = &work.owner.target;
            let request = Request::prepare(work.gateway()?, &target.manager_project_id, &target.manager_thread_id, event.0)?;
            anyhow::ensure!(request.environment_id.as_ref() == Some(&target.environment_id), "Express environment changed");
            let saved = work.store()?.load()?;
            let mut attempted_id = None;
            let result = request.submit_with_retainer(work.gateway()?, &work.journal, &work.owner.state, &work.environment,
                || { work.current()?; anyhow::ensure!(work.owner.client.can_operate(), "Manager cannot send an Express request"); Ok(()) }, |message| {
                    work.current()?;
                    attempted_id = Some(message.client_request_id.clone());
                    retain_annotation(&work.owner.state, &work.environment, target, &message)?;
                    work.current()?;
                    let record = work.journal.prepare_message(message)?;
                    Ok(record)
                }).map_err(|e| format!("{e:#}"));
            let retained = attempted_id.is_some_and(|id| work.journal.records()
                .map(|records| records.iter().any(|record| record.message.client_request_id == id)).unwrap_or(true));
            Ok((result, retained, saved))
        }, move |_, form, result, _, cx| {
            form.update(cx, |form, cx| match result {
                Ok((result, retained, saved)) => {
                    form.request_nonce = None;
                    form.chooser_target = Some(saved.target);
                    form.selection_revision = Some(saved.revision);
                    match result {
                        Ok(submission) => {
                            form.accepted(&submission, cx);
                            form.notice = Some("Manager request retained. Choose this exact request to refresh its result; it was not selected automatically.".into());
                        }
                        Err(error) => form.failed(error, retained, cx),
                    }
                }
                Err(error) => form.failed(error, false, cx),
            });
        });
    }

    pub(super) fn render_express_session(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(session) = self.express.active.and_then(|i| self.express.forms.get(i)) else {
            return div().into_any_element();
        };
        v_flex()
            .id("manager-express-surface")
            .max_h(px(480.))
            .overflow_y_scroll()
            .child(session.form.clone())
            .child(
                Button::new("express-close-form")
                    .ghost()
                    .label("Close Express")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.express.revoke();
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}

/// The independent Express form has no type selector. Absence remains Chat;
/// it must not inherit a token or classification from the parked chat composer.
/// Retain this local source/owner annotation before the SAME journal message.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpressAnnotation {
    target: SessionTarget,
    message: okilum_t3::OutgoingMessage,
    metadata: Option<okilum_t3::message_types::Metadata>,
}
fn retain_annotation(
    state: &Path,
    environment: &Environment,
    target: &SessionTarget,
    message: &okilum_t3::OutgoingMessage,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        environment.origin() == target.origin && message.thread_id == target.manager_thread_id,
        "Express annotation belongs to another owner"
    );
    let store = okilum_t3::chat_drafts::Store::<ExpressAnnotation>::new(
        &state.join("t3-express-annotations"),
        environment,
        &target.manager_project_id,
        &target.manager_thread_id,
    );
    let saved = store.load()?;
    anyhow::ensure!(
        saved.draft.text.is_empty(),
        "Express annotations are malformed; nothing was sent"
    );
    let annotation = ExpressAnnotation {
        target: target.clone(),
        message: message.clone(),
        metadata: None,
    };
    let mut ids = std::collections::HashSet::new();
    for old in &saved.draft.attachments {
        anyhow::ensure!(
            old.target == *target
                && old.message.thread_id == target.manager_thread_id
                && old.metadata.is_none()
                && ids.insert(&old.message.client_request_id),
            "Express annotation identity is corrupt or ambiguous; nothing was sent"
        );
    }
    if let Some(old) = saved
        .draft
        .attachments
        .iter()
        .find(|old| old.message.client_request_id == message.client_request_id)
    {
        anyhow::ensure!(
            *old == annotation,
            "Retained Express identity has different source; nothing was sent"
        );
        return Ok(());
    }
    let mut draft = saved.draft;
    draft.attachments.push(annotation);
    store.save(saved.revision, draft)?;
    Ok(())
}

#[cfg(test)]
#[path = "express_session_tests.rs"]
mod tests;
