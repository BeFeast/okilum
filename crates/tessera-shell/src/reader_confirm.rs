//! Confirmation for a risk-bearing action. `window.prompt` falls back to a GPUI
//! view that neither wraps its detail text nor closes on Escape, so the answer
//! comes from an app dialog instead. Escape, the close button and Cancel all
//! answer `false`; only the confirm button answers `true`.
use super::*;
use gpui_component::WindowExt;

/// What the dialog says: paragraphs before and after an optional list of names
/// (kept scrollable so a long list never pushes the buttons out of the window).
pub(super) struct Body {
    pub intro: Vec<String>,
    pub items: Vec<String>,
    pub outro: Vec<String>,
}

pub(super) fn confirm(
    window: &mut Window,
    cx: &mut App,
    title: &'static str,
    body: Body,
    confirm_label: &'static str,
) -> async_channel::Receiver<bool> {
    let (send, receive) = async_channel::bounded(1);
    window.open_dialog(cx, move |dialog, _, _| {
        let yes = send.clone();
        let no = send.clone();
        let close = send.clone();
        let footer_yes = send.clone();
        let footer_no = send.clone();
        dialog
            .title(title)
            .width(px(480.))
            .child(
                v_flex()
                    .gap_2()
                    .children(
                        body.intro
                            .iter()
                            .map(|text| div().text_sm().child(text.clone())),
                    )
                    .when(!body.items.is_empty(), |column| {
                        column.child(
                            v_flex()
                                .id("confirm-items")
                                .max_h(px(160.))
                                .overflow_y_scroll()
                                .children(
                                    body.items
                                        .iter()
                                        .map(|item| div().text_sm().child(item.clone())),
                                ),
                        )
                    })
                    .children(
                        body.outro
                            .iter()
                            .map(|text| div().text_sm().child(text.clone())),
                    ),
            )
            .on_ok(move |_, _, _| {
                let _ = yes.try_send(true);
                true
            })
            .on_cancel(move |_, _, _| {
                let _ = no.try_send(false);
                true
            })
            .on_close(move |_, _, _| {
                let _ = close.try_send(false);
            })
            .footer(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-confirm")
                            .debug_selector(|| "cancel-confirm".into())
                            .ghost()
                            .label("Cancel")
                            .on_click(move |_, window, cx| {
                                let _ = footer_no.try_send(false);
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("confirm-action")
                            .debug_selector(|| "confirm-action".into())
                            .primary()
                            .label(confirm_label)
                            .on_click(move |_, window, cx| {
                                let _ = footer_yes.try_send(true);
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
    receive
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn body() -> Body {
        Body {
            intro: vec!["Home/Plan.md → Work/Plan.md".into()],
            items: vec!["Notes/Index.md".into(), "Notes/Ref.md".into()],
            outro: vec!["Files edited since this operation will not be overwritten.".into()],
        }
    }

    #[gpui::test]
    fn escape_and_cancel_answer_false_and_confirm_answers_true(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            super::super::bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Start.md"), "# Start\n").unwrap();
        let root = root.canonicalize().unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Start.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        let ask = |visual: &mut VisualTestContext| {
            let answer = reader.update_in(visual, |_, window, cx| {
                confirm(window, cx, "Revert this link move?", body(), "Revert")
            });
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            answer
        };

        // Escape cancels, closes the dialog and answers false.
        let answer = ask(visual);
        assert!(visual.update(|window, cx| window.has_active_dialog(cx)));
        assert!(visual.debug_bounds("confirm-action").is_some());
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(!visual.update(|window, cx| window.has_active_dialog(cx)));
        assert_eq!(answer.try_recv(), Ok(false));

        // Cancel answers false.
        let answer = ask(visual);
        let cancel = visual
            .debug_bounds("cancel-confirm")
            .expect("cancel button");
        visual.simulate_click(cancel.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(!visual.update(|window, cx| window.has_active_dialog(cx)));
        assert_eq!(answer.try_recv(), Ok(false));

        // Positive control: the confirm button answers true.
        let answer = ask(visual);
        let confirm_button = visual
            .debug_bounds("confirm-action")
            .expect("confirm button");
        visual.simulate_click(confirm_button.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(!visual.update(|window, cx| window.has_active_dialog(cx)));
        assert_eq!(answer.try_recv(), Ok(true));
    }
}
