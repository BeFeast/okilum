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
        Self {
            request: Some(request),
            kind,
            lines,
            input,
            _enter: enter,
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
