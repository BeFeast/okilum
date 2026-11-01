//! Linux composer invalidates its own controls, never the history entity.
use super::*;
use gpui_component::ThemeStyled as _;
pub(super) mod history;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Auto,
    Queue,
    Steer,
}
pub(super) struct SendRequested;
impl EventEmitter<SendRequested> for Composer {}
pub(super) struct HistoryRecalled;
impl EventEmitter<HistoryRecalled> for Composer {}
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
    mode_open: bool,
    mode_focus: gpui::FocusHandle,
    mode_trigger_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<Pixels>>>,
    native: bool,
    steer: bool,
    busy: bool,
    has_attachments: bool,
    can_attach: bool,
    history_feed: Option<Entity<feed::Feed>>,
    history_position: Option<history::Position>,
    history_blocked: bool,
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
                && this.prompt_validation(cx).is_none()
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
            mode_open: false,
            mode_focus: cx.focus_handle(),
            mode_trigger_bounds: Default::default(),
            native: false,
            steer: false,
            busy: false,
            has_attachments: false,
            can_attach: false,
            history_feed: None,
            history_position: None,
            history_blocked: false,
            _input: subscription,
        }
    }
    pub(super) fn bind_history(&mut self, feed: Entity<feed::Feed>) {
        self.history_feed = Some(feed);
    }
    pub(super) fn reset_history(&mut self, cx: &mut Context<Self>) {
        if self.history_position.take().is_some() {
            cx.notify();
        }
    }
    pub(super) fn set_history_blocked(&mut self, blocked: bool) {
        self.history_blocked = blocked;
    }
    fn navigate_history(
        &mut self,
        direction: history::Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = window.modifiers();
        if modifiers.shift
            || modifiers.alt
            || modifiers.control
            || modifiers.platform
            || modifiers.function
        {
            return false;
        }
        let Some(feed) = &self.history_feed else {
            return false;
        };
        let input = self.input.read(cx);
        if self.history_blocked
            || self.has_attachments
            || feed.read(cx).history_has_requests(cx)
            || !input.focus_handle(cx).is_focused(window)
            || input.presentation().is_disabled()
            || !input.is_editable()
            || input.ime_settling(cx)
            || input.completion_active(cx)
            || !input.selected_range().is_empty()
        {
            return false;
        }
        let prompt = input.value().to_string();
        // Avoid both a layout read and a history rebuild for ordinary typing.
        if self.history_position.is_none() && !prompt.is_empty() {
            return false;
        }
        if !input.is_caret_on_visual_edge(direction == history::Direction::Forward) {
            return false;
        }
        let entries = feed.read(cx).prompt_history();
        let Some(step) =
            history::step(direction, &entries, self.history_position.as_ref(), &prompt)
        else {
            return false;
        };
        self.history_position = step.position;
        self.input.update(cx, |input, cx| {
            let end = step.prompt.len();
            input.set_value(step.prompt, window, cx);
            input.set_selected_range(end..end, cx);
        });
        // Programmatic set_value deliberately emits no InputEvent. Publish one
        // explicit recall event so the parent's existing draft writer captures
        // it, without teaching durable draft storage about history position.
        cx.emit(HistoryRecalled);
        cx.stop_propagation();
        cx.notify();
        true
    }
    fn prompt_validation(&self, cx: &App) -> Option<String> {
        self.native
            .then(|| {
                okilum_t3::actions::prompt_length_validation(self.input.read(cx).value().as_str())
            })
            .flatten()
    }
    pub(super) fn set_native(&mut self, native: bool, steer: bool, cx: &mut Context<Self>) {
        if (self.native, self.steer) != (native, steer) {
            self.native = native;
            self.steer = steer;
            if !native {
                self.mode_open = false;
            }
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
            if busy {
                self.mode_open = false;
            }
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

    fn close_send_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode_open = false;
        self.input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }
    fn render_send_mode(&self, cx: &mut Context<Self>) -> AnyElement {
        let bounds = self.mode_trigger_bounds.clone();
        let trigger = div()
            .id("project-send-mode-anchor")
            .relative()
            .on_prepaint(move |measured, _, _| bounds.set(measured))
            .child(
                Button::new("project-send-mode")
                    .debug_selector(|| "project-send-mode".into())
                    .ghost()
                    .small()
                    .icon(IconName::ChevronDown)
                    .selected(self.mode_open)
                    .accessibility_label("Choose send mode")
                    .tooltip("Choose send mode")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.mode_open {
                            this.close_send_mode(window, cx);
                        } else {
                            this.mode_open = true;
                            this.mode_focus.focus(window, cx);
                            cx.notify();
                        }
                    })),
            );
        if !self.mode_open {
            return trigger.into_any_element();
        }
        let mut menu = v_flex()
            .id("project-send-mode-menu")
            .debug_selector(|| "project-send-mode-menu".into())
            .role(gpui::Role::Menu)
            .track_focus(&self.mode_focus)
            .occlude()
            .on_prepaint(gpui_base::GlobalState::record_native_overlay)
            .popover_style(cx)
            .text_size(px(13.))
            .line_height(px(20.))
            .w(px(200.))
            .p_1()
            .gap_0p5()
            .on_mouse_down_out(
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    // The trigger owns its completed click. A pointer-down outside
                    // the menu but on that same trigger must not close then reopen.
                    if !this.mode_trigger_bounds.get().contains(&event.position) {
                        this.close_send_mode(window, cx);
                    }
                }),
            )
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close_send_mode(window, cx);
                    cx.stop_propagation();
                }
            }));
        for (mode, label, tooltip) in [
            (
                Mode::Auto,
                "Send",
                "Send normally; let the manager handle an active turn",
            ),
            (
                Mode::Queue,
                "Queue",
                "Queue this message after the current turn",
            ),
            (
                Mode::Steer,
                "Steer",
                "Send this message into the current turn",
            ),
        ] {
            menu = menu.child(
                Button::new(SharedString::from(format!("project-mode-{label}")))
                    .debug_selector(move || format!("project-mode-{label}"))
                    .ghost()
                    .small()
                    .w_full()
                    .justify_start()
                    .label(label)
                    .selected(self.mode == mode)
                    .toggled(self.mode == mode)
                    .tooltip(tooltip)
                    .disabled(self.busy || (mode == Mode::Steer && !self.steer))
                    .child(div().flex_1())
                    .child(div().w(px(14.)).when(self.mode == mode, |check| {
                        check.child(Icon::new(IconName::Check).small())
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.mode = mode;
                        this.close_send_mode(window, cx);
                    })),
            );
        }
        // The measured trigger stays in the composer flow; only the menu is
        // deferred. Opening must not change the trigger's bounds or its own
        // outside-click exclusion region. Side placement flips when the menu
        // cannot fit above, rather than clamping it over its own trigger.
        trigger
            .child(
                gpui::deferred(
                    gpui_base::Positioner::side(self.mode_trigger_bounds.get())
                        .placement(gpui_base::Placement::Top)
                        .align(gpui_base::Align::End)
                        .offset(px(4.))
                        .margin(px(12.))
                        .occlude()
                        .child(menu),
                )
                .with_priority(gpui_base::POPUP_PRIORITY),
            )
            .into_any_element()
    }
}
impl Render for Composer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let validation = self.prompt_validation(cx);
        v_flex()
            .id("project-composer")
            .relative()
            .w_full()
            .min_w_0()
            .gap_2()
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::MoveUp, window, cx| {
                    if !this.navigate_history(history::Direction::Backward, window, cx) {
                        cx.propagate();
                    }
                }),
            )
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::MoveDown, window, cx| {
                    if !this.navigate_history(history::Direction::Forward, window, cx) {
                        cx.propagate();
                    }
                }),
            )
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
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .debug_selector(|| "project-composer-input".into())
                    .child(Textarea::new(&self.input).w_full().aria_label("Message")),
            )
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("project-attach")
                            .debug_selector(|| "project-attach".into())
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .tooltip("Attach images or files")
                            .disabled(!self.can_attach)
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(AttachmentRequested::Choose)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .debug_selector(|| "project-send-controls".into())
                            .items_center()
                            .gap_0p5()
                            .child(
                                Button::new("project-send")
                                    .debug_selector(|| "project-send".into())
                                    .primary()
                                    .small()
                                    .icon(IconName::ArrowUp)
                                    .when(self.mode != Mode::Auto, |button| {
                                        button.label(if self.mode == Mode::Queue {
                                            "Queue"
                                        } else {
                                            "Steer"
                                        })
                                    })
                                    .accessibility_label(match self.mode {
                                        Mode::Auto => "Send message",
                                        Mode::Queue => "Queue message",
                                        Mode::Steer => "Steer current turn",
                                    })
                                    .tooltip(match self.mode {
                                        Mode::Auto => "Send to manager (Enter)",
                                        Mode::Queue => "Queue message (Enter)",
                                        Mode::Steer => "Steer current turn (Enter)",
                                    })
                                    .disabled(
                                        self.busy
                                            || validation.is_some()
                                            || !self.enabled
                                            || (!self.has_attachments
                                                && self.input.read(cx).value().trim().is_empty()),
                                    )
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SendRequested))),
                            )
                            .when(self.native, |group| group.child(self.render_send_mode(cx))),
                    ),
            )
            .when_some(validation, |composer, message| {
                composer.child(
                    div()
                        .debug_selector(|| "project-prompt-length-validation".into())
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(message),
                )
            })
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
    fn send_mode_disclosure_stays_with_send_and_enter_retains_each_explicit_mode(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let sent = Rc::new(RefCell::new(Vec::new()));
        let observed = sent.clone();
        let (root, visual) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| TextareaState::new(window, cx).submit_on_enter(true));
            let composer = cx.new(|cx| {
                let mut composer = Composer::new(input, cx);
                composer.set_native(true, true, cx);
                composer.set_controls(true, false, cx);
                composer.set_attachments(false, true, cx);
                cx.subscribe(&cx.entity(), move |this, _, _: &SendRequested, _| {
                    observed.borrow_mut().push(this.mode);
                })
                .detach();
                composer
            });
            Root::new(composer, window, cx)
        });
        visual.simulate_resize(size(px(320.), px(600.)));
        visual.run_until_parked();
        let composer = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Composer>().unwrap()
        });
        let input = visual.debug_bounds("project-composer-input").unwrap();
        let send = visual.debug_bounds("project-send").unwrap();
        let mode = visual.debug_bounds("project-send-mode").unwrap();
        let attach = visual.debug_bounds("project-attach").unwrap();
        assert!(
            input.size.width >= px(280.),
            "textarea retains the narrow-panel width"
        );
        assert!(attach.top() >= input.bottom() && send.top() >= input.bottom());
        assert_eq!(
            send.top(),
            mode.top(),
            "mode disclosure shares the Send baseline"
        );
        assert!(
            mode.left() >= send.right() && mode.left() - send.right() <= px(4.),
            "mode belongs to the send group"
        );
        assert!(visual.debug_bounds("project-mode-Send").is_none());
        assert!(visual.debug_bounds("project-mode-Queue").is_none());
        assert!(
            visual.debug_bounds("project-mode-Steer").is_none(),
            "idle composer has no always-visible mode strip"
        );
        for (index, (selector, expected)) in [
            ("project-mode-Send", Mode::Auto),
            ("project-mode-Queue", Mode::Queue),
            ("project-mode-Steer", Mode::Steer),
        ]
        .into_iter()
        .enumerate()
        {
            let trigger = visual.debug_bounds("project-send-mode").unwrap();
            visual.simulate_click(trigger.center(), Modifiers::default());
            visual.run_until_parked();
            let choice = visual
                .debug_bounds(selector)
                .expect("all supported modes remain reachable in the disclosure");
            visual.simulate_click(choice.center(), Modifiers::default());
            visual.run_until_parked();
            assert!(
                visual.debug_bounds("project-send-mode-menu").is_none(),
                "choosing mode closes its disclosure"
            );
            composer.update_in(visual, |composer, window, cx| {
                assert_eq!(composer.mode, expected);
                composer.input.update(cx, |input, cx| {
                    input.set_value(format!("Message {index}"), window, cx);
                    input.focus(window, cx);
                });
            });
            visual.run_until_parked();
            if expected != Mode::Auto {
                assert!(
                    visual.debug_bounds("project-send").unwrap().size.width > send.size.width,
                    "Queue/Steer is visibly named in the primary send control"
                );
            }
            visual.simulate_keystrokes("enter");
            visual.run_until_parked();
            assert_eq!(
                *sent.borrow(),
                [Mode::Auto, Mode::Queue, Mode::Steer][..=index]
            );
        }
        let trigger = visual.debug_bounds("project-send-mode").unwrap();
        visual.simulate_click(trigger.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("project-send-mode-menu").is_some());
        let menu = visual.debug_bounds("project-send-mode-menu").unwrap();
        let open_trigger = visual.debug_bounds("project-send-mode").unwrap();
        assert!(
            menu.bottom() <= open_trigger.top() || menu.top() >= open_trigger.bottom(),
            "menu cannot cover its closing trigger: menu={menu:?}, trigger={open_trigger:?}"
        );
        assert_eq!(
            open_trigger, trigger,
            "opening cannot move the disclosure trigger"
        );
        composer.read_with(visual, |composer, _| {
            assert!(composer.mode_trigger_bounds.get().contains(&open_trigger.center()),
                "outside-click exclusion uses the real trigger bounds: measured={:?}, trigger={open_trigger:?}", composer.mode_trigger_bounds.get());
        });
        visual.simulate_click(open_trigger.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("project-send-mode-menu").is_none(),
            "a second completed trigger click closes without reopening"
        );
        composer.read_with(visual, |composer, _| {
            assert_eq!(
                composer.mode,
                Mode::Steer,
                "closing the disclosure never selects a mode under its trigger"
            )
        });
        assert_eq!(
            sent.borrow().len(),
            3,
            "disclosure interactions never dispatch a message"
        );
        composer.update(visual, |composer, cx| composer.set_native(true, false, cx));
        composer.read_with(visual, |composer, _| {
            assert_eq!(
                composer.mode,
                Mode::Auto,
                "a lost Steer capability returns to the safe existing mode"
            )
        });
    }

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
    #[gpui::test]
    fn oversized_prompt_blocks_enter_and_click_without_changing_the_draft(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let sends = Rc::new(Cell::new(0));
        let observed = sends.clone();
        let (root, visual) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| TextareaState::new(window, cx).submit_on_enter(true));
            let composer = cx.new(|cx| {
                let mut composer = Composer::new(input, cx);
                composer.set_native(true, false, cx);
                composer.set_controls(true, false, cx);
                cx.subscribe(&cx.entity(), move |_, _, _: &SendRequested, _| {
                    observed.set(observed.get() + 1);
                })
                .detach();
                composer
            });
            Root::new(composer, window, cx)
        });
        let composer = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Composer>().unwrap()
        });
        let oversized = "a".repeat(120_001);
        composer.update_in(visual, |composer, window, cx| {
            composer.input.update(cx, |input, cx| {
                input.set_value(oversized.clone(), window, cx);
                input.focus(window, cx);
            });
            assert_eq!(composer.prompt_validation(cx).as_deref(), Some("Prompt is 1 character over the 120,000-character limit. Shorten or split it before sending."));
        });
        visual.run_until_parked();
        assert!(visual
            .debug_bounds("project-prompt-length-validation")
            .is_some());
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        let send = visual.debug_bounds("project-send").unwrap().center();
        visual.simulate_click(send, Modifiers::default());
        visual.run_until_parked();
        assert_eq!(sends.get(), 0);
        composer.read_with(visual, |composer, cx| {
            assert_eq!(composer.input.read(cx).value().as_str(), oversized);
        });
        composer.update_in(visual, |composer, window, cx| {
            composer.input.update(cx, |input, cx| {
                input.set_value("a".repeat(120_000), window, cx);
                input.focus(window, cx);
            });
        });
        visual.run_until_parked();
        assert!(visual
            .debug_bounds("project-prompt-length-validation")
            .is_none());
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert_eq!(
            sends.get(),
            1,
            "positive control: the boundary prompt dispatches once"
        );
    }
    #[gpui::test]
    fn prompt_history_keys_preserve_typed_selected_attached_and_modified_input(
        cx: &mut TestAppContext,
    ) {
        use okilum_t3::projection::{Changes, RowKey, TurnRow};
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let mut project = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = super::super::fixture::view(
                    temp.path().to_owned(),
                    Arc::new(super::super::fixture::Fake::default()),
                    window,
                    cx,
                );
                view.active = false;
                view.compose.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.focus(window, cx);
                });
                view
            });
            let composer = view.read(cx).composer.clone();
            project = Some(view);
            Root::new(composer, window, cx)
        });
        let project = project.unwrap();
        let composer = project.read_with(visual, |view, _| view.composer.clone());
        project.update(visual, |view, cx| view.feed.update(cx, |feed, cx| {
            let key = |id: &str| RowKey { thread: "history".into(), item: id.into() };
            let row = |id: &str, text: &str| Arc::new(TurnRow { key: key(id), local: true, item: Arc::new(serde_json::json!({"id":id,"type":"user_message","text":text,"status":"completed"})) });
            feed.apply_native(Changes { rows: vec![row("first", "first"), row("last", "second")], order: Some(vec![key("first"), key("last")]), ..Default::default() }, &[], cx);
        }));
        visual.simulate_resize(size(px(600.), px(600.)));
        visual.run_until_parked();
        composer.update_in(visual, |composer, window, cx| {
            composer
                .input
                .update(cx, |input, cx| input.focus(window, cx))
        });
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(
                composer
                    .history_feed
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .prompt_history()
                    .len(),
                2,
                "positive control: visible thread has two recallable user prompts"
            );
            assert!(
                composer.input.read(cx).is_caret_on_visual_edge(false),
                "positive control: empty input is on its top visual edge"
            );
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(
                composer.input.read(cx).value().as_str(),
                "second",
                "positive control: unmodified Up recalled the latest prompt"
            )
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(composer.input.read(cx).value().as_str(), "first")
        });
        visual.simulate_keystrokes("down");
        visual.run_until_parked();
        visual.simulate_keystrokes("down");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert!(composer.input.read(cx).value().is_empty())
        });
        // Any modifier is native caret/selection navigation, never recall.
        for key in ["shift-up", "ctrl-up", "alt-up", "cmd-up"] {
            visual.simulate_keystrokes(key);
            visual.run_until_parked();
            composer.read_with(visual, |composer, cx| {
                assert!(
                    composer.input.read(cx).value().is_empty(),
                    "{key} recalled text"
                )
            });
        }
        composer.update_in(visual, |composer, window, cx| {
            composer
                .input
                .update(cx, |input, cx| input.set_value("typed", window, cx));
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(composer.input.read(cx).value().as_str(), "typed")
        });
        composer.update_in(visual, |composer, window, cx| {
            composer
                .input
                .update(cx, |input, cx| input.set_value("", window, cx));
            composer.set_attachments(true, true, cx);
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert!(composer.input.read(cx).value().is_empty())
        });
        composer.update(visual, |composer, cx| {
            composer.set_attachments(false, true, cx);
            composer.set_history_blocked(true);
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert!(
                composer.input.read(cx).value().is_empty(),
                "pending question must block recall"
            )
        });
        composer.update(visual, |composer, _| composer.set_history_blocked(false));
        project.update(visual, |view, cx| {
            view.native.actions.update(cx, |actions, _| {
                actions.state.requests.push(okilum_t3::actions::Request {
                    id: "pending".into(),
                    item: serde_json::json!({"type":"approval_request"}),
                    answerable: true,
                    dismissible: false,
                    response_mode: Some("live".into()),
                    approval_detail: None,
                    reason: None,
                });
            })
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert!(
                composer.input.read(cx).value().is_empty(),
                "live native approval blocks recall without waiting for a parent render"
            )
        });
        project.update(visual, |view, cx| {
            view.native
                .actions
                .update(cx, |actions, _| actions.state.requests.clear())
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.update(visual, |composer, cx| {
            composer
                .input
                .update(cx, |input, cx| input.set_selected_range(0..3, cx))
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(
                composer.input.read(cx).value().as_str(),
                "second",
                "selected recall must not be replaced with older text"
            )
        });
    }
    #[gpui::test]
    fn prompt_history_respects_soft_wrap_then_uses_resized_visual_edges(cx: &mut TestAppContext) {
        use gpui::EntityInputHandler as _;
        use okilum_t3::projection::{Changes, RowKey, TurnRow};
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let long = "one two three four five six seven eight nine ten ".repeat(3);
        let mut project = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = super::super::fixture::view(
                    temp.path().to_owned(),
                    Arc::new(super::super::fixture::Fake::default()),
                    window,
                    cx,
                );
                view.active = false;
                view.compose.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.focus(window, cx);
                });
                view
            });
            let composer = view.read(cx).composer.clone();
            project = Some(view);
            Root::new(composer, window, cx)
        });
        let project = project.unwrap();
        let composer = project.read_with(visual, |view, _| view.composer.clone());
        project.update(visual, |view, cx| view.feed.update(cx, |feed, cx| {
            let key = |id: &str| RowKey { thread: "wrap-history".into(), item: id.into() };
            let row = |id: &str, text: &str| Arc::new(TurnRow { key: key(id), local: true, item: Arc::new(serde_json::json!({"id":id,"type":"user_message","text":text,"status":"completed"})) });
            feed.apply_native(Changes { rows: vec![row("first", "first"), row("last", &long)], order: Some(vec![key("first"), key("last")]), ..Default::default() }, &[], cx);
        }));
        visual.simulate_resize(size(px(260.), px(600.)));
        visual.run_until_parked();
        composer.update_in(visual, |composer, window, cx| {
            composer
                .input
                .update(cx, |input, cx| input.focus(window, cx))
        });
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert!(
                composer.input.read(cx).is_caret_on_visual_edge(false),
                "positive control: empty input is on its top edge before recall"
            )
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(composer.input.read(cx).value().as_str(), long.trim());
            assert!(
                !composer.input.read(cx).is_caret_on_visual_edge(false),
                "positive control: recalled prompt ends below its first visual row"
            );
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(
                composer.input.read(cx).value().as_str(),
                long.trim(),
                "Up inside a wrapped prompt moves the caret instead of replacing text"
            )
        });
        visual.simulate_resize(size(px(4000.), px(600.)));
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert!(
                composer.input.read(cx).is_caret_on_visual_edge(false),
                "positive control: wide resize puts the same caret on the first row"
            )
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(composer.input.read(cx).value().as_str(), "first")
        });
        composer.update_in(visual, |composer, window, cx| {
            composer.input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(Some(0..5), "first", Some(5..5), window, cx);
            })
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("down");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(
                composer.input.read(cx).value().as_str(),
                "first",
                "IME candidate navigation must not recall a newer prompt"
            )
        });
    }
}
