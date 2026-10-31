//! Linux composer invalidates its own controls, never the history entity.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Auto,
    Queue,
    Steer,
}
pub(super) struct SendRequested;
impl EventEmitter<SendRequested> for Composer {}
pub(super) enum AttachmentRequested {
    Choose,
    Paths(Vec<PathBuf>),
    Paste,
}
impl EventEmitter<AttachmentRequested> for Composer {}

pub(super) struct Composer {
    input: Entity<TextareaState>,
    enabled: bool,
    pub(super) mode: Mode,
    native: bool,
    steer: bool,
    busy: bool,
    has_attachments: bool,
    can_attach: bool,
    _input: Subscription,
}
impl Composer {
    pub(super) fn new(input: Entity<TextareaState>, cx: &mut Context<Self>) -> Self {
        // Match Reader's IME commit settle policy: some input methods deliver
        // the candidate-confirming Enter after the marked range is cleared.
        input.update(cx, |input, _| {
            input.set_ime_settle(Duration::from_millis(500))
        });
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { shift: false, .. })
                && this.enabled
                && !this.busy
                && !this.input.read(cx).ime_settling(cx)
                && (this.has_attachments || !this.input.read(cx).value().trim().is_empty())
            {
                cx.emit(SendRequested);
            }
            cx.notify();
        });
        Self {
            input,
            enabled: false,
            mode: Mode::Auto,
            native: false,
            steer: false,
            busy: false,
            has_attachments: false,
            can_attach: false,
            _input: subscription,
        }
    }
    pub(super) fn set_native(&mut self, native: bool, steer: bool, cx: &mut Context<Self>) {
        if (self.native, self.steer) != (native, steer) {
            self.native = native;
            self.steer = steer;
            if !steer && self.mode == Mode::Steer {
                self.mode = Mode::Auto;
            }
            cx.notify();
        }
    }
    pub(super) fn set_controls(&mut self, enabled: bool, busy: bool, cx: &mut Context<Self>) {
        if (self.enabled, self.busy) != (enabled, busy) {
            self.enabled = enabled;
            self.busy = busy;
            cx.notify();
        }
    }
    pub(super) fn set_attachments(
        &mut self,
        has_attachments: bool,
        can_attach: bool,
        cx: &mut Context<Self>,
    ) {
        if (self.has_attachments, self.can_attach) != (has_attachments, can_attach) {
            self.has_attachments = has_attachments;
            self.can_attach = can_attach;
            cx.notify();
        }
    }
}
impl Render for Composer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let modes = if self.native {
            [
                (Mode::Auto, "Send"),
                (Mode::Queue, "Queue"),
                (Mode::Steer, "Steer"),
            ]
            .into_iter()
            .map(|(mode, label)| {
                Button::new(SharedString::from(format!("project-mode-{label}")))
                    .ghost()
                    .label(format!(
                        "{}{label}",
                        if self.mode == mode { "✓ " } else { "" }
                    ))
                    .disabled(self.busy || (mode == Mode::Steer && !self.steer))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.mode = mode;
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let modes = h_flex().gap_1().children(modes);
        v_flex()
            .w_full()
            .gap_1()
            // Textarea propagates submit-on-enter. Consume that action here so
            // the platform input fallback cannot insert a newline before send.
            .on_action(|_: &gpui_component::input::Enter, _, _| {})
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Paste, window, cx| {
                    if this.native
                        && this.input.read(cx).focus_handle(cx).is_focused(window)
                        && !this.input.read(cx).presentation().is_disabled()
                    {
                        cx.emit(AttachmentRequested::Paste);
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .on_drop(cx.listener(|_, paths: &ExternalPaths, _, cx| {
                cx.emit(AttachmentRequested::Paths(paths.0.to_vec()))
            }))
            .child(modes)
            .child(
                h_flex()
                    .items_end()
                    .gap_2()
                    .child(
                        Button::new("project-attach")
                            .debug_selector(|| "project-attach".into())
                            .ghost()
                            .label("Attach")
                            .tooltip("Attach images or files")
                            .disabled(!self.can_attach)
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(AttachmentRequested::Choose)),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .debug_selector(|| "project-composer-input".into())
                            .child(Textarea::new(&self.input).w_full().aria_label("Message")),
                    )
                    .child(
                        Button::new("project-send")
                            .debug_selector(|| "project-send".into())
                            .ghost()
                            .icon(IconName::ArrowUp)
                            .tooltip("Send to manager")
                            .disabled(
                                self.busy
                                    || !self.enabled
                                    || (!self.has_attachments
                                        && self.input.read(cx).value().trim().is_empty()),
                            )
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SendRequested))),
                    ),
            )
    }
}
