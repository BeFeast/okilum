//! Regressions from the independent review of the complete CodePreview header.
use super::view_tests::{fixture, settle};
use super::*;
use ::core::prelude::v1::test;

#[gpui::test]
fn code2809_navigation_never_changes_view_memory_even_after_restart(cx: &mut TestAppContext) {
    let state = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_ui_state::install(state.path(), cx);
        reader_ui_state::set_code_view("json", View::Formatted, cx);
    });
    {
        let (code, visual, _temp) = fixture(cx, "broken.json", "{ invalid: true }");
        let badge = visual
            .debug_bounds("code-invalid")
            .expect("real warning badge");
        visual.simulate_click(badge.center(), Modifiers::default());
        settle(visual);
        code.update_in(visual, |code, window, cx| {
            assert!(
                code.input().unwrap().read(cx).cursor() > 0,
                "error navigation ran"
            );
            let _ = window;
        });
        let status = code.read_with(visual, |preview, cx| preview.status(code.clone(), cx));
        visual.update(|window, cx| (status.actions[0].invoke)(window, cx));
        // Dispatch the action too, rather than invoking only the helper.
        visual.dispatch_action(GoToError);
        visual.update(|_, cx| {
            assert_eq!(
                reader_ui_state::code_view("json", cx),
                Some(View::Formatted)
            );
            reader_ui_state::flush(cx);
            reader_ui_state::install(state.path(), cx);
            assert_eq!(
                reader_ui_state::code_view("json", cx),
                Some(View::Formatted)
            );
        });
    }
    let (code, visual, _temp) = fixture(cx, "valid.json", "{\"outer\":{\"value\":1}}");
    code.update_in(visual, |code, window, cx| {
        assert_eq!(
            code.view,
            View::Formatted,
            "valid JSON retains its explicit default"
        );
        code.set_view(View::Tree, window, cx);
    });
    settle(visual);
    let source = visual
        .debug_bounds("data-node-source-0")
        .expect("real node navigation");
    visual.simulate_click(source.center(), Modifiers::default());
    settle(visual);
    code.read_with(visual, |code, cx| {
        assert_eq!(
            code.view,
            View::Raw,
            "row navigation really switched to Raw"
        );
        assert_eq!(reader_ui_state::code_view("json", cx), Some(View::Tree));
    });
    visual.update(|_, cx| {
        reader_ui_state::flush(cx);
        reader_ui_state::install(state.path(), cx);
        assert_eq!(reader_ui_state::code_view("json", cx), Some(View::Tree));
    });
}

#[gpui::test]
fn code2809_first_text_precedes_full_parse_and_format_on_three_point_five_mb(
    cx: &mut TestAppContext,
) {
    use std::sync::atomic::Ordering;
    for (index, text) in [
        format!(
            "{{\"code2809_ordering_large\":\"{}\"}}",
            "x".repeat(3_500_000)
        ),
        format!(
            "{{\n\"code2809_ordering_small\":[\n{}0]}}",
            "1,\n".repeat(50_000)
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.json");
        std::fs::write(&path, &text).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_ui_state::install(&temp.path().join("state"), cx);
            // Exercise the old single-page eager-format branch without a
            // default Formatted request obscuring whether it was lazy.
            reader_ui_state::set_code_view("json", View::Raw, cx);
        });
        document::ORDERING_PARSES[index].store(0, Ordering::SeqCst);
        let (release, gate) = futures::channel::oneshot::channel();
        let mut code = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view =
                    CodePreview::new(temp.path().into(), "large.json".into(), path, window, cx);
                view.preparation_gate = Some(gate);
                view
            });
            code = Some(view.clone());
            Root::new(cx.new(|_| Surface(view)), window, cx)
        });
        visual.run_until_parked();
        let code = code.unwrap();
        code.read_with(visual, |code, cx| {
            assert_eq!(
                code.input()
                    .expect("text before parser")
                    .read(cx)
                    .value()
                    .as_ref(),
                &text[document::first_page(&text)]
            );
            assert_eq!(
                document::ORDERING_PARSES[index].load(Ordering::SeqCst),
                0,
                "the actual full parser must not run before first text"
            );
            assert!(code.document.is_none() && code.raw_document.is_none() && code.json.is_none());
            assert!(code.first_content_ms.unwrap() < 1_000);
        });
        assert!(visual.debug_bounds("reader-code-file").is_some());
        release.send(()).unwrap();
        settle(visual);
        code.read_with(visual, |code, _| {
            assert_eq!(code.view, if index == 0 { View::Tree } else { View::Raw });
            assert!(
                code.document.is_some(),
                "positive control: parser completed"
            );
            assert_eq!(
                document::ORDERING_PARSES[index].load(Ordering::SeqCst),
                1,
                "one raw Document, no eager formatted Document"
            );
            assert!(code.json.as_ref().unwrap().pretty.is_none());
        });
    }
}

#[gpui::test]
fn code2809_header_caches_node_lookup_until_caret_or_snapshot_changes(cx: &mut TestAppContext) {
    use std::sync::atomic::Ordering;
    let source = format!("{{\"items\":[{}0]}}", "1,".repeat(40_000));
    let (code, visual, _temp) = fixture(cx, "large.json", &source);
    code.update_in(visual, |code, window, cx| {
        code.set_view(View::Raw, window, cx);
        code.show_page(1, window, cx);
        let input = code.input().unwrap();
        input.update(cx, |input, cx| input.set_selected_range(5..5, cx));
        let doc = code.document.clone().unwrap();
        doc.node_lookups.store(0, Ordering::Relaxed);
        for _ in 0..20 {
            code.chrome(cx.entity(), cx);
        }
        assert_eq!(
            doc.node_lookups.load(Ordering::Relaxed),
            1,
            "one lookup for one caret, regardless of Reader renders"
        );
        input.update(cx, |input, cx| input.set_selected_range(10..10, cx));
        for _ in 0..20 {
            code.chrome(cx.entity(), cx);
        }
        assert_eq!(
            doc.node_lookups.load(Ordering::Relaxed),
            2,
            "new caret recomputes the actual range"
        );
        code.show_page(2, window, cx);
        code.chrome(cx.entity(), cx);
        assert_eq!(
            doc.node_lookups.load(Ordering::Relaxed),
            3,
            "paging invalidates the cached node"
        );
    });
}

#[gpui::test]
fn code2809_async_format_keeps_focus_and_respects_a_new_view_choice(cx: &mut TestAppContext) {
    for (switch_away, formatted_paged) in
        [(false, true), (true, true), (false, false), (true, false)]
    {
        let source = if formatted_paged {
            format!("{{\"payload\":\"{}\"}}", "x".repeat(120_000))
        } else {
            format!("{{\"payload\":[{}0]}}", "0,".repeat(40_000))
        };
        let (code, visual, _temp) = fixture(cx, "data.json", &source);
        let (release, gate) = futures::channel::oneshot::channel();
        code.update_in(visual, |code, window, cx| {
            code.format_gate = Some(gate);
            code.set_view(View::Formatted, window, cx);
            assert!(code.json.as_ref().unwrap().formatting);
            if switch_away {
                code.set_view(View::Raw, window, cx);
            }
            code.find.update(cx, |input, cx| input.focus(window, cx));
        });
        release.send(()).unwrap();
        settle(visual);
        code.update_in(visual, |code, window, cx| {
            assert_eq!(
                code.view,
                if switch_away {
                    View::Raw
                } else {
                    View::Formatted
                }
            );
            assert!(
                code.find.focus_handle(cx).is_focused(window),
                "worker completion preserves Find focus"
            );
            assert!(!code.json.as_ref().unwrap().formatting);
            assert_eq!(
                code.snapshot_for(View::Formatted).unwrap().pages.len() > 1,
                formatted_paged
            );
        });
        assert!(
            visual.debug_bounds("data-whole-file-find").is_some(),
            "the focused Find is still mounted after completion"
        );
        visual.simulate_input("payload");
        settle(visual);
        code.read_with(visual, |code, cx| {
            assert_eq!(code.find.read(cx).value().as_ref(), "payload");
            assert_eq!(code.matches.len(), 1, "the mounted Find remains usable");
        });
    }
}

#[gpui::test]
fn code2809_async_format_cannot_change_the_hidden_preview_after_real_edit_entry(
    cx: &mut TestAppContext,
) {
    let source = format!("{{\"payload\":\"{}\"}}", "x".repeat(120_000));
    let (reader, visual, _temp) =
        reader_viewer_chrome::tests::fixture_with_files(cx, "data.json", &[("data.json", &source)]);
    settle(visual);
    let code = reader.read_with(visual, |reader, _| {
        reader.file_preview.as_ref().unwrap().code.clone().unwrap()
    });
    let (release, gate) = futures::channel::oneshot::channel();
    code.update_in(visual, |code, window, cx| {
        code.format_gate = Some(gate);
        code.set_view(View::Formatted, window, cx);
        assert_eq!(code.view, View::Tree);
    });
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    settle(visual);
    reader.update_in(visual, |reader, window, cx| {
        assert!(reader
            .editing
            .as_ref()
            .unwrap()
            .input()
            .focus_handle(cx)
            .is_focused(window));
    });
    release.send(()).unwrap();
    settle(visual);
    code.read_with(visual, |code, _| {
        assert_eq!(code.view, View::Tree, "pending view is not applied in Edit")
    });
    reader.update_in(visual, |reader, window, cx| {
        assert!(
            reader
                .editing
                .as_ref()
                .unwrap()
                .input()
                .focus_handle(cx)
                .is_focused(window),
            "source editor keeps native focus"
        );
    });
}

#[gpui::test]
fn code2809_part_keys_and_repeated_slider_values_do_not_rebuild_text(cx: &mut TestAppContext) {
    for source in [
        "fn main() {\n  let value = 1;\n}\n".to_owned(),
        format!("{{\"payload\":\"{}\"}}", "x".repeat(120_000)),
    ] {
        let name = if source.starts_with("fn") {
            "source.rs"
        } else {
            "data.json"
        };
        let (code, visual, _temp) = fixture(cx, name, &source);
        code.update_in(visual, |code, window, cx| {
            code.set_view(View::Raw, window, cx);
            code.input()
                .unwrap()
                .update(cx, |input, cx| input.set_selected_range(4..8, cx));
            code.focus.focus(window, cx);
        });
        visual.simulate_keystrokes("alt-pageup");
        if name.ends_with("rs") {
            visual.simulate_keystrokes("alt-pagedown");
        }
        settle(visual);
        code.read_with(visual, |code, cx| {
            assert_eq!(
                code.input().unwrap().read(cx).selected_range(),
                4..8,
                "a boundary/nonpaged part command leaves the buffer untouched"
            )
        });
        if name.ends_with("json") {
            let slider = code.read_with(visual, |code, _| code.scrubber.clone());
            slider.update(visual, |_, cx| {
                cx.emit(gpui_component::slider::SliderEvent::Change(0.0.into()))
            });
            settle(visual);
            visual.update(|window, cx| window.simulate_next_frame(cx));
            settle(visual);
            code.read_with(visual, |code, cx| {
                assert_eq!(
                    code.input().unwrap().read(cx).selected_range(),
                    4..8,
                    "the same slider part leaves caret and selection untouched"
                )
            });
            visual.simulate_keystrokes("alt-pagedown");
            settle(visual);
            code.read_with(visual, |code, _| {
                assert_eq!(code.page, 1, "positive control: a real next part works")
            });
            code.update_in(visual, |code, window, cx| {
                code.set_view(View::Tree, window, cx)
            });
            visual.simulate_keystrokes("alt-pagedown");
            settle(visual);
            code.read_with(visual, |code, _| {
                assert_eq!(code.view, View::Tree);
                assert_eq!(code.page, 1, "part keys never rewrite Tree's hidden editor");
            });
        }
    }
}

#[gpui::test]
fn code2809_error_tolerant_source_folds_use_distinct_header_glyphs(cx: &mut TestAppContext) {
    toolbar::take_built_fold_icons();
    let source = "fn main() {\n let broken = ;\n if true {\n   call();\n }\n}\n";
    let (code, visual, _temp) = fixture(cx, "source.rs", source);
    code.update(visual, |code, cx| {
        let doc = code.document.as_ref().unwrap();
        assert!(
            !doc.parses && doc.foldable,
            "positive control: a locally malformed grammar still has folds"
        );
        assert_eq!(code.fold_availability(), Availability::Available);
        code.fold_all(true, cx);
        assert!(!code.input().unwrap().read(cx).folded_ranges().is_empty());
        code.fold_all(false, cx);
        assert!(code.input().unwrap().read(cx).folded_ranges().is_empty());
    });
    assert!(visual.debug_bounds("code-fold-all").is_some());
    let icons = toolbar::take_built_fold_icons();
    assert!(
        !icons.is_empty(),
        "the actual chrome constructor used the fold icon builder"
    );
    assert!(icons.contains(&brand::READER_COLLAPSE_ICON));
    assert!(icons.contains(&brand::READER_EXPAND_ICON));
    assert!(
        icons
            .iter()
            .all(|path| *path != brand::NOTE_WIDTH_NORMAL_ICON
                && *path != brand::NOTE_WIDTH_FULL_ICON)
    );
}

#[gpui::test]
fn code2809_header_mouse_and_view_keys_keep_live_reader_focus(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = reader_viewer_chrome::tests::fixture(cx, "data.json");
    visual.simulate_resize(size(px(1600.), px(900.)));
    settle(visual);
    let code = reader.read_with(visual, |reader, _| {
        reader.file_preview.as_ref().unwrap().code.clone().unwrap()
    });
    reader.update_in(visual, |reader, window, cx| {
        reader.focus_handle.focus(window, cx)
    });
    let raw = reader_viewer_chrome::tests::measured_bounds(visual, "code-view-raw")
        .expect("actual Raw segment");
    visual.simulate_click(raw.center(), Modifiers::default());
    settle(visual);
    reader.update_in(visual, |reader, window, cx| {
        assert!(
            reader.focus_handle.is_focused(window),
            "Raw click preserves Reader commands gated on !Input"
        );
        assert!(!code
            .read(cx)
            .input()
            .unwrap()
            .focus_handle(cx)
            .is_focused(window));
        assert_eq!(code.read(cx).view, View::Raw);
    });
    #[cfg(target_os = "macos")]
    visual.simulate_keystrokes("ctrl-3");
    #[cfg(not(target_os = "macos"))]
    visual.simulate_keystrokes("alt-3");
    settle(visual);
    reader.update_in(visual, |reader, window, cx| {
        assert_eq!(
            code.read(cx).view,
            View::Formatted,
            "real Reader action listener ran"
        );
        assert!(reader.focus_handle.is_focused(window));
    });
    let tree = reader_viewer_chrome::tests::measured_bounds(visual, "code-view-tree")
        .expect("Tree is actually available");
    visual.simulate_click(tree.center(), Modifiers::default());
    settle(visual);
    code.update_in(visual, |code, window, cx| {
        assert_eq!(code.view, View::Tree);
        assert!(
            code.focus.is_focused(window),
            "Tree has a mounted non-Input keyboard target"
        );
        assert_eq!(code.availability(View::Tree), Availability::Available);
        cx.bind_keys([KeyBinding::new("f9", FindInNote, Some("ReaderCode"))]);
    });
    visual.simulate_keystrokes("f9");
    settle(visual);
    code.update_in(visual, |code, window, cx| {
        assert!(code.find.focus_handle(cx).is_focused(window))
    });
}

#[gpui::test]
fn code2809_table_switch_does_not_focus_an_unmounted_text_input(cx: &mut TestAppContext) {
    let (reader, visual, _temp) = reader_viewer_chrome::tests::fixture_with_files(
        cx,
        "array.json",
        &[("array.json", "[{\"value\":1}]")],
    );
    visual.simulate_resize(size(px(1600.), px(900.)));
    settle(visual);
    let code = reader.read_with(visual, |reader, _| {
        reader.file_preview.as_ref().unwrap().code.clone().unwrap()
    });
    reader.update_in(visual, |reader, window, cx| {
        reader.focus_handle.focus(window, cx)
    });
    for selector in ["code-view-raw", "code-view-table"] {
        let bounds = reader_viewer_chrome::tests::measured_bounds(visual, selector).unwrap();
        visual.simulate_click(bounds.center(), Modifiers::default());
        settle(visual);
        reader.update_in(visual, |reader, window, cx| {
            assert!(
                reader.focus_handle.is_focused(window),
                "view switch preserves Reader focus"
            );
            assert!(!code
                .read(cx)
                .input()
                .unwrap()
                .focus_handle(cx)
                .is_focused(window));
        });
    }
    assert!(reader_viewer_chrome::tests::measured_bounds(visual, "delimited-viewport").is_some());
}

#[cfg(unix)]
#[gpui::test]
fn code2809_real_edit_admission_distinguishes_vault_from_recovery_permissions(
    cx: &mut TestAppContext,
) {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let (reader, visual, temp) = reader_viewer_chrome::tests::fixture(cx, "data.json");
    settle(visual);
    let vault = temp.path().join("vault");
    let original = std::fs::read(vault.join("data.json")).unwrap();
    let code = reader.read_with(visual, |reader, _| {
        reader.file_preview.as_ref().unwrap().code.clone().unwrap()
    });
    assert!(code.read_with(visual, |preview, cx| preview.editable()
        && preview.chrome(code.clone(), cx).state.is_none()));
    let _restore_vault = Restore(vault.clone());
    std::fs::set_permissions(&vault, std::fs::Permissions::from_mode(0o500)).unwrap();
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    settle(visual);
    reader.read_with(visual, |reader, _| {
        assert!(
            reader.editing.is_none(),
            "real Edit is refused by unwritable vault directory"
        )
    });
    code.read_with(visual, |preview, cx| {
        assert!(!preview.editable());
        assert_eq!(
            preview
                .chrome(code.clone(), cx)
                .state
                .unwrap()
                .tooltip
                .as_ref(),
            "Read-only: this folder cannot be written"
        );
    });
    std::fs::set_permissions(&vault, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(std::fs::read(vault.join("data.json")).unwrap(), original);
    // Reopening discards the failed attempt's location-specific capability.
    reader.update_in(visual, |reader, window, cx| {
        reader.preview_file("data.json", window, cx)
    });
    settle(visual);
    let code = reader.read_with(visual, |reader, _| {
        reader.file_preview.as_ref().unwrap().code.clone().unwrap()
    });
    let fresh_recovery = temp.path().join("fresh-recovery");
    reader.update(visual, |reader, _| {
        reader.session_directory = Some(fresh_recovery)
    });
    let recovery = reader.read_with(visual, |reader, _| {
        reader
            .session_directory
            .as_ref()
            .unwrap()
            .join("editor-drafts")
    });
    std::fs::create_dir_all(&recovery).unwrap();
    assert_eq!(
        std::fs::read_dir(&recovery).unwrap().count(),
        0,
        "the denied recovery fixture has no pre-existing writable lock file"
    );
    let _restore_recovery = Restore(recovery.clone());
    std::fs::set_permissions(&recovery, std::fs::Permissions::from_mode(0o500)).unwrap();
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    settle(visual);
    reader.read_with(visual, |reader, _| {
        assert!(reader.editing.is_none(), "private recovery failure is real")
    });
    code.read_with(visual, |preview, cx| {
        assert!(
            preview.editable(),
            "private recovery failure does not poison document capability"
        );
        assert!(preview.chrome(code.clone(), cx).state.is_none());
    });
    std::fs::set_permissions(&recovery, std::fs::Permissions::from_mode(0o700)).unwrap();
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    settle(visual);
    reader.read_with(visual, |reader, _| {
        assert!(
            reader.editing.is_some(),
            "positive control: repaired private recovery permits Edit"
        )
    });
    assert_eq!(std::fs::read(vault.join("data.json")).unwrap(), original);
}

#[gpui::test]
fn code2809_large_non_json_status_never_claims_a_formatted_alternative(cx: &mut TestAppContext) {
    let (code, visual, _temp) = fixture(cx, "text.txt", "small\n");
    code.update(visual, |preview, cx| {
        let source = "line\n".repeat(1_800_000);
        preview.source_bytes = Some(source.len());
        preview.raw_document = Some(Arc::new(Document::for_file(source, "text", false)));
        let status = preview.status(code.clone(), cx);
        assert_eq!(status.label.as_ref(), "Read-only.");
        assert_eq!(
            status.text.as_ref(),
            "9.0 MB is over the 4 MB editing limit"
        );
        assert_eq!(status.actions[0].id, "code-status-open");
    });
}

#[cfg(unix)]
#[gpui::test]
fn code2809_file_identity_admission_precedes_any_storage_probe(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let (reader, visual, temp) = reader_viewer_chrome::tests::fixture(cx, "data.json");
    let vault = temp.path().join("vault");
    std::fs::hard_link(vault.join("data.json"), vault.join("alias.json")).unwrap();
    reader.update_in(visual, |reader, window, cx| {
        reader.preview_file("alias.json", window, cx)
    });
    settle(visual);
    let _restore = Restore(vault.clone());
    std::fs::set_permissions(&vault, std::fs::Permissions::from_mode(0o500)).unwrap();
    reader.update_in(visual, |reader, window, cx| {
        reader.toggle_source(window, cx)
    });
    settle(visual);
    reader.read_with(visual, |reader, cx| {
        assert!(reader.editing.is_none());
        assert!(
            reader
                .link_notice
                .as_ref()
                .unwrap()
                .contains("regular file"),
            "existing identity policy wins before a folder probe"
        );
        let code = reader
            .file_preview
            .as_ref()
            .unwrap()
            .code
            .as_ref()
            .unwrap()
            .read(cx);
        assert!(
            code.storage_reason.is_none(),
            "identity failure cannot become a storage lock"
        );
    });
}
