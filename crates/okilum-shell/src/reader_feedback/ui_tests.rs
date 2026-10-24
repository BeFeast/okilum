use super::*;
use ::core::prelude::v1::test;
use gpui_component::WindowExt;

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
    visual.update(|window, cx| {
        let id = window.window_handle().window_id();
        cx.global::<Drafts>()
            .0
            .iter()
            .find_map(|(owner, draft)| {
                (*owner == id || draft.window.window_id() == id).then(|| draft.view.clone())
            })
            .expect("Feedback draft")
    })
}
fn feedback_context(reader: &mut VisualTestContext) -> VisualTestContext {
    let handle = reader
        .update(|window, cx| cx.global::<Drafts>().0[&window.window_handle().window_id()].window);
    VisualTestContext::from_window(handle.into(), &reader.cx)
}
fn open_from_button(reader: &mut VisualTestContext) -> (Entity<FeedbackView>, VisualTestContext) {
    let button = reader
        .debug_bounds("reader-feedback")
        .expect("always-visible header button");
    reader.simulate_click(button.center(), Modifiers::default());
    reader.run_until_parked();
    reader.update(|window, cx| {
        assert!(
            !window.has_active_dialog(cx),
            "Feedback never opens a modal in the main window"
        )
    });
    let mut visual = feedback_context(reader);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(visual.debug_bounds("feedback-form").is_some());
    assert!(
        visual.debug_bounds("feedback-public-notice").is_some(),
        "public visibility is disclosed in the actual form"
    );
    let view = get_draft(&mut visual);
    visual.update(|window, cx| {
        assert!(
            Focusable::focus_handle(view.read(cx).text.read(cx), cx).is_focused(window),
            "opening feedback focuses its text field for immediate paste"
        );
    });
    (view, visual)
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
fn feedback_window_pastes_text_and_image_and_remove_omits_the_attachment(cx: &mut TestAppContext) {
    with_reader(cx, |visual| {
        visual.simulate_resize(size(px(600.), px(900.)));
        visual.run_until_parked();
        let button = visual.debug_bounds("reader-feedback").unwrap();
        assert!(
            button.left() >= px(0.) && button.right() <= px(600.),
            "feedback stays visible in a narrow window"
        );
        let reader = visual;
        let (view, mut feedback) = open_from_button(reader);
        let visual = &mut feedback;
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
        let reader = visual;
        let (view, mut feedback) = open_from_button(reader);
        let visual = &mut feedback;
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
        let center = visual.debug_bounds("feedback-form").unwrap().center();
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
fn dropped_video_goes_to_the_video_slot_beside_the_screenshot_and_remove_omits_it(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("screenshot.png");
    std::fs::write(&image, png()).unwrap();
    let video = dir.path().join("recording.MOV");
    std::fs::write(&video, b"moov").unwrap();
    let second = dir.path().join("second.webm");
    std::fs::write(&second, b"webm").unwrap();
    with_reader(cx, |visual| {
        let reader = visual;
        let (view, mut feedback) = open_from_button(reader);
        let visual = &mut feedback;
        assert!(visual.debug_bounds("feedback-video-hint").is_some());
        let center = visual.debug_bounds("feedback-form").unwrap().center();
        visual.simulate_event(FileDropEvent::Entered {
            position: center,
            paths: ExternalPaths([video.clone(), image.clone()].into_iter().collect()),
        });
        visual.simulate_event(FileDropEvent::Submit { position: center });
        visual.run_until_parked();
        view.read_with(visual, |view, cx| {
            let report = view.report(cx).unwrap();
            let attached = report.video.expect("dropped video is attached");
            assert_eq!(attached.mime, "video/quicktime");
            assert_eq!(attached.path, video);
            assert!(
                report.screenshot.is_some(),
                "the PNG still goes to the screenshot"
            );
            assert!(view.video_error.is_none() && view.image_error.is_none());
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("feedback-video").is_some());

        // Two videos at once are refused and the attached one stays.
        view.update(visual, |view, cx| {
            view.attach_paths(&[second.clone(), video.clone()], cx)
        });
        view.read_with(visual, |view, _| {
            assert!(view.image_error.is_some());
            assert_eq!(view.video.as_ref().unwrap().path, video);
        });
        // A rejected replacement keeps the previous video.
        view.update(visual, |view, cx| {
            view.attach_video(&dir.path().join("missing.mp4"), cx)
        });
        view.read_with(visual, |view, _| {
            assert!(view.video_error.is_some());
            assert_eq!(view.video.as_ref().unwrap().path, video);
        });

        // The next good drop clears both stale reasons (review: 1630).
        view.update(visual, |view, cx| view.attach_paths(&[video.clone()], cx));
        view.read_with(visual, |view, _| {
            assert!(view.image_error.is_none() && view.video_error.is_none());
        });
        view.update(visual, |view, cx| {
            view.attach_video(&dir.path().join("missing.mp4"), cx)
        });
        view.update(visual, |view, cx| view.attach_paths(&[image.clone()], cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert!(
                view.video_error.is_none(),
                "screenshot drop clears the old video reason"
            );
            assert!(view.image.is_some() && view.video.is_some());
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));

        let remove = visual.debug_bounds("feedback-remove-video").unwrap();
        visual.simulate_click(remove.center(), Modifiers::default());
        visual.run_until_parked();
        view.read_with(visual, |view, cx| {
            assert!(view.report(cx).unwrap().video.is_none());
            assert!(view.video_error.is_none());
        });
    });
}

#[gpui::test]
fn failed_send_retains_draft_on_reopen_copy_and_retry_then_shows_the_issue_link(
    cx: &mut TestAppContext,
) {
    with_reader(cx, |visual| {
        let reader = visual;
        let (view, mut feedback) = open_from_button(reader);
        let mut visual = &mut feedback;
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
                "Escape closes the window without dropping the failed draft"
            )
        });
        reader.update(|window, _| window.activate_window());
        reader.dispatch_action(SendFeedback);
        reader.run_until_parked();
        feedback = feedback_context(reader);
        visual = &mut feedback;
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

#[gpui::test]
fn copy_report_keeps_text_and_metadata_when_the_opted_in_log_is_unavailable(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let valid = dir.path().join("valid.log");
    let invalid = dir.path().join("invalid.log");
    std::fs::write(&valid, "LOG-POSITIVE-CONTROL\n").unwrap();
    std::fs::write(&invalid, [0xff]).unwrap();
    with_reader(cx, |visual| {
        let reader = visual;
        let (view, mut feedback) = open_from_button(reader);
        let visual = &mut feedback;
        view.update_in(visual, |view, window, cx| {
            view.text.update(cx, |text, cx| {
                text.set_value("Keep this feedback draft", window, cx)
            });
            view.include_log = true;
            view.log_path = Some(valid.clone());
            view.sender = Arc::new(|_| panic!("Copy report must not send feedback"));
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let copy = visual.debug_bounds("feedback-copy").unwrap();
        visual.simulate_click(copy.center(), Modifiers::default());
        visual.update(|_, cx| {
            assert!(cx
                .read_from_clipboard()
                .unwrap()
                .text()
                .unwrap()
                .contains("LOG-POSITIVE-CONTROL"));
        });
        for path in [
            None,
            Some(dir.path().join("missing.log")),
            Some(invalid.clone()),
        ] {
            view.update(visual, |view, cx| {
                view.log_path = path.clone();
                cx.notify();
            });
            visual.update(|_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string("COPY-CANARY".into()))
            });
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let copy = visual.debug_bounds("feedback-copy").unwrap();
            visual.simulate_click(copy.center(), Modifiers::default());
            visual.update(|_, cx| {
                let copied = cx.read_from_clipboard().unwrap().text().unwrap();
                assert!(copied.contains("Keep this feedback draft"));
                assert!(copied.contains(&view.read(cx).meta.summary()));
                assert!(copied.contains("Diagnostic log could not be included"));
                assert!(!copied.contains("LOG-POSITIVE-CONTROL"));
            });
            view.read_with(visual, |view, cx| {
                assert_eq!(view.text.read(cx).value(), "Keep this feedback draft");
                assert!(view.include_log, "Copy must retain the attachment choice for Retry");
                assert_eq!(view.log_path, path);
                assert!(matches!(&view.state, SendState::Failed(message) if message.contains("Copied report without the diagnostic log")));
            });
        }
    });
}

#[test]
fn backing_scale_uses_the_actual_window_without_assuming_every_mac_is_retina() {
    assert_eq!(backing_scale(Some(2.), 1.), 2.);
    assert_eq!(backing_scale(Some(1.), 2.), 1.);
    for invalid in [
        None,
        Some(0.),
        Some(-1.),
        Some(f32::NAN),
        Some(f32::INFINITY),
    ] {
        assert_eq!(backing_scale(invalid, 1.5), 1.5);
    }
}

#[gpui::test]
fn feedback_keeps_the_main_window_interactive_and_metadata_belongs_to_its_source(
    cx: &mut TestAppContext,
) {
    with_reader(cx, |reader| {
        reader.update(|window, _| window.set_scale_factor(2.));
        let (view, mut feedback) = open_from_button(reader);
        let owner = reader.update(|window, _| window.window_handle().window_id());
        let feedback_id = feedback.update(|window, _| window.window_handle().window_id());
        assert_ne!(
            owner, feedback_id,
            "Feedback is an independent native window"
        );
        view.read_with(reader, |view, _| {
            assert_eq!(
                view.meta.scale, 2.,
                "originating window scale, not the Feedback window"
            );
            assert_eq!(
                (view.meta.window_width, view.meta.window_height),
                (1200., 900.)
            );
        });
        feedback.simulate_input("Draft: ");
        // Real main-window keyboard actions and text copying while Feedback stays open.
        reader.update(|window, _| window.activate_window());
        reader.simulate_keystrokes("secondary-f");
        reader.run_until_parked();
        reader.update(|window, cx| {
            assert!(!window.has_active_dialog(cx));
            let root = window.root::<Root>().flatten().unwrap();
            let entity = root.read(cx).view().clone().downcast::<Reader>().unwrap();
            assert!(
                entity.read(cx).find_open,
                "main-window Find received its shortcut"
            );
        });
        reader.simulate_input("COPY-FROM-MAIN");
        reader.simulate_keystrokes("secondary-a secondary-c");
        reader.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("COPY-FROM-MAIN")
            )
        });
        feedback.update(|window, cx| {
            window.activate_window();
            Focusable::focus_handle(view.read(cx).text.read(cx), cx).focus(window, cx);
        });
        feedback.simulate_keystrokes("secondary-v");
        view.read_with(&feedback, |view, cx| {
            assert_eq!(view.text.read(cx).value(), "Draft: COPY-FROM-MAIN")
        });
        // Help in Feedback must raise this window, not create another draft/window.
        let count = feedback.update(|_, cx| cx.windows().len());
        feedback.dispatch_action(SendFeedback);
        feedback.run_until_parked();
        feedback.update(|window, cx| {
            assert_eq!(cx.windows().len(), count);
            assert_eq!(window.window_handle().window_id(), feedback_id);
        });
        feedback.simulate_keystrokes("secondary-w");
        reader.run_until_parked();
        reader.update(|_, cx| {
            assert!(!cx
                .windows()
                .iter()
                .any(|window| window.window_id() == feedback_id))
        });
        reader.update(|window, _| window.activate_window());
        reader.dispatch_action(SendFeedback);
        reader.run_until_parked();
        feedback = feedback_context(reader);
        assert_eq!(get_draft(&mut feedback).entity_id(), view.entity_id());
        // The remounted field has working keyboard/focus subscriptions in its new window.
        feedback.simulate_keystrokes("secondary-v");
        view.read_with(&feedback, |view, cx| {
            assert_eq!(
                view.text.read(cx).value(),
                "Draft: COPY-FROM-MAINCOPY-FROM-MAIN"
            )
        });
    });
}

#[gpui::test]
fn one_click_sprint_send_reuses_the_non_modal_feedback_draft(cx: &mut TestAppContext) {
    with_reader(cx, |reader| {
        let (view, mut feedback) = open_from_button(reader);
        let original_window = feedback.window_handle();
        let sent = Arc::new(std::sync::Mutex::new(Vec::<Report>::new()));
        let calls = sent.clone();
        view.update_in(&mut feedback, |view, window, cx| {
            view.text
                .update(cx, |text, cx| text.set_value("Sprint result", window, cx));
            view.include_log = false; // The mock has no native report collector.
            view.sender = Arc::new(move |report| {
                calls.lock().unwrap().push(report);
                Ok(Receipt {
                    issue_number: 42,
                    issue_url: "https://git.oklabs.uk/BeFeast/okilum/issues/42".into(),
                })
            });
        });
        reader.update(|window, cx| show_mode(window, cx, true));
        reader.run_until_parked();
        feedback.run_until_parked();
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert_eq!(sent.lock().unwrap()[0].text, "Sprint result");
        assert_eq!(feedback_context(reader).window_handle(), original_window);
        view.read_with(&feedback, |view, _| {
            assert!(matches!(view.state, SendState::Sent(_)))
        });
        reader.update(|window, cx| assert!(!window.has_active_dialog(cx)));
    });
}

// Use the geometry actually painted by the input, including its scroll offset.
// Focus alone would miss a caret below the clipped viewport (#1574).
fn feedback_caret_in_view(view: &Entity<FeedbackView>, visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| {
        window.draw(cx).clear(cx);
        let text = view.read(cx).text.read(cx);
        assert!(
            text.focus_handle(cx).is_focused(window),
            "typing keeps focus"
        );
        assert_eq!(
            text.painted_cursor(),
            Some(text.cursor()),
            "painted caret follows the edit"
        );
        let (mut caret, _) = text
            .cursor_layout()
            .expect("laid-out caret positive control");
        caret.origin.y += text.scroll_offset().y;
        let bounds = text.input_bounds();
        assert!(
            caret.top() >= bounds.top() && caret.bottom() <= bounds.bottom(),
            "caret must be fully visible: {caret:?} in {bounds:?}, scroll {:?}",
            text.scroll_offset()
        );
    });
}

#[gpui::test]
fn feedback_textarea_grows_from_three_rows_then_scrolls_and_survives_remount(
    cx: &mut TestAppContext,
) {
    with_reader(cx, |reader| {
        let (view, mut feedback) = open_from_button(reader);
        feedback_caret_in_view(&view, &mut feedback);
        let (initial, line_height) = view.read_with(&feedback, |view, cx| {
            let text = view.text.read(cx);
            (
                text.input_bounds().size.height,
                text.cursor_layout().unwrap().1,
            )
        });
        assert!(
            initial >= 3. * line_height - px(1.),
            "three visible rows initially: {initial:?}"
        );
        feedback.simulate_input("one\ntwo\nthree\nfour\nfive\nsix");
        feedback_caret_in_view(&view, &mut feedback);
        let grown = view.read_with(&feedback, |view, cx| {
            view.text.read(cx).input_bounds().size.height
        });
        assert!(
            grown > initial + 2. * line_height,
            "field grows with multiline text"
        );
        feedback.simulate_input(&"\nnext".repeat(20));
        feedback_caret_in_view(&view, &mut feedback);
        let capped = view.read_with(&feedback, |view, cx| {
            let text = view.text.read(cx);
            assert!(
                text.scroll_offset().y < -line_height,
                "positive control: long draft actually scrolls"
            );
            text.input_bounds().size.height
        });
        assert!(
            capped <= 8. * line_height + px(1.),
            "sensible eight-row cap: {capped:?}"
        );
        feedback.simulate_input(&"\nmore".repeat(10));
        feedback_caret_in_view(&view, &mut feedback);
        assert_eq!(
            view.read_with(&feedback, |view, cx| view
                .text
                .read(cx)
                .input_bounds()
                .size
                .height),
            capped
        );
        // Reopening uses a fresh field and must preserve both its draft and sizing policy.
        let draft = view.read_with(&feedback, |view, cx| view.text.read(cx).value());
        feedback.simulate_keystrokes("secondary-w");
        reader.run_until_parked();
        reader.update(|window, _| window.activate_window());
        reader.dispatch_action(SendFeedback);
        reader.run_until_parked();
        feedback = feedback_context(reader);
        feedback_caret_in_view(&view, &mut feedback);
        view.read_with(&feedback, |view, cx| {
            let text = view.text.read(cx);
            assert_eq!(text.value(), draft);
            assert_eq!(text.input_bounds().size.height, capped);
        });
    });
}

#[gpui::test]
fn feedback_backspace_to_empty_keeps_caret_and_next_line_visible(cx: &mut TestAppContext) {
    with_reader(cx, |reader| {
        let (view, mut feedback) = open_from_button(reader);
        // Cover the tester's last-character deletion and a previously scrolled draft.
        for source in [
            "x".to_string(),
            "writing a feedback\nsecond row of feedback".into(),
            "line\n".repeat(12) + "last",
        ] {
            feedback.simulate_input(&source);
            feedback_caret_in_view(&view, &mut feedback);
            view.read_with(&feedback, |view, cx| {
                assert_eq!(view.text.read(cx).value(), source)
            });
            for remaining in (0..source.len()).rev() {
                feedback.simulate_keystrokes("backspace");
                feedback_caret_in_view(&view, &mut feedback);
                view.read_with(&feedback, |view, cx| {
                    let text = view.text.read(cx);
                    assert_eq!(text.value(), &source[..remaining]);
                    assert_eq!(text.cursor(), remaining);
                });
            }
            view.read_with(&feedback, |view, cx| {
                assert_eq!(
                    view.text.read(cx).scroll_offset().y,
                    px(0.),
                    "empty field returns to the first line"
                );
            });
            feedback.simulate_keystrokes("enter");
            feedback.simulate_input("visible next line");
            feedback_caret_in_view(&view, &mut feedback);
            view.read_with(&feedback, |view, cx| {
                assert_eq!(view.text.read(cx).value(), "\nvisible next line")
            });
            feedback.simulate_keystrokes("secondary-a backspace");
            feedback_caret_in_view(&view, &mut feedback);
        }
    });
}

#[test]
fn sprint_default_text_is_a_placeholder_and_the_empty_send_fallback() {
    // #1597: the Sprint line was the starting value with the caret before it.
    assert_eq!(placeholder(true), "Sprint test report");
    assert_ne!(placeholder(false), "Sprint test report");
    assert_eq!(report_text("", true).as_deref(), Some("Sprint test report"));
    assert_eq!(
        report_text("  \n", true).as_deref(),
        Some("Sprint test report")
    );
    assert_eq!(report_text("", false), None, "release Send stays disabled");
    assert_eq!(report_text("ываы", true).as_deref(), Some("ываы"));
    assert_eq!(report_text("Typed", false).as_deref(), Some("Typed"));
}

#[gpui::test]
fn feedback_field_starts_empty_and_typing_is_the_whole_value(cx: &mut TestAppContext) {
    with_reader(cx, |reader| {
        let (view, mut feedback) = open_from_button(reader);
        view.read_with(&feedback, |view, cx| {
            assert_eq!(view.text.read(cx).value(), "", "no prefilled text (#1597)");
            assert_eq!(
                view.text.read(cx).presentation().placeholder().as_ref(),
                placeholder(sprint::enabled())
            );
        });
        feedback.simulate_input("ываы");
        feedback.run_until_parked();
        view.read_with(&feedback, |view, cx| {
            assert_eq!(view.text.read(cx).value(), "ываы");
            assert_eq!(view.report(cx).unwrap().text, "ываы");
        });
    });
}
