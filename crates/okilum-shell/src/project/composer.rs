//! Linux composer invalidates its own controls, never the history entity.
use super::*;
pub(super) mod history;

#[derive(Clone, Copy, PartialEq, Eq)]
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
        let validation = self.prompt_validation(cx);
        let modes = if self.native {
            [
                (Mode::Auto, "Send"),
                (Mode::Queue, "Queue"),
                (Mode::Steer, "Steer"),
            ]
            .into_iter()
            .map(|(mode, label)| {
                Button::new(SharedString::from(format!("project-mode-{label}")))
                    .debug_selector(move || format!("project-mode-{label}"))
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
                                    || validation.is_some()
                                    || !self.enabled
                                    || (!self.has_attachments
                                        && self.input.read(cx).value().trim().is_empty()),
                            )
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SendRequested))),
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
