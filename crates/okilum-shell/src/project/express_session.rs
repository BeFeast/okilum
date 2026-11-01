//! Express form. The owner handles Requested on its serial background worker
//! using okilum_t3::express_session::Request and the existing Manager journal.
use super::*;
use okilum_t3::express_session::{Draft, Platform, SourceContext, Submission};

#[derive(Clone)]
pub(super) struct Requested(pub Draft);

pub(super) struct Form {
    goal: Entity<InputState>,
    recipient: Entity<InputState>,
    platforms: Vec<Platform>,
    source: Option<SourceContext>,
    locked: bool,
    notice: Option<String>,
    conversation_link: Option<String>,
    _inputs: Vec<Subscription>,
}

impl EventEmitter<Requested> for Form {}

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
            notice: None,
            conversation_link: None,
            _inputs: inputs,
        }
    }

    /// Call only for this form's still-current project/connection generation.
    pub(super) fn accepted(&mut self, submission: &Submission, cx: &mut Context<Self>) {
        self.locked = true;
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
        self.notice = Some(detail);
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
        panel
    }
}
