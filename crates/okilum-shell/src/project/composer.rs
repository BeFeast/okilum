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

pub(super) struct Composer {
    input: Entity<TextareaState>,
    enabled: bool,
    pub(super) mode: Mode,
    native: bool,
    steer: bool,
    busy: bool,
    _input: Subscription,
}
impl Composer {
    pub(super) fn new(input: Entity<TextareaState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
        Self {
            input,
            enabled: false,
            mode: Mode::Auto,
            native: false,
            steer: false,
            busy: false,
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
        v_flex().w_full().gap_1().child(modes).child(
            h_flex()
                .items_end()
                .gap_2()
                .child(Textarea::new(&self.input).flex_1())
                .child(
                    Button::new("project-send")
                        .debug_selector(|| "project-send".into())
                        .ghost()
                        .icon(IconName::ArrowUp)
                        .tooltip("Send to manager")
                        .disabled(
                            self.busy
                                || !self.enabled
                                || self.input.read(cx).value().trim().is_empty(),
                        )
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SendRequested))),
                ),
        )
    }
}
