//! The provisional editor remains an interactive, stable presentation.
use super::view_tests::settle;
use super::*;
use ::core::prelude::v1::test;

fn held<'a>(
    cx: &'a mut TestAppContext,
    source: &str,
) -> (
    Entity<Reader>,
    Entity<CodePreview>,
    &'a mut VisualTestContext,
    tempfile::TempDir,
    futures::channel::oneshot::Sender<()>,
) {
    let (reader, visual, temp) = reader_viewer_chrome::tests::fixture_with_files(
        cx,
        "source.rs",
        &[("pending.json", source)],
    );
    let (release, gate) = futures::channel::oneshot::channel();
    let code = reader.update_in(visual, |reader, window, cx| {
        reader.preview_file("pending.json", window, cx);
        let code = reader.file_preview.as_ref().unwrap().code.clone().unwrap();
        code.update(cx, |code, _| code.preparation_gate = Some(gate));
        code
    });
    settle(visual);
    code.read_with(visual, |code, _| {
        assert!(
            code.input().is_some(),
            "bounded text before the held parser"
        );
        assert!(code.raw_document.is_none());
        assert_eq!(code.view, View::Raw);
    });
    (reader, code, visual, temp, release)
}

#[gpui::test]
fn code2809_initial_default_unmount_hands_programmatic_input_focus_to_code(
    cx: &mut TestAppContext,
) {
    for (source, expected) in [
        (
            format!("{{\"payload\":\"{}\"}}", "x".repeat(3_500_000)),
            View::Tree,
        ),
        ("[{\"value\":1},{\"value\":2}]".to_owned(), View::Table),
    ] {
        let (_reader, code, visual, _temp, release) = held(cx, &source);
        // Programmatic focus alone is not a user gesture; it must still have
        // a mounted dispatch target when the untouched default is published.
        code.update_in(visual, |code, window, cx| {
            code.input()
                .unwrap()
                .update(cx, |input, cx| input.focus(window, cx));
        });
        release.send(()).unwrap();
        settle(visual);
        code.update_in(visual, |code, window, _| {
            assert_eq!(code.view, expected);
            assert!(
                code.focus.is_focused(window),
                "default unmounted the focused text Input"
            );
        });
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "ctrl-4"
        } else {
            "alt-4"
        });
        settle(visual);
        code.read_with(visual, |code, _| {
            assert_eq!(code.view, View::Raw, "live Reader view shortcut")
        });
    }
}

#[gpui::test]
fn code2809_provisional_scroll_selection_find_and_choice_keep_raw(cx: &mut TestAppContext) {
    let multiline = format!("{{\n\"values\":[\n{}0]\n}}", "1,\n".repeat(2_000));
    for source in [
        format!("{{\"payload\":\"{}\"}}", "x".repeat(3_500_000)),
        format!("[\n{}{{\"value\":0}}]", "{\"value\":1},\n".repeat(2_000)),
        multiline,
    ] {
        for gesture in ["scroll", "selection", "find", "choice", "click"] {
            let (_reader, code, visual, _temp, release) = held(cx, &source);
            let input = code.read_with(visual, |code, _| code.input().unwrap());
            if gesture == "scroll" {
                let body = visual.debug_bounds("reader-code-file").unwrap();
                visual.simulate_event(ScrollWheelEvent {
                    position: point(body.center().x, body.bottom() - px(80.)),
                    delta: ScrollDelta::Pixels(if source.contains('\n') {
                        point(px(0.), px(-160.))
                    } else {
                        point(px(-160.), px(0.))
                    }),
                    ..Default::default()
                });
            } else if gesture == "click" {
                let body = visual.debug_bounds("reader-code-file").unwrap();
                visual.simulate_click(body.center(), Modifiers::default());
                input.update_in(visual, |input, window, cx| {
                    assert!(
                        input.focus_handle(cx).is_focused(window),
                        "real click focused provisional text"
                    )
                });
            } else if gesture == "selection" {
                input.update_in(visual, |input, window, cx| input.focus(window, cx));
                visual.simulate_keystrokes("right shift-right shift-right");
            } else if gesture == "find" {
                code.update_in(visual, |code, window, cx| code.open_find(window, cx));
            } else {
                visual.simulate_keystrokes(if cfg!(target_os = "macos") {
                    "ctrl-4"
                } else {
                    "alt-4"
                });
            }
            settle(visual);
            let before = input.read_with(visual, |input, _| {
                (input.selected_range(), input.scroll_offset())
            });
            if gesture == "scroll" {
                assert_ne!(before.1, point(px(0.), px(0.)), "actual provisional scroll");
            }
            if gesture == "selection" {
                assert!(!before.0.is_empty(), "actual provisional selection");
            }
            release.send(()).unwrap();
            settle(visual);
            code.read_with(visual, |code, cx| {
                assert!(
                    code.raw_document.is_some(),
                    "positive control: preparation completed"
                );
                assert_eq!(code.view, View::Raw, "{gesture} pins the provisional view");
                let input = input.read(cx);
                assert_eq!(
                    (input.selected_range(), input.scroll_offset()),
                    before,
                    "{gesture} retains the reading place"
                );
                assert!(
                    code.json.as_ref().unwrap().pretty.is_none(),
                    "no default format after interaction"
                );
            });
        }
    }
}

#[gpui::test]
fn code2809_interaction_during_default_format_keeps_the_provisional_place(cx: &mut TestAppContext) {
    let source = format!("{{\n\"values\":[\n{}0]\n}}", "1,\n".repeat(2_000));
    let (_reader, code, visual, _temp, parse) = held(cx, &source);
    let (release, gate) = futures::channel::oneshot::channel();
    code.update(visual, |code, _| code.format_gate = Some(gate));
    parse.send(()).unwrap();
    settle(visual);
    let input = code.read_with(visual, |code, _| {
        assert!(code.json.as_ref().unwrap().formatting);
        code.input().unwrap()
    });
    input.update_in(visual, |input, window, cx| input.focus(window, cx));
    visual.simulate_keystrokes("right shift-right");
    settle(visual);
    let before = input.read_with(visual, |input, _| {
        (input.selected_range(), input.scroll_offset())
    });
    release.send(()).unwrap();
    settle(visual);
    code.read_with(visual, |code, cx| {
        assert!(
            code.snapshot_for(View::Formatted).is_some(),
            "formatter really completed"
        );
        assert_eq!(code.view, View::Raw);
        let input = input.read(cx);
        assert_eq!((input.selected_range(), input.scroll_offset()), before);
    });
}

#[gpui::test]
fn code2809_real_park_draft_resumes_the_retained_header(cx: &mut TestAppContext) {
    let source = "{\n\"value\":1\n}";
    let (reader, visual, temp) =
        reader_viewer_chrome::tests::fixture_with_files(cx, "data.json", &[("data.json", source)]);
    settle(visual);
    let code = reader.read_with(visual, |reader, _| {
        reader.file_preview.as_ref().unwrap().code.clone().unwrap()
    });
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx);
        let input = reader.editing.as_ref().unwrap().input().clone();
        input.update(cx, |input, cx| {
            input.replace_source_range(0..source.len(), "{\"local\":2}", window, cx)
        });
        std::fs::write(temp.path().join("vault/data.json"), "{\"incoming\":3}").unwrap();
        assert!(!reader.save_source(cx), "real disk conflict");
    });
    settle(visual);
    let close = visual
        .debug_bounds("source-park")
        .expect("real Close, keeping draft action");
    visual.simulate_click(close.center(), Modifiers::default());
    settle(visual);
    reader.read_with(visual, |reader, _| {
        assert!(reader.editing.is_none());
        assert_eq!(
            reader
                .file_preview
                .as_ref()
                .unwrap()
                .code
                .as_ref()
                .unwrap()
                .entity_id(),
            code.entity_id(),
            "same retained preview"
        );
    });
    // Geometry helper replays cached header selectors; it cannot change
    // read_active. The missing-resume compiled control must still fail here.
    let tree = reader_viewer_chrome::tests::measured_bounds(visual, "code-view-tree").unwrap();
    visual.simulate_click(tree.center(), Modifiers::default());
    settle(visual);
    code.read_with(visual, |code, _| {
        assert_eq!(
            code.view,
            View::Tree,
            "park exit resumes real header callbacks"
        )
    });
    assert_eq!(
        std::fs::read_to_string(temp.path().join("vault/data.json")).unwrap(),
        "{\"incoming\":3}",
        "park never overwrites incoming data"
    );
}

#[gpui::test]
fn code2809_explicit_table_choice_keeps_a_mounted_focus_target(cx: &mut TestAppContext) {
    let (code, visual, _temp) = super::view_tests::fixture(cx, "array.json", "[{\"value\":1}]");
    code.update_in(visual, |code, window, cx| {
        code.set_view(View::Tree, window, cx);
        code.open_find(window, cx);
        assert!(code.find.focus_handle(cx).is_focused(window));
    });
    settle(visual);
    assert!(visual.debug_bounds("viewer-find-field").is_some());
    code.update_in(visual, |code, window, cx| {
        code.set_view(View::Table, window, cx);
        assert_eq!(code.view, View::Table);
        assert!(
            code.focus.is_focused(window),
            "explicit removal hands Find focus to a mounted target"
        );
    });
    settle(visual);
    assert!(visual.debug_bounds("delimited-viewport").is_some());
    assert!(
        visual.debug_bounds("viewer-find-field").is_some(),
        "Table keeps shared Find mounted"
    );
}

#[gpui::test]
fn code2810_provisional_find_names_its_scope_and_keeps_the_pending_query(cx: &mut TestAppContext) {
    let source = format!("{{\"payload\":\"{} NEEDLE\"}}", "x".repeat(100_000));
    let (reader, code, visual, _temp, release) = held(cx, &source);
    code.read_with(visual, |code, cx| {
        assert_eq!(
            code.find.read(cx).presentation().placeholder().as_ref(),
            "Find in whole file"
        )
    });
    reader.update_in(visual, |r, w, cx| r.open_find(w, cx));
    settle(visual);
    visual.simulate_input("NEEDLE");
    settle(visual);
    code.read_with(visual, |state, cx| {
        assert_eq!(
            state.find_target(code.clone(), cx).count.unwrap().as_ref(),
            "Searching…",
            "an unfinished whole-file index cannot report No matches"
        );
    });
    release.send(()).unwrap();
    settle(visual);
    code.read_with(visual, |code, cx| {
        assert_eq!(
            code.view,
            View::Raw,
            "Find is interaction with provisional text"
        );
        assert_eq!(code.find.read(cx).value().as_ref(), "NEEDLE");
        assert_eq!(
            code.matches.len(),
            1,
            "worker indexes the pending whole-file query"
        );
        assert!(code.page > 0);
    });
}
