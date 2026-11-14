//! Okilum's own dialog for questions OpenSSH asks while opening an
//! `ssh://` Markdown link (#1610): key passphrase, password, 2FA code, or
//! trust for an unknown host. ssh-agent and keys are tried first by OpenSSH;
//! this window appears only when OpenSSH runs `SSH_ASKPASS`. Closing the
//! window, Escape or Cancel answers nothing and the connection fails.
use crate::*;

#[cfg(unix)]
use okilum_core::ssh_askpass::{Kind, Request};

/// Start the bridge before the first remote connection. Failure leaves
/// connections non-interactive (agent/keys only), as before.
pub(crate) fn ensure(cx: &mut App) {
    #[cfg(unix)]
    {
        struct Installed;
        impl Global for Installed {}
        if cx.has_global::<Installed>() {
            return;
        }
        let helper = match std::env::current_exe() {
            Ok(helper) => helper,
            Err(error) => {
                diagnostic_error!("SSH askpass: no helper path: {error}");
                return;
            }
        };
        let (sender, receiver) = async_channel::unbounded::<Request>();
        if let Err(error) = okilum_core::ssh_askpass::install(helper, move |request| {
            // A closed receiver drops the request, which answers Cancel.
            let _ = sender.try_send(request);
        }) {
            diagnostic_error!("SSH askpass: {error}");
            return;
        }
        cx.set_global(Installed);
        cx.spawn(async move |cx| {
            while let Ok(request) = receiver.recv().await {
                cx.update(|cx| show(request, cx));
            }
        })
        .detach();
    }
    #[cfg(not(unix))]
    let _ = cx;
}

#[cfg(unix)]
fn show(request: Request, cx: &mut App) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(window_state::default_bounds(
            size(px(520.), px(260.)),
            cx,
        ))),
        window_min_size: Some(size(px(420.), px(220.))),
        focus: true,
        ..TitleBar::window_options()
    };
    let opened = cx.open_window(options, move |window, cx| {
        window.set_window_title(title(request.kind));
        let prompt = cx.new(|cx| Prompt::new(request, window, cx));
        window.activate_window();
        cx.new(|cx| Root::new(prompt, window, cx))
    });
    // On failure the request was dropped with the closure: Cancel.
    if let Err(error) = opened {
        diagnostic_error!("SSH askpass window: {error}");
    }
}

#[cfg(unix)]
fn title(kind: Kind) -> &'static str {
    match kind {
        Kind::Secret => "SSH authentication",
        Kind::Confirm => "SSH host verification",
    }
}

#[cfg(unix)]
pub(crate) struct Prompt {
    /// Dropping the window drops the request, which answers Cancel.
    request: Option<Request>,
    kind: Kind,
    lines: Vec<String>,
    input: Option<Entity<InputState>>,
    _enter: Option<Subscription>,
    /// Closes the window once the connection is stopped or ends elsewhere.
    _watch: Task<()>,
    focus: FocusHandle,
}

#[cfg(unix)]
impl Prompt {
    fn new(request: Request, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let kind = request.kind;
        let lines = request
            .prompt
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        let (input, enter) = if kind == Kind::Secret {
            let input = cx.new(|cx| InputState::new(window, cx).masked(true));
            let enter = cx.subscribe_in(&input, window, |this, _, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit(window, cx);
                }
            });
            input.update(cx, |input, cx| input.focus(window, cx));
            (Some(input), Some(enter))
        } else {
            (None, None)
        };
        let focus = cx.focus_handle();
        if input.is_none() {
            focus.focus(window, cx);
        }
        let watch = cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
            let closed = this.update_in(cx, |this, window, _| {
                let stopped = this
                    .request
                    .as_ref()
                    .is_none_or(|request| request.is_cancelled());
                if stopped {
                    this.answer(None, window);
                }
                stopped
            });
            if closed.unwrap_or(true) {
                break;
            }
        });
        Self {
            request: Some(request),
            kind,
            lines,
            input,
            _enter: enter,
            _watch: watch,
            focus,
        }
    }
    fn answer(&mut self, answer: Option<String>, window: &mut Window) {
        if let Some(request) = self.request.take() {
            request.answer(answer);
        }
        window.remove_window();
    }
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = match (&self.input, self.kind) {
            (Some(input), _) => Some(input.read(cx).value().to_string()),
            // OpenSSH accepts "yes" to trust and record an unknown host key.
            (None, Kind::Confirm) => Some("yes".to_owned()),
            (None, Kind::Secret) => None,
        };
        self.answer(answer, window);
    }
}

#[cfg(unix)]
impl Render for Prompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let confirm = self.kind == Kind::Confirm;
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => this.answer(None, window),
                    "enter" if this.input.is_none() => this.submit(window, cx),
                    _ => {}
                }
            }))
            .child(TitleBar::new().child(title(self.kind)))
            .child(
                v_flex()
                    .id("ssh-askpass-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .gap_2()
                    .children(
                        self.lines
                            .iter()
                            .map(|line| div().text_sm().whitespace_normal().child(line.clone())),
                    )
                    .children(self.input.as_ref().map(|input| Input::new(input).w_full()))
                    .child(div().text_xs().text_color(muted).child(if confirm {
                        "Trusting adds this host key to ~/.ssh/known_hosts, as ssh does in a terminal."
                    } else {
                        "Sent only to OpenSSH for this connection; Okilum does not store it."
                    })),
            )
            .child(
                h_flex()
                    .p_3()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("ssh-askpass-cancel")
                            .debug_selector(|| "ssh-askpass-cancel".into())
                            .ghost()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, _| this.answer(None, window))),
                    )
                    .child(
                        Button::new("ssh-askpass-ok")
                            .debug_selector(|| "ssh-askpass-ok".into())
                            .primary()
                            .label(if confirm { "Trust and connect" } else { "Connect" })
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn open<'a>(
        prompt: &str,
        kind: Kind,
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<Prompt>,
        &'a mut VisualTestContext,
        std::sync::mpsc::Receiver<Option<String>>,
    ) {
        let (request, reply, _ended) = Request::detached(prompt, kind);
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let prompt = cx.new(|cx| Prompt::new(request, window, cx));
            entity = Some(prompt.clone());
            Root::new(prompt, window, cx)
        });
        visual.run_until_parked();
        (entity.unwrap(), visual, reply)
    }

    #[gpui::test]
    fn passphrase_is_answered_masked_and_only_once(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let (prompt, visual, reply) = open(
            "Enter passphrase for key '/home/u/.ssh/id_ed25519': ",
            Kind::Secret,
            cx,
        );
        prompt.update_in(visual, |prompt, window, cx| {
            let input = prompt.input.clone().expect("secret prompts have an input");
            assert!(input.read(cx).presentation().is_masked());
            input.update(cx, |input, cx| input.set_value("hunter2", window, cx));
            prompt.submit(window, cx);
        });
        assert_eq!(reply.try_recv().unwrap().as_deref(), Some("hunter2"));
        assert!(reply.try_recv().is_err(), "answered once");
    }

    #[gpui::test]
    fn unknown_host_trust_answers_yes_and_cancel_answers_nothing(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let (prompt, visual, reply) = open(
            "The authenticity of host 'box (10.0.0.1)' can't be established.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ",
            Kind::Confirm,
            cx,
        );
        prompt.update_in(visual, |prompt, window, cx| {
            assert!(prompt.input.is_none());
            assert_eq!(prompt.lines.len(), 2);
            prompt.submit(window, cx);
        });
        assert_eq!(reply.try_recv().unwrap().as_deref(), Some("yes"));

        let (prompt, visual, reply) = open("u@box's password: ", Kind::Secret, cx);
        prompt.update_in(visual, |prompt, window, _| prompt.answer(None, window));
        assert_eq!(reply.try_recv().unwrap(), None);
    }

    #[gpui::test]
    fn stopping_the_connection_closes_the_open_dialog(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let (request, reply, ended) = Request::detached("u@box's password: ", Kind::Secret);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let prompt = cx.new(|cx| Prompt::new(request, window, cx));
            Root::new(prompt, window, cx)
        });
        visual.run_until_parked();
        assert!(reply.try_recv().is_err(), "still waiting for the person");
        // The remote view's Stop drops the askpass session.
        ended.store(true, std::sync::atomic::Ordering::Release);
        visual
            .executor()
            .advance_clock(std::time::Duration::from_millis(250));
        visual.run_until_parked();
        assert_eq!(reply.try_recv().unwrap(), None);
    }

    #[gpui::test]
    fn closing_the_window_cancels(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let (prompt, visual, reply) = open("Verification code: ", Kind::Secret, cx);
        drop(prompt);
        visual.update(|window, _| window.remove_window());
        visual.run_until_parked();
        assert_eq!(
            reply
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            None
        );
    }
}
