//! Lightweight confirmation dialog before inserting a reminder line (#724).
//! Prefilled with the trimmed selected sentence and resolved date; Enter
//! confirms and Escape cancels with no modal chain.
use super::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::WindowExt as _;

struct ConfirmDialog {
    reader: WeakEntity<Reader>,
    request: selection::Request,
    heading: Option<String>,
    text_input: Entity<InputState>,
    date_input: Entity<InputState>,
    error: Option<String>,
    submitted: bool,
    _subscriptions: Vec<Subscription>,
}

impl ConfirmDialog {
    fn new(
        reader: WeakEntity<Reader>,
        request: selection::Request,
        sentence: String,
        heading: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_date = request.date().to_string();
        let text_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Reminder text");
            input.set_value(sentence.trim().to_owned(), window, cx);
            input
        });
        let date_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("YYYY-MM-DD");
            input.set_value(initial_date, window, cx);
            input
        });
        let text_sub = cx.subscribe_in(
            &text_input,
            window,
            |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    this.submit(window, cx);
                }
                InputEvent::Change => {
                    if this.error.take().is_some() {
                        cx.notify();
                    }
                }
                _ => {}
            },
        );
        let date_sub = cx.subscribe_in(
            &date_input,
            window,
            |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    this.submit(window, cx);
                }
                InputEvent::Change => {
                    if this.error.take().is_some() {
                        cx.notify();
                    }
                }
                _ => {}
            },
        );
        Self {
            reader,
            request,
            heading,
            text_input,
            date_input,
            error: None,
            submitted: false,
            _subscriptions: vec![text_sub, date_sub],
        }
    }

    fn focus_first(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.text_input.update(cx, |input, cx| {
            input.focus(window, cx);
        });
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.submitted {
            return true;
        }
        let text = self.text_input.read(cx).value().to_string();
        let date_raw = self.date_input.read(cx).value().to_string();
        let Some(date) = reminder_dates::parse(&date_raw, today()) else {
            self.error = Some("Enter a valid date (for example 2026-11-01 or 1 Nov 2026).".into());
            cx.notify();
            return false;
        };
        let line = match reminder_task::format(
            &text,
            self.request.document(),
            self.heading.as_deref(),
            date,
        ) {
            Ok(line) => line,
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return false;
            }
        };
        self.submitted = true;
        let request = self.request.clone();
        let reader = self.reader.clone();
        window.close_dialog(cx);
        let _ = reader.update(cx, |reader, cx| {
            request.apply_line(reader, line, date, window, cx);
        });
        true
    }
}

impl Render for ConfirmDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let destination = self.request.destination().to_owned();
        v_flex()
            .debug_selector(|| "remind-confirm-dialog".into())
            .gap_3()
            .on_key_down(|event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    window.close_dialog(cx);
                    cx.stop_propagation();
                }
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Reminder"),
                    )
                    .child(
                        div()
                            .debug_selector(|| "remind-confirm-text".into())
                            .child(Input::new(&self.text_input).small()),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Date"),
                    )
                    .child(
                        div()
                            .debug_selector(|| "remind-confirm-date".into())
                            .child(Input::new(&self.date_input).small()),
                    ),
            )
            .when_some(self.error.clone(), |col, error| {
                col.child(
                    div()
                        .debug_selector(|| "remind-confirm-error".into())
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Saved to {destination}")),
            )
    }
}

pub(super) fn open(
    request: selection::Request,
    reader: &mut Reader,
    window: &mut Window,
    cx: &mut Context<Reader>,
) {
    if !request.preflight(reader, window, cx) {
        return;
    }
    let (sentence, heading) = request.default_sentence_and_heading(reader);
    let weak_reader = cx.weak_entity();
    let dialog =
        cx.new(|cx| ConfirmDialog::new(weak_reader, request, sentence, heading, window, cx));
    let content = dialog.clone();
    let on_ok_dialog = dialog.clone();
    let submit_dialog = dialog.clone();
    window.open_dialog(cx, move |d, _, _| {
        let on_ok_dialog = on_ok_dialog.clone();
        let submit_dialog = submit_dialog.clone();
        d.title("Add reminder")
            .width(px(440.))
            .on_ok(move |_, window, cx| on_ok_dialog.update(cx, |d, cx| d.submit(window, cx)))
            .on_cancel(|_, _, _| true)
            .child(content.clone())
            .footer(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("remind-confirm-cancel")
                            .debug_selector(|| "remind-confirm-cancel".into())
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| {
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("remind-confirm-submit")
                            .debug_selector(|| "remind-confirm-submit".into())
                            .primary()
                            .label("Add reminder")
                            .on_click(move |_, window, cx| {
                                submit_dialog.update(cx, |d, cx| {
                                    d.submit(window, cx);
                                });
                            }),
                    ),
            )
    });
    window.defer(cx, move |window, cx| {
        dialog.update(cx, |d, cx| d.focus_first(window, cx));
    });
}
