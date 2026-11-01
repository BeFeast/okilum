//! Express form. The owner handles Requested on its serial background worker
//! using okilum_t3::express_session::Request and the existing Manager journal.
use super::*;
use okilum_t3::express_session::{
    CloseReason, ClosureRecord, Draft, Platform, Recovered, SourceContext, Submission,
};

#[derive(Clone)]
pub(super) struct Requested(pub Draft);

#[derive(Clone)]
pub(super) struct RefreshRequested;

#[derive(Clone)]
pub(super) struct ContinueHistoryRequested;

#[derive(Clone)]
pub(super) enum ClosureAction {
    FollowUpPr,
    Discard(String),
}

#[derive(Clone)]
pub(super) struct ClosureRequested {
    pub request_id: String,
    pub action: ClosureAction,
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
    _inputs: Vec<Subscription>,
}

impl EventEmitter<Requested> for Form {}
impl EventEmitter<RefreshRequested> for Form {}
impl EventEmitter<ContinueHistoryRequested> for Form {}
impl EventEmitter<ClosureRequested> for Form {}

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
            _inputs: inputs,
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
        self.conversation_link = Some(recovered.conversation_link.clone());
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
        cx.emit(ClosureRequested { request_id, action });
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
            .child("Express session")
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
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.refreshing {
                                this.no_retained_request(cx);
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
        if self.locked {
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
