//! Linux composer invalidates its own controls, never the history entity.
use super::*;

pub(super) struct SendRequested;
impl EventEmitter<SendRequested> for Composer {}

pub(super) struct Composer {
    input: Entity<TextareaState>,
    enabled: bool,
    busy: bool,
    _input: Subscription,
}
impl Composer {
    pub(super) fn new(input: Entity<TextareaState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
        Self {
            input,
            enabled: false,
            busy: false,
            _input: subscription,
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
                        self.busy || !self.enabled || self.input.read(cx).value().trim().is_empty(),
                    )
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SendRequested))),
            )
    }
}
