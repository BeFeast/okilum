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
            .id("project-composer")
            .relative()
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
                        cx.stop_propagation();
                        cx.emit(AttachmentRequested::Paste);
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(brand::palette(cx).selected))
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

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    #[gpui::test]
    fn native_paste_consumes_the_action_once_and_legacy_paste_still_works(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let received = Rc::new(Cell::new(0));
        let insert = Rc::new(Cell::new(false));
        let dropped = Rc::new(RefCell::new(Vec::new()));
        let (root, visual) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| TextareaState::new(window, cx));
            let composer = cx.new(|cx| {
                let mut composer = Composer::new(input, cx);
                composer.set_native(true, false, cx);
                composer.set_controls(true, false, cx);
                let received = received.clone();
                let insert = insert.clone();
                let dropped = dropped.clone();
                cx.subscribe_in(
                    &cx.entity(),
                    window,
                    move |this, _, event: &AttachmentRequested, window, cx| {
                        if let AttachmentRequested::Paths(paths) = event {
                            dropped.borrow_mut().extend(paths.clone());
                        }
                        if matches!(event, AttachmentRequested::Paste) {
                            received.set(received.get() + 1);
                            // File paste stages a row without inserting its URI.
                            // Text paste uses the same production InputState::insert.
                            if insert.get() {
                                let text = cx.read_from_clipboard().unwrap().text().unwrap();
                                this.input
                                    .update(cx, |input, cx| input.insert(text, window, cx));
                            }
                        }
                    },
                )
                .detach();
                composer
            });
            Root::new(composer, window, cx)
        });
        let composer = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Composer>().unwrap()
        });
        composer.update_in(visual, |composer, window, cx| {
            composer
                .input
                .update(cx, |input, cx| input.focus(window, cx))
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                "file:///typed-file.txt".into(),
            ))
        });
        visual.simulate_keystrokes("ctrl-v");
        visual.run_until_parked();
        assert_eq!(received.get(), 1);
        composer.read_with(visual, |composer, cx| {
            assert!(
                composer.input.read(cx).value().is_empty(),
                "file URI must not reach the default text Paste handler"
            )
        });
        insert.set(true);
        visual.update(|_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                "First line\nSecond line".into(),
            ))
        });
        visual.simulate_keystrokes("ctrl-v");
        visual.run_until_parked();
        assert_eq!(received.get(), 2);
        composer.update_in(visual, |composer, window, cx| {
            assert_eq!(
                composer.input.read(cx).value().as_str(),
                "First line\nSecond line"
            );
            composer
                .input
                .update(cx, |input, cx| input.set_value("", window, cx));
            composer.set_native(false, false, cx);
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("ctrl-v");
        visual.run_until_parked();
        assert_eq!(
            received.get(),
            2,
            "legacy text Paste must use its original handler"
        );
        composer.read_with(visual, |composer, cx| {
            assert_eq!(
                composer.input.read(cx).value().as_str(),
                "First line\nSecond line"
            )
        });
        composer.update(visual, |composer, cx| composer.set_native(true, false, cx));
        visual.run_until_parked();
        let center = visual
            .debug_bounds("project-composer-input")
            .unwrap()
            .center();
        let paths = vec![
            PathBuf::from("/qa/screenshot.png"),
            PathBuf::from("/qa/report.pdf"),
            PathBuf::from("/qa/notes.txt"),
        ];
        // A real file drag moves the pointer before entering the drop target.
        // FileDropEvent alone does not leave GPUI's keyboard modality.
        visual.simulate_event(gpui::MouseMoveEvent {
            position: center,
            pressed_button: Some(MouseButton::Left),
            ..Default::default()
        });
        visual.run_until_parked();
        visual.update(|window, _| assert!(!window.last_input_was_keyboard()));
        visual.simulate_event(gpui::FileDropEvent::Entered {
            position: center,
            paths: ExternalPaths(paths.clone().into_iter().collect()),
        });
        visual.run_until_parked();
        visual.simulate_event(gpui::FileDropEvent::Submit { position: center });
        visual.run_until_parked();
        assert_eq!(
            *dropped.borrow(),
            paths,
            "native file-drop events must retain all typed paths"
        );
    }
}
