//! Express form. The owner handles Requested on its serial background worker
//! using okilum_t3::express_session::Request and the existing Manager journal.
use super::*;
use okilum_t3::express_session::{Draft, Platform, Recovered, SourceContext, Submission};

#[derive(Clone)]
pub(super) struct Requested(pub Draft);

#[derive(Clone)]
pub(super) struct RefreshRequested;

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
    _inputs: Vec<Subscription>,
}

impl EventEmitter<Requested> for Form {}
impl EventEmitter<RefreshRequested> for Form {}

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
        let inputs = [&goal, &recipient]
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
            _inputs: inputs,
        }
    }

    /// Call only for this form's still-current project/connection generation.
    pub(super) fn accepted(&mut self, submission: &Submission, cx: &mut Context<Self>) {
        self.locked = true;
        self.refreshing = false;
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
        self.notice = Some(detail);
        cx.notify();
    }

    /// Lock before the serial worker starts so reconnect cannot race a new intent.
    pub(super) fn begin_reconnect(&mut self, cx: &mut Context<Self>) {
        self.locked = true;
        self.refreshing = true;
        self.build_links.clear();
        self.executor_link = None;
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
