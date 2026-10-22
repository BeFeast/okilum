use super::*;
use ::core::prelude::v1::test;

fn with_reader(cx: &mut TestAppContext, check: impl FnOnce(&mut VisualTestContext)) {
    cx.update(|cx| {
        gpui_component::init(cx);
        version_info::install(cx);
        bind_keys(cx);
        cx.set_reduce_motion(true);
    });
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    std::fs::create_dir(&vault).unwrap();
    std::fs::write(vault.join("start.md"), "# Feedback\nA normal note.\n").unwrap();
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault),
                    note: Some("start.md".into()),
                    session_directory: Some(dir.path().join("state")),
                    index_dir: Some(dir.path().join("index")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        Root::new(reader, window, cx)
    });
    visual.simulate_resize(size(px(1200.), px(900.)));
    visual.run_until_parked();
    visual.update(|window, _| window.activate_window());
    check(visual);
}
fn get_draft(visual: &mut VisualTestContext) -> Entity<FeedbackView> {
    visual.update(|window, cx| cx.global::<Drafts>().0[&window.window_handle().window_id()].clone())
}
fn open_from_button(visual: &mut VisualTestContext) -> Entity<FeedbackView> {
    let button = visual
        .debug_bounds("reader-feedback")
        .expect("always-visible header button");
    visual.simulate_click(button.center(), Modifiers::default());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("feedback-dialog").is_some());
    let view = get_draft(visual);
    visual.update(|window, cx| {
        assert!(
            Focusable::focus_handle(view.read(cx).text.read(cx), cx).is_focused(window),
            "opening feedback focuses its text field for immediate paste"
        )
    });
    view
}
fn png() -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        12,
        8,
        image::Rgb([230, 40, 20]),
    ))
    .write_to(&mut bytes, image::ImageFormat::Png)
    .unwrap();
    bytes.into_inner()
}

#[gpui::test]
fn header_dialog_pastes_text_and_image_and_remove_omits_the_attachment(cx: &mut TestAppContext) {
    with_reader(cx, |visual| {
        visual.simulate_resize(size(px(600.), px(900.)));
        visual.run_until_parked();
        let button = visual.debug_bounds("reader-feedback").unwrap();
        assert!(
            button.left() >= px(0.) && button.right() <= px(600.),
            "feedback stays visible in a narrow window"
        );
        let view = open_from_button(visual);
        visual
            .update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("Report text".into())));
        visual.simulate_keystrokes("secondary-v");
        visual.run_until_parked();
        view.read_with(visual, |view, cx| {
            assert_eq!(view.text.read(cx).value(), "Report text");
            assert!(!view.include_log);
            assert!(view.image.is_none());
        });
        visual.update(|_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_image(&gpui::Image {
                format: gpui::ImageFormat::Png,
                bytes: png(),
                id: 1,
            }))
        });
        visual.simulate_keystrokes("secondary-v");
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("feedback-preview").is_some(),
            "positive control: real image preview"
        );
        view.read_with(visual, |view, cx| {
            assert_eq!(view.text.read(cx).value(), "Report text");
            assert!(view.report(cx).unwrap().screenshot.is_some());
            assert!(view.report(cx).unwrap().log.is_none());
        });
        let remove = visual.debug_bounds("feedback-remove").unwrap();
        visual.simulate_click(remove.center(), Modifiers::default());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("feedback-preview").is_none());
        view.read_with(visual, |view, cx| {
            assert!(view.report(cx).unwrap().screenshot.is_none())
        });
    });
}

#[gpui::test]
fn picker_and_native_file_drop_attach_images_and_invalid_replacement_keeps_the_preview(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("screenshot.png");
    std::fs::write(&path, png()).unwrap();
    with_reader(cx, |visual| {
        let view = open_from_button(visual);
        let attach = visual.debug_bounds("feedback-attach").unwrap();
        visual.simulate_click(attach.center(), Modifiers::default());
        visual.run_until_parked();
        visual.simulate_path_prompt_response(|options| {
            assert!(options.files && !options.directories && !options.multiple);
            Some(vec![path.clone()])
        });
        visual.run_until_parked();
        assert!(
            view.read_with(visual, |view, _| view.image.is_some()),
            "picker positive control"
        );
        view.update(visual, |view, cx| view.remove_image(cx));
        visual.run_until_parked();
        let center = visual.debug_bounds("feedback-dialog").unwrap().center();
        visual.simulate_event(FileDropEvent::Entered {
            position: center,
            paths: ExternalPaths([path.clone()].into_iter().collect()),
        });
        visual.simulate_event(FileDropEvent::Submit { position: center });
        visual.run_until_parked();
        assert!(
            view.read_with(visual, |view, _| view.image.is_some()),
            "native drop positive control"
        );
        view.update(visual, |view, cx| {
            view.image_input(ImageInput::Clipboard(b"invalid".to_vec()), cx)
        });
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert!(view.image_error.is_some());
            assert!(view.image.is_some());
            assert!(!view.attaching);
        });
    });
}

#[gpui::test]
fn failed_send_retains_draft_on_reopen_copy_and_retry_then_shows_the_issue_link(
    cx: &mut TestAppContext,
) {
    with_reader(cx, |visual| {
        let view = open_from_button(visual);
        let sent = Arc::new(std::sync::Mutex::new(Vec::<Report>::new()));
        let calls = sent.clone();
        view.update_in(visual, |view, window, cx| {
            view.text
                .update(cx, |text, cx| text.set_value("Keep my draft", window, cx));
            view.log_path = None; // Off means no log access is needed to send.
            view.sender = Arc::new(move |report| {
                calls.lock().unwrap().push(report);
                anyhow::bail!("Network unavailable")
            });
            view.send(cx);
            view.send(cx); // Double click must not post a second request.
        });
        visual.run_until_parked();
        view.read_with(visual, |view, cx| {
            assert!(matches!(view.state, SendState::Failed(_)));
            assert_eq!(view.text.read(cx).value(), "Keep my draft");
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert!(
                !view.visible,
                "Escape closes the dialog without dropping the failed draft"
            )
        });
        visual.dispatch_action(SendFeedback);
        visual.run_until_parked();
        assert_eq!(
            get_draft(visual).entity_id(),
            view.entity_id(),
            "same failed draft"
        );
        view.update(visual, |view, cx| view.copy(cx));
        visual.update(|_, cx| {
            assert!(cx
                .read_from_clipboard()
                .unwrap()
                .text()
                .unwrap()
                .contains("Keep my draft"))
        });
        let calls = sent.clone();
        view.update(visual, |view, cx| {
            view.sender = Arc::new(move |report| {
                calls.lock().unwrap().push(report);
                Ok(Receipt {
                    issue_number: 123,
                    issue_url: "https://git.oklabs.uk/BeFeast/okilum/issues/123".into(),
                })
            });
            view.send(cx);
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("feedback-issue").is_some());
        let calls = sent.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].copy_text(), calls[1].copy_text());
        assert!(calls[0].log.is_none());
    });
}
