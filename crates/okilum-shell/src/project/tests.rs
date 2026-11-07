use super::*;
use ::core::prelude::v1::test;

// Deliberately outside cfg(native_manager): the old Windows MCP fallback must
// fail this regression instead of silently excluding its entire test module.
#[cfg(windows)]
#[gpui::test]
fn windows_manager_1788_uses_native_constructor(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let files = fixture::canonical_tempdir();
    cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let view = ProjectView::new(
                None,
                Some(files.path().join("state")),
                files.path().to_owned(),
                std::time::Instant::now(),
                window,
                cx,
            );
            assert!(
                cfg!(native_manager),
                "Windows must construct the shared native Manager, not the MCP fallback"
            );
            #[cfg(native_manager)]
            assert!(
                view.threads.enabled,
                "shipping constructor must select native"
            );
            view
        });
        Root::new(view, window, cx)
    });
}

#[cfg(native_manager)]
#[gpui::test]
fn composer_grows_and_shift_enter_keeps_a_newline_while_enter_sends_once(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake::default());
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture::view(temp.path().to_owned(), gateway.clone(), window, cx));
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(760.), px(900.)));
    visual.run_until_parked();
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    let initial = visual.debug_bounds("project-composer-input").unwrap();
    assert!(
        initial.size.height >= px(24.) && initial.size.height < px(48.),
        "the empty composer starts compact and grows with authored text: {initial:?}"
    );
    view.update_in(visual, |view, window, cx| {
        view.compose.update(cx, |input, cx| input.focus(window, cx))
    });
    visual.simulate_input("First line");
    visual.simulate_keystrokes("shift-enter");
    visual.simulate_input("Second line");
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.compose.read(cx).value().as_str(),
            "First line\nSecond line"
        )
    });
    assert!(
        gateway.sends.lock().unwrap().is_empty(),
        "Shift+Enter must not send"
    );
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    assert_eq!(
        gateway.sends.lock().unwrap().len(),
        1,
        "plain Enter sends exactly once"
    );
    assert_eq!(
        gateway.sends.lock().unwrap()[0].text,
        "First line\nSecond line"
    );
    view.read_with(visual, |view, cx| {
        assert!(view.compose.read(cx).value().is_empty())
    });
    visual.simulate_keystrokes("enter");
    assert_eq!(
        gateway.sends.lock().unwrap().len(),
        1,
        "empty Enter never sends"
    );
    visual.simulate_input(&"Another line\n".repeat(12));
    visual.run_until_parked();
    let grown = visual.debug_bounds("project-composer-input").unwrap();
    assert!(
        grown.size.height > initial.size.height + px(40.),
        "multiline paste grows the composer: {initial:?} -> {grown:?}"
    );
    assert!(
        grown.size.height <= px(320.),
        "long drafts keep the history viewport usable"
    );
    view.update_in(visual, |view, window, cx| {
        view.compose
            .update(cx, |input, cx| input.set_value("", window, cx))
    });
    visual.run_until_parked();
    assert_eq!(
        visual
            .debug_bounds("project-composer-input")
            .unwrap()
            .size
            .height,
        initial.size.height
    );
    view.update_in(visual, |view, window, cx| {
        view.compose.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(Some(0..0), "入力", None, window, cx);
            assert!(
                input.ime_composing(),
                "positive control: a real IME composition is active"
            );
        })
    });
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    assert_eq!(
        gateway.sends.lock().unwrap().len(),
        1,
        "confirming an IME candidate must not send a message"
    );
    view.update_in(visual, |view, window, cx| {
        view.compose.update(cx, |input, cx| {
            input.replace_text_in_range(None, "入力", window, cx);
            assert!(!input.ime_composing());
            assert!(
                input.ime_settling(cx),
                "positive control: commit leaves a settling input"
            );
        })
    });
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    assert_eq!(
        gateway.sends.lock().unwrap().len(),
        1,
        "the Enter delivered after IME commit must not send either"
    );
    visual.executor().advance_clock(Duration::from_millis(600));
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    assert_eq!(
        gateway.sends.lock().unwrap().len(),
        2,
        "ordinary Enter resumes after the composition settles"
    );
    assert_eq!(gateway.sends.lock().unwrap()[1].text, "入力");
}

#[gpui::test]
fn model_controls_confirm_readback_and_preserve_draft_on_gateway_refusal(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake {
        many_models: true,
        ..Default::default()
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let view = fixture::view(state.path().to_owned(), gateway.clone(), window, cx);
            view.compose.update(cx, |input, cx| {
                input.set_value("Retain the manager draft", window, cx)
            });
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual.run_until_parked();
    let history_before = visual.debug_bounds("project-chat-history").unwrap();
    let composer_before = visual.debug_bounds("project-chat-composer").unwrap();
    let button = visual
        .debug_bounds("project-manager-model")
        .expect("manager model control");
    visual.simulate_click(button.center(), Modifiers::default());
    visual.run_until_parked();
    let dropdown = visual
        .debug_bounds("project-model-panel")
        .expect("model dropdown");
    let viewport = visual.update(|window, _| window.viewport_size());
    assert!(dropdown.left() >= px(0.) && dropdown.right() <= viewport.width);
    assert!(dropdown.top() >= px(0.) && dropdown.bottom() <= viewport.height);
    assert_eq!(
        visual.debug_bounds("project-chat-history"),
        Some(history_before),
        "the dropdown must overlay history without shrinking it"
    );
    assert_eq!(
        visual.debug_bounds("project-chat-composer"),
        Some(composer_before),
        "the dropdown must not move the composer"
    );
    assert!(
        visual
            .debug_bounds("project-model-scheduler-limit")
            .is_none(),
        "internal migration diagnostics are not ordinary chat content"
    );
    let first_label = visual.debug_bounds("project-model-label-0").unwrap();
    let second_label = visual.debug_bounds("project-model-label-1").unwrap();
    assert_eq!(
        first_label.origin.x, second_label.origin.x,
        "model names share a left edge regardless of label width"
    );
    assert!(
        {
            let choices = visual.debug_bounds("project-model-choices").unwrap();
            first_label.left() >= choices.left()
                && first_label.left() - choices.left() <= px(64.)
                && first_label.right() <= choices.right()
        },
        "model names align within their own readable model column beside the account rail"
    );
    assert!(
        gateway.model_writes.lock().unwrap().is_empty(),
        "opening the catalog is read-only"
    );
    let alternative = visual
        .debug_bounds("project-model-choice-1")
        .expect("model from live catalog");
    visual.simulate_click(alternative.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        gateway.model_writes.lock().unwrap().len(),
        1,
        "a click selects without Apply"
    );
    assert!(visual.debug_bounds("project-model-panel").is_none());
    assert!(visual.debug_bounds("project-model-apply").is_none());
    view.read_with(visual, |view, cx| {
        assert_eq!(
            view.models
                .as_ref()
                .unwrap()
                .configuration
                .model_selection
                .model,
            "gpt-6-astra"
        );
        assert!(view.model_error.is_none());
        assert_eq!(
            view.compose.read(cx).value().as_ref(),
            "Retain the manager draft"
        );
    });
    *gateway.model_failure.lock().unwrap() = Some(okilum_t3::GatewayError::Tool {
        code: "capability_denied".into(),
        message: "Cannot configure full-access thread".into(),
    });
    let trigger = visual.debug_bounds("project-manager-model").unwrap();
    visual.simulate_click(trigger.center(), Modifiers::default());
    visual.run_until_parked();
    let original = visual.debug_bounds("project-model-choice-0").unwrap();
    visual.simulate_click(original.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        assert!(view
            .model_error
            .as_ref()
            .unwrap()
            .contains("capability_denied"));
        assert_eq!(
            view.models
                .as_ref()
                .unwrap()
                .configuration
                .model_selection
                .model,
            "gpt-6-astra",
            "a refused mutation cannot replace the last confirmed configuration"
        );
        assert_eq!(view.model_candidate.as_ref().unwrap().model, "gpt-6.1-sol");
        assert_eq!(
            view.compose.read(cx).value().as_ref(),
            "Retain the manager draft"
        );
    });
    assert_eq!(
        gateway.model_writes.lock().unwrap().len(),
        2,
        "no automatic retry"
    );
    assert!(
        gateway.sends.lock().unwrap().is_empty(),
        "model errors must not send the draft"
    );
    *gateway.model_read_failure.lock().unwrap() = Some(okilum_t3::GatewayError::Unreachable(
        "Read interrupted".into(),
    ));
    let refresh = visual.debug_bounds("project-model-refresh").unwrap();
    visual.simulate_click(refresh.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        let error = view.model_error.as_ref().unwrap();
        assert!(error.starts_with("Cannot read thread models; no change was requested."));
        assert!(error.contains("Read interrupted"));
        assert!(!error.contains("Model change not confirmed"));
        assert_eq!(
            view.models
                .as_ref()
                .unwrap()
                .configuration
                .model_selection
                .model,
            "gpt-6-astra"
        );
        assert_eq!(
            view.compose.read(cx).value().as_ref(),
            "Retain the manager draft"
        );
    });
    assert_eq!(
        gateway.model_writes.lock().unwrap().len(),
        2,
        "a failed Refresh never dispatches a model change"
    );
    // Closing must not discard a confirmed model or the unsent chat draft.
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-model-panel").is_none());
    view.read_with(visual, |view, cx| {
        assert!(!view.model_open);
        assert_eq!(
            view.models
                .as_ref()
                .unwrap()
                .configuration
                .model_selection
                .model,
            "gpt-6-astra"
        );
        assert_eq!(
            view.compose.read(cx).value().as_ref(),
            "Retain the manager draft"
        );
    });
    let trigger = visual.debug_bounds("project-manager-model").unwrap();
    visual.simulate_click(trigger.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-model-panel").is_some());
    visual.simulate_click(composer_before.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-model-panel").is_none(),
        "outside click dismisses dropdown"
    );
    assert_eq!(gateway.model_writes.lock().unwrap().len(), 2);
}

#[gpui::test]
fn manager_history_and_questions_render_through_the_gateway(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake::default());
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture::view(state.path().to_owned(), gateway.clone(), window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        let chat = view.chat.as_ref().unwrap();
        assert_eq!(chat.items.len(), 2);
        assert_eq!(chat.pending_questions, 1);
        assert_eq!(
            chat.items[&1].text.as_deref(),
            Some("The first slice is ready for review.")
        );
    });
    *gateway.grown.lock().unwrap() = true;
    visual.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.chat.as_mut().unwrap().poll(gateway.as_ref()).unwrap();
            cx.notify();
        })
    });
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert_eq!(view.chat.as_ref().unwrap().items.len(), 2);
        assert!(view.chat.as_ref().unwrap().items[&1]
            .text
            .as_deref()
            .unwrap()
            .contains("Next:"));
    });
    assert!(visual.debug_bounds("project-tab").is_some());
}

#[gpui::test]
fn a_revoked_connection_has_a_visible_reason_and_keeps_the_draft(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake {
        revoked: true,
        ..Default::default()
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let view = fixture::view(state.path().to_owned(), gateway.clone(), window, cx);
            view.compose.update(cx, |input, cx| {
                input.set_value("Keep this draft", window, cx)
            });
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual.run_until_parked();
    view.read_with(visual, |view, cx| {
        assert_eq!(view.compose.read(cx).value().as_ref(), "Keep this draft");
        assert!(status_note(&view.gateway.as_ref().unwrap().status())
            .unwrap()
            .contains("revoked"));
    });
    assert!(visual.debug_bounds("project-chat-status").is_some());
    assert!(visual.debug_bounds("project-connection-status").is_none());
    view.update_in(visual, |view, _, cx| {
        view.connection_details = true;
        cx.notify();
    });
    visual.run_until_parked();
    // This chrome belongs to the native Manager capability. Revoked status
    // and retained draft are asserted above on every platform.
    #[cfg(native_manager)]
    assert!(
        visual.debug_bounds("manager-details").is_some(),
        "revocation details remain available on request"
    );
}

#[gpui::test]
fn send_and_retry_keep_one_retained_message_and_one_request_id(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake {
        fail_first: true,
        ..Default::default()
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let view = fixture::view(state.path().to_owned(), gateway.clone(), window, cx);
            view.compose
                .update(cx, |input, cx| input.set_value("One message", window, cx));
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual.run_until_parked();
    let send = visual
        .debug_bounds("project-send")
        .expect("connected send control");
    visual.simulate_click(send.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert_eq!(view.records.len(), 1);
        assert!(view.records[0].receipt.is_none());
    });
    let retry = visual
        .debug_bounds("project-retry-0")
        .expect("retained message retry control");
    visual.simulate_click(retry.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert_eq!(view.records.len(), 1);
        assert!(view.records[0].receipt.is_some());
    });
    let calls = gateway.sends.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
}

#[test]
fn nearest_area_is_explicit_and_connections_never_contain_token_bytes() {
    let state = tempfile::tempdir().unwrap();
    let vault = state.path().join("vault");
    let area = vault.join("Area");
    std::fs::create_dir_all(area.join("ops")).unwrap();
    std::fs::create_dir_all(area.join("notes")).unwrap();
    std::fs::write(area.join("ops/project.yaml"), "display_name: Example\n").unwrap();
    assert_eq!(
        connections::area_for(&vault, &area.join("notes/note.md")).unwrap(),
        area
    );
    let env = Environment::new("test", "https://t3.example.test").unwrap();
    connections::save(state.path(), &env).unwrap();
    let source = std::fs::read_to_string(state.path().join("t3-connections.json")).unwrap();
    assert_eq!(source, "{\"test\":\"https://t3.example.test\"}");
}

#[gpui::test]
fn a_manager_from_another_project_is_not_enabled_for_sending(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake {
        wrong_project: true,
        ..Default::default()
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture::view(state.path().to_owned(), gateway, window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert!(!view.can_send());
        assert!(view
            .error
            .as_deref()
            .unwrap()
            .contains("another T3 project"));
    });
    // Cross-project send refusal above is portable; native onboarding chrome
    // is present only where that component is compiled.
    #[cfg(native_manager)]
    {
        assert!(visual.debug_bounds("manager-onboarding").is_some());
        assert!(
            visual.debug_bounds("project-error").is_none(),
            "raw setup errors stay under Details"
        );
    }
}

#[gpui::test]
fn projects_section_is_the_shared_picker_with_quiet_skips_and_row_warnings(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        crate::test_init(cx);
        bind_keys(cx);
    });
    let fixture = fixture::canonical_tempdir();
    let vault = fixture.path().join("vault");
    let grisaille_id = "b0728fed-73e6-45c2-9c61-a96f7df467f8";
    for (folder, manifest) in [
        ("Dev/Areas/App", "kind: knowledge\n".to_owned()),
        (
            "Dev/Areas/grisaille",
            format!(
                "slug: grisaille\nkind: code\nt3:\n  manager: {{thread_id: \"{grisaille_id}\"}}\n"
            ),
        ),
        ("Dev/Areas/intake", "kind: operating-area\n".to_owned()),
        ("Music/Areas/Broken", "kind: [code]\n".to_owned()),
    ] {
        std::fs::create_dir_all(vault.join(folder).join("ops")).unwrap();
        std::fs::write(vault.join(folder).join("ops/project.yaml"), manifest).unwrap();
        std::fs::write(vault.join(folder).join("note.md"), "# Project note\n").unwrap();
    }
    std::fs::write(vault.join("note.md"), "# Retained note\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    note: Some("note.md".into()),
                    index_dir: Some(fixture.path().join("index")),
                    session_directory: Some(fixture.path().join("state")),
                    ..Opts::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let reader = reader.unwrap();
    visual.simulate_resize(size(px(1400.), px(900.)));
    visual.run_until_parked();
    let show = |visual: &mut gpui::VisualTestContext| {
        visual.update(|window, cx| {
            reader.update(cx, |view, cx| {
                view.show_app_section(reader_project_navigation::Section::Projects, window, cx)
            })
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
    };
    show(visual);
    reader.read_with(visual, |view, _| {
        let discovery = &view.project_navigation.discovery;
        let names: Vec<_> = discovery.projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["App", "grisaille"], "intake is skipped quietly");
        assert_eq!(
            discovery.projects[1].manager,
            okilum_t3::projects::Manager::Thread(grisaille_id.into()),
            "a plain T3 UUID is a valid manager id"
        );
        assert_eq!(discovery.errors.len(), 1, "{:?}", discovery.errors);
        assert!(discovery.errors[0].0.ends_with("Music/Areas/Broken"));
    });
    // The picker is the rendered surface: left-aligned, grouped by domain,
    // the broken manifest is a muted row in its own domain.
    let section = visual
        .debug_bounds("app-section")
        .expect("section rendered");
    let picker = visual
        .debug_bounds("projects-picker")
        .expect("the shared picker is the Projects surface");
    assert!(
        picker.origin.x - section.origin.x < px(40.),
        "left-aligned, not centered: {picker:?} in {section:?}"
    );
    for selector in [
        "project-section-0",
        "project-choice-1",
        "project-choice-2",
        "project-section-3",
        "project-problem-4",
    ] {
        assert!(visual.debug_bounds(selector).is_some(), "{selector}");
    }
    assert!(visual.debug_bounds("project-choice-5").is_none());
    // The page has the switcher's rows (#1520): an icon slot before every
    // name, ⚠ only where project.yaml needs fixing (grisaille names a
    // manager thread without its T3 environment; Broken cannot be read).
    for index in [1, 2] {
        let icon = visual
            .debug_bounds(leak_selector(format!("project-icon-{index}")))
            .expect("icon slot");
        let row = visual
            .debug_bounds(leak_selector(format!("project-choice-{index}")))
            .unwrap();
        assert!(icon.left() - row.left() < px(12.), "icon on the left");
    }
    assert!(
        visual.debug_bounds("project-status-1").is_none(),
        "App: quiet"
    );
    assert!(
        visual.debug_bounds("project-status-2").is_some(),
        "grisaille: ⚠"
    );
    assert!(
        visual.debug_bounds("project-status-4").is_some(),
        "Broken: ⚠"
    );
    let entity = reader.read_with(visual, |view, _| {
        view.project_navigation.picker.clone().unwrap()
    });
    entity.read_with(visual, |picker, _| {
        let groups = picker.model().groups();
        let labels: Vec<_> = groups.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(labels, ["Dev", "Music"]);
        assert!(groups[1].problems[0]
            .detail
            .contains("invalid project kind"));
    });
    // Choosing a row enters through the existing pipeline and becomes recent.
    let app = visual.debug_bounds("project-choice-1").unwrap();
    visual.simulate_click(app.center(), gpui::Modifiers::none());
    visual.run_until_parked();
    reader.read_with(visual, |view, _| {
        assert_eq!(
            view.project_navigation.active.as_ref().unwrap().folder,
            vault.join("Dev/Areas/App")
        );
        assert_eq!(view.sidebar.recent_projects, ["Dev/Areas/App"]);
    });
    show(visual);
    entity.read_with(visual, |picker, _| {
        let groups = picker.model().groups();
        assert_eq!(groups[0].label, "Recent", "recents first");
        assert_eq!(groups[0].rows[0].project.name, "App");
    });
}

#[gpui::test]
fn projects_menu_scope_switch_and_leave_restore_the_original_tree(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let fixture = fixture::canonical_tempdir();
    let vault = fixture.path().join("vault");
    for folder in ["Dev/Areas/App", "Music/Areas/Studio"] {
        std::fs::create_dir_all(vault.join(folder).join("ops")).unwrap();
        std::fs::write(
            vault.join(folder).join("ops/project.yaml"),
            "kind: knowledge\n",
        )
        .unwrap();
        std::fs::write(vault.join(folder).join("note.md"), "# Project note\n").unwrap();
    }
    std::fs::write(vault.join("note.md"), "# Retained note\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    note: Some("note.md".into()),
                    index_dir: Some(fixture.path().join("index")),
                    session_directory: Some(fixture.path().join("state")),
                    ..Opts::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let reader = reader.unwrap();
    visual.simulate_resize(size(px(1400.), px(900.)));
    visual.run_until_parked();
    let before = reader.update(visual, |view, cx| {
        view.sync_tree();
        view.tree.set_subtree("Dev", true);
        view.tree.cursor = Some("note.md".into());
        cx.notify();
        (
            view.tree
                .rows
                .iter()
                .map(|r| r.path.clone())
                .collect::<Vec<_>>(),
            view.panels.clone(),
            view.content.clone(),
        )
    });
    for folder in ["Dev/Areas/App", "Music/Areas/Studio"] {
        let project = okilum_t3::projects::Project::read(&vault.join(folder)).unwrap();
        visual.update(|window, cx| {
            reader.update(cx, |view, cx| view.enter_vault_project(project, window, cx))
        });
        visual.run_until_parked();
        reader.read_with(visual, |view, _| {
            assert_eq!(
                view.project_navigation.active.as_ref().unwrap().folder,
                vault.join(folder)
            );
            assert!(!view.tree.rows.is_empty(), "scoped tree positive control");
            assert!(view
                .tree
                .rows
                .iter()
                .all(|r| r.path.starts_with(&format!("{folder}/"))));
            assert_eq!(view.current_rel, "note.md");
            assert!(view.content == before.2);
        });
    }
    visual.update(|window, cx| reader.update(cx, |view, cx| view.leave_vault_project(window, cx)));
    visual.run_until_parked();
    reader.read_with(visual, |view, _| {
        assert!(view.project_navigation.active.is_none());
        assert!(!view.project_open);
        assert_eq!(
            view.tree
                .rows
                .iter()
                .map(|r| r.path.clone())
                .collect::<Vec<_>>(),
            before.0
        );
        assert_eq!(view.tree.cursor.as_deref(), Some("note.md"));
        assert_eq!(view.panels, before.1);
        assert!(view.content == before.2);
    });
    assert!(
        visual.debug_bounds("reader-project").is_none(),
        "old top-right toggle is removed"
    );
}

#[cfg(unix)]
#[gpui::test]
fn a_failed_new_intent_keeps_a_draft_even_when_old_text_matches(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    #[cfg(target_os = "linux")]
    use std::os::unix::process::CommandExt;

    // The local CI container can run as root, which bypasses directory mode
    // bits. Run this same test unprivileged in a child; never change the UID
    // of the shared, multithreaded test runner.
    #[cfg(target_os = "linux")]
    if rustix::process::geteuid().as_raw() == 0 {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "project::tests::a_failed_new_intent_keeps_a_draft_even_when_old_text_matches",
                "--nocapture",
            ])
            .gid(65534)
            .uid(65534)
            .status()
            .expect("run the actual draft regression without root's permission bypass");
        assert!(status.success(), "unprivileged draft regression failed");
        return;
    }

    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake::default());
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let view = fixture::view(state.path().to_owned(), gateway.clone(), window, cx);
            let journal = view.journal().unwrap();
            let old = journal.prepare("Continue").unwrap();
            journal.send(gateway.as_ref(), &old).unwrap();
            view.compose
                .update(cx, |input, cx| input.set_value("Continue", window, cx));
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual.run_until_parked();
    let directory = std::fs::read_dir(state.path().join("t3-messages"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let original_permissions = std::fs::metadata(&directory).unwrap().permissions();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
    let denied = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("write-positive-control"))
        .expect_err("the journal must actually reject a new write");
    assert_eq!(denied.kind(), std::io::ErrorKind::PermissionDenied);
    view.read_with(visual, |view, _| {
        let records = view.journal().unwrap().records().unwrap();
        assert_eq!(records.len(), 1, "the older intent must remain readable");
        assert!(records[0].receipt.is_some());
    });
    let send = visual
        .debug_bounds("project-send")
        .expect("connected send control");
    visual.simulate_click(send.center(), Modifiers::default());
    visual.run_until_parked();
    std::fs::set_permissions(&directory, original_permissions).unwrap();
    view.read_with(visual, |view, cx| {
        assert!(!view.busy, "the failed send completed");
        assert!(view.error.is_some(), "failed persistence must be visible");
        assert_eq!(view.compose.read(cx).value().as_ref(), "Continue");
        assert_eq!(
            view.records.len(),
            1,
            "only the older operation is retained"
        );
        assert!(view.records[0].receipt.is_some());
    });
    assert_eq!(
        gateway.sends.lock().unwrap().len(),
        1,
        "only the old send reached T3"
    );
}

#[gpui::test]
fn rotation_deactivates_the_cached_manager_before_polling_its_replacement(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let files = fixture::canonical_tempdir();
    let vault = files.path().join("vault");
    std::fs::create_dir_all(vault.join("ops")).unwrap();
    std::fs::write(vault.join("note.md"), "# Retained note\n").unwrap();
    let old_gateway = Arc::new(fixture::Fake {
        grown: std::sync::Mutex::new(true),
        ..Default::default()
    });
    let new_gateway = Arc::new(fixture::Fake {
        grown: std::sync::Mutex::new(true),
        ..Default::default()
    });
    let mut entities = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let old =
            cx.new(|cx| fixture::view(files.path().join("state"), old_gateway.clone(), window, cx));
        let binding = old.read(cx).binding.clone().unwrap();
        let mut rotated = binding.clone();
        rotated.manager.thread_id = Some("mcp:rotated-manager".into());
        std::fs::write(
            vault.join("ops/project.yaml"),
            serde_yaml::to_string(&serde_json::json!({"kind": "code", "t3": binding})).unwrap(),
        )
        .unwrap();
        old.update(cx, |view, cx| {
            view.set_active(false, window, cx);
            view.compose.update(cx, |input, cx| {
                input.set_value("Old manager draft", window, cx)
            });
        });
        let new = cx.new(|cx| {
            let mut view =
                fixture::view(files.path().join("state"), new_gateway.clone(), window, cx);
            view.binding = Some(rotated.clone());
            view.chat = Some(Conversation::new("mcp:rotated-manager"));
            view.set_active(false, window, cx);
            view
        });
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    note: Some("note.md".into()),
                    index_dir: Some(files.path().join("index")),
                    session_directory: Some(files.path().join("state")),
                    ..Opts::default()
                },
                window,
                cx,
            )
        });
        reader.update(cx, |reader, _| {
            reader.project_views.insert(
                (
                    vault.clone(),
                    serde_json::to_string(&Some(binding.clone())).unwrap(),
                ),
                old.clone(),
            );
            reader.project_views.insert(
                (
                    vault.clone(),
                    serde_json::to_string(&Some(rotated.clone())).unwrap(),
                ),
                new.clone(),
            );
        });
        entities = Some((reader.clone(), old, new, binding, rotated));
        Root::new(reader, window, cx)
    });
    let (reader, old, new, binding, rotated) = entities.unwrap();
    visual.run_until_parked();
    visual.update(|window, cx| reader.update(cx, |r, cx| r.toggle_project(window, cx)));
    visual.run_until_parked();
    reader.read_with(visual, |r, _| assert!(r.project.as_ref().unwrap() == &old));
    assert!(
        !old_gateway.reads.lock().unwrap().is_empty(),
        "old poll positive control"
    );
    std::fs::write(
        vault.join("ops/project.yaml"),
        serde_yaml::to_string(&serde_json::json!({"kind": "code", "t3": rotated})).unwrap(),
    )
    .unwrap();
    visual.update(|window, cx| reader.update(cx, |r, cx| r.toggle_project(window, cx)));
    old.read_with(visual, |v, _| {
        assert!(!v.active, "close deactivates before replacement")
    });
    visual.update(|window, cx| reader.update(cx, |r, cx| r.toggle_project(window, cx)));
    visual.run_until_parked();
    reader.read_with(visual, |r, _| assert!(r.project.as_ref().unwrap() == &new));
    let old_count = old_gateway.reads.lock().unwrap().len();
    let new_count = new_gateway.reads.lock().unwrap().len();
    visual.executor().advance_clock(Duration::from_secs(60));
    visual.run_until_parked();
    assert_eq!(
        old_gateway.reads.lock().unwrap().len(),
        old_count,
        "superseded cache stopped polling"
    );
    assert!(
        new_gateway.reads.lock().unwrap().len() > new_count,
        "replacement polling positive control"
    );
    assert!(new_gateway
        .reads
        .lock()
        .unwrap()
        .iter()
        .all(|q| q.thread_id == "mcp:rotated-manager"));
    visual.update(|window, cx| reader.update(cx, |r, cx| r.close_project(window, cx)));
    std::fs::write(
        vault.join("ops/project.yaml"),
        serde_yaml::to_string(&serde_json::json!({"kind": "code", "t3": binding})).unwrap(),
    )
    .unwrap();
    visual.update(|window, cx| reader.update(cx, |r, cx| r.toggle_project(window, cx)));
    visual.run_until_parked();
    new.read_with(visual, |v, _| assert!(!v.active));
    old.read_with(visual, |v, cx| {
        assert!(v.active);
        assert_eq!(v.compose.read(cx).value().as_ref(), "Old manager draft");
    });
    reader.read_with(visual, |r, _| assert!(r.project.as_ref().unwrap() == &old));
}

#[gpui::test]
fn deactivation_and_reactivation_with_a_read_awaiting_publication_keep_one_poller(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake {
        grown: std::sync::Mutex::new(true),
        ..Default::default()
    });
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture::view(state.path().to_owned(), gateway.clone(), window, cx);
            // add_window_view drains the scheduler before returning. Keep the
            // view inactive through that drain, then step its first real read.
            view.set_active(false, window, cx);
            view
        });
        entity = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = entity.unwrap();
    assert!(gateway.reads.lock().unwrap().is_empty());
    visual.update(|window, cx| view.update(cx, |v, cx| v.set_active(true, window, cx)));
    for _ in 0..1000 {
        if !gateway.reads.lock().unwrap().is_empty() {
            break;
        }
        assert!(visual.executor().tick(), "read must reach the gateway");
    }
    assert_eq!(gateway.reads.lock().unwrap().len(), 1);
    view.read_with(visual, |v, _| {
        assert!(
            v.chat.as_ref().unwrap().items.is_empty(),
            "worker result not published yet"
        )
    });
    visual.update(|window, cx| view.update(cx, |v, cx| v.set_active(false, window, cx)));
    visual.update(|window, cx| view.update(cx, |v, cx| v.set_active(true, window, cx)));
    visual.run_until_parked();
    assert_eq!(
        gateway.reads.lock().unwrap().len(),
        1,
        "reactivation reuses the in-flight poller"
    );
    visual.executor().advance_clock(Duration::from_secs(20));
    visual.run_until_parked();
    assert_eq!(
        gateway.reads.lock().unwrap().len(),
        2,
        "one successor poll, without overlapping loops"
    );
    visual.update(|window, cx| view.update(cx, |v, cx| v.set_active(false, window, cx)));
    visual.executor().advance_clock(Duration::from_secs(60));
    visual.run_until_parked();
    assert_eq!(
        gateway.reads.lock().unwrap().len(),
        2,
        "deactivation cancels future reads"
    );
}

#[gpui::test]
fn executors_segment_uses_real_registry_shows_mismatches_and_retains_rows_and_draft_on_error(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let files = tempfile::tempdir().unwrap();
    let area = files.path().join("area");
    std::fs::create_dir_all(area.join("ops")).unwrap();
    let registry = "## Current threads\n\n| Role | Thread |\n|---|---|\n| Manager | `mcp:fixture-manager` |\n| UI | `mcp:ui` |\n| Missing | `mcp:missing` |\n";
    std::fs::write(area.join("ops/thread-ids.md"), registry).unwrap();
    let summary = |id: &str| {
        serde_json::from_value::<okilum_t3::ThreadSummary>(serde_json::json!({
        "threadId":id,"title":id,"status":"running","latestRunId":"r","providerInstanceId":"codex","model":"gpt-6.1-sol","runtimeMode":"full-access","parentThreadId":null,"itemCount":19,"updatedAt":"now","linkedPullRequest":null,"pullRequests":[{"url":"https://git.example/pulls/9"}]
    })).unwrap()
    };
    let gateway = Arc::new(fixture::Fake::default());
    *gateway.threads.lock().unwrap() = vec![summary("mcp:ui"), summary("mcp:unknown")];
    let mut entity = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture::view(files.path().join("state"), gateway.clone(), window, cx);
            view.area = Some(area.clone());
            view.compose.update(cx, |input, cx| {
                input.set_value("Keep manager draft", window, cx)
            });
            view
        });
        entity = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = entity.unwrap();
    let menu = visual.debug_bounds("project-connection-menu").unwrap();
    visual.simulate_click(menu.center(), Modifiers::default());
    visual.run_until_parked();
    // The native Manager has Rotation policy before Details.
    #[cfg(native_manager)]
    visual.simulate_keystrokes("down down down down enter");
    #[cfg(not(native_manager))]
    visual.simulate_keystrokes("down down down enter");
    visual.executor().advance_clock(Duration::from_secs(20));
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(v.executors_open);
        let s = v
            .executors
            .as_ref()
            .expect("registry and actual gateway projection");
        assert_eq!(s.registered.len(), 2);
        assert_eq!(s.registered[0].entry.thread_id, "mcp:ui");
        assert_eq!(s.registered[0].thread.as_ref().unwrap().item_count, 19);
        assert_eq!(s.registered[0].pending_questions, Some(1));
        assert_eq!(
            s.registered[0].thread.as_ref().unwrap().pull_request_urls(),
            ["https://git.example/pulls/9"]
        );
        assert!(s.registered[1].thread.is_none());
        assert_eq!(s.unregistered[0].thread_id, "mcp:unknown");
    });
    assert!(visual.debug_bounds("project-executors-list").is_some());
    assert!(visual.debug_bounds("project-executor-0").is_some());
    let (connection, generation, reads) = view.read_with(visual, |v, _| {
        (
            v.gateway.clone().unwrap(),
            v.generation,
            gateway.reads.lock().unwrap().len(),
        )
    });
    *gateway.threads.lock().unwrap() = vec![summary("mcp:missing")];
    let refresh = visual
        .debug_bounds("project-executors-refresh")
        .expect("explicit Refresh");
    visual.simulate_click(refresh.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |v, cx| {
        assert!(
            Arc::ptr_eq(v.gateway.as_ref().unwrap(), &connection),
            "Refresh keeps the authenticated gateway"
        );
        assert_eq!(v.generation, generation, "Refresh never reconnects");
        assert_eq!(v.chat.as_ref().unwrap().thread_id, "mcp:fixture-manager");
        assert_eq!(v.compose.read(cx).value().as_ref(), "Keep manager draft");
        let snapshot = v.executors.as_ref().unwrap();
        assert!(snapshot.registered[0].thread.is_none());
        assert!(
            snapshot.registered[1].thread.is_some(),
            "button read the changed server listing before polling"
        );
    });
    assert_eq!(
        gateway.reads.lock().unwrap().len(),
        reads,
        "executor refresh does not re-read Manager history"
    );
    *gateway.list_error.lock().unwrap() = Some("metadata unavailable".into());
    visual.simulate_click(refresh.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |v, cx| {
        assert!(v
            .executors_error
            .as_deref()
            .unwrap()
            .contains("metadata unavailable"));
        assert_eq!(v.executors.as_ref().unwrap().registered.len(), 2);
        assert_eq!(v.compose.read(cx).value().as_ref(), "Keep manager draft");
    });
    assert!(gateway.sends.lock().unwrap().is_empty());
    *gateway.list_error.lock().unwrap() = None;
    std::fs::write(
        area.join("ops/thread-ids.md"),
        registry.replace(
            "`mcp:fixture-manager`",
            "[Manager](t3-thread://env/mcp:fixture-manager)",
        ),
    )
    .unwrap();
    visual.simulate_click(refresh.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        let error = v.executors_error.as_ref().unwrap();
        assert!(
            error.contains("ops/thread-ids.md:5")
                && error.contains("Manager")
                && error.contains("`mcp:<id>`"),
            "{error}"
        );
        assert!(
            v.executors.as_ref().unwrap().registered[1].thread.is_some(),
            "invalid source retains previous rows"
        );
    });
    // A registry/manifest mismatch is equally visible and never rebinds ownership.
    std::fs::write(
        area.join("ops/thread-ids.md"),
        registry.replace("mcp:fixture-manager", "mcp:other-manager"),
    )
    .unwrap();
    visual.executor().advance_clock(Duration::from_secs(20));
    visual.run_until_parked();
    view.read_with(visual, |v, cx| {
        assert!(v
            .executors_error
            .as_deref()
            .unwrap()
            .contains("ops/thread-ids.md:5: Manager row"));
        assert_eq!(
            v.executors.as_ref().unwrap().registered[0].entry.thread_id,
            "mcp:ui"
        );
        assert_eq!(v.compose.read(cx).value().as_ref(), "Keep manager draft");
    });
    // Correcting the source recovers immediately without reconnecting or waiting
    // for the regular poll. Then a late read cannot publish into a new binding.
    std::fs::write(area.join("ops/thread-ids.md"), registry).unwrap();
    visual.simulate_click(refresh.center(), Modifiers::default());
    visual.run_until_parked();
    view.read_with(visual, |v, _| assert!(v.executors_error.is_none()));
    visual.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.refresh_executors(window, cx);
            assert_eq!(v.executors_refreshing, Some(v.generation));
            v.generation = v.generation.wrapping_add(1);
            v.executors = None;
        })
    });
    visual.run_until_parked();
    view.read_with(visual, |v, _| {
        assert!(
            v.executors.is_none(),
            "stale refresh must not repopulate another project"
        );
        assert!(v.executors_refreshing.is_none());
    });
}

fn leak_selector(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

#[gpui::test]
fn manager_address_field_normalizes_and_saves_a_strict_origin(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            fixture::view(
                state.path().to_owned(),
                Arc::new(fixture::Fake::default()),
                window,
                cx,
            )
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    for (entered, expected) in [
        ("t3.oklabs.uk", "https://t3.oklabs.uk"),
        ("10.10.0.23:23110", "http://10.10.0.23:23110"),
    ] {
        view.update_in(visual, |view, window, cx| {
            view.origin
                .update(cx, |input, cx| input.set_value(entered, window, cx));
            let env = view
                .connection_environment(cx)
                .expect("T3_INPUT1755: human manager address must normalize before OAuth");
            assert_eq!(env.origin(), expected);
            connections::save(state.path(), &env).unwrap();
            let stored: std::collections::BTreeMap<String, String> = serde_json::from_slice(
                &std::fs::read(state.path().join("t3-connections.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(stored["example"], expected);
            assert_eq!(
                connections::load(state.path(), view.binding.as_ref().unwrap())
                    .unwrap()
                    .unwrap(),
                env
            );
        });
    }
    // Persistent state remains a strict origin API, never an input normalizer.
    std::fs::write(
        state.path().join("t3-connections.json"),
        r#"{"example":"t3.oklabs.uk"}"#,
    )
    .unwrap();
    view.read_with(visual, |view, _| {
        assert!(connections::load(state.path(), view.binding.as_ref().unwrap()).is_err())
    });
}

#[gpui::test]
fn manager_address_field_shows_a_friendly_error_without_starting_oauth(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            fixture::view(
                state.path().to_owned(),
                Arc::new(fixture::Fake::default()),
                window,
                cx,
            )
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    for input in ["", "not a host", "ftp://t3.oklabs.uk"] {
        view.update_in(visual, |view, window, cx| {
            view.origin
                .update(cx, |field, cx| field.set_value(input, window, cx));
            view.connect(window, cx);
            assert!(
                view.error
                    .as_ref()
                    .unwrap()
                    .starts_with("Enter a T3 address like "),
                "T3_INPUT1755: {:?}",
                view.error
            );
            assert!(!view.busy);
            assert_eq!(view.origin.read(cx).value().as_ref(), input);
        });
    }
    assert!(!state.path().join("t3-connections.json").exists());
}

#[gpui::test]
fn unavailable_questions_keep_manager_history_and_send_working(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake {
        question_failure: Some(okilum_t3::GatewayError::Unsupported {
            tool: "t3_pending_request".into(),
        }),
        ..Default::default()
    });
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let view = fixture::view(state.path().to_owned(), gateway.clone(), window, cx);
            view.compose.update(cx, |input, cx| {
                input.set_value("A message despite unavailable questions", window, cx)
            });
            view
        });
        Root::new(view, window, cx)
    });
    let view = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<ProjectView>().unwrap()
    });
    visual.run_until_parked();
    view.read_with(visual, |view, _| {
        assert!(
            view.can_send(),
            "T3_PANEL1759: optional questions cannot disable send"
        );
        assert!(view.error.is_none());
        let chat = view.chat.as_ref().unwrap();
        assert_eq!(chat.items.len(), 2);
        assert!(chat
            .pending_question_error
            .as_ref()
            .unwrap()
            .contains("t3_pending_request"));
    });
    assert!(visual
        .debug_bounds("project-questions-unavailable")
        .is_some());
    let send = visual
        .debug_bounds("project-send")
        .expect("connected send control");
    visual.simulate_click(send.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(gateway.sends.lock().unwrap().len(), 1);
    view.read_with(visual, |view, _| {
        assert!(view.error.is_none());
        assert!(view.records[0].receipt.is_some());
    });
}

#[cfg(native_manager)]
#[gpui::test]
fn connected_controls_collapse_until_details_is_requested(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let expiry = chrono::Utc::now().timestamp().max(0) as u64 + 3600;
    let gateway = Arc::new(fixture::Fake::default());
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = fixture::view(state.path().to_owned(), gateway.clone(), window, cx);
            view.native.set_detail_test_expiry(expiry);
            view
        });
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-connected-line").is_some());
    assert!(visual.debug_bounds("manager-details").is_none());
    for selector in [
        "project-token-expiry",
        "project-renew",
        "project-sign-in-again",
        "project-live-connect",
        "project-pair",
        "project-diagnostics",
    ] {
        assert!(
            visual.debug_bounds(selector).is_none(),
            "old connection dump: {selector}"
        );
    }
    let details = visual.debug_bounds("manager-details-toggle").unwrap();
    visual.simulate_click(details.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("manager-details").is_some());
    let copy = visual.debug_bounds("manager-copy-details").unwrap();
    visual.simulate_click(copy.center(), Modifiers::default());
    visual.run_until_parked();
    visual.update(|_, cx| {
        let text = cx.read_from_clipboard().unwrap().text().unwrap();
        assert!(text.contains(&expiry_note(expiry)));
        let local = chrono::DateTime::from_timestamp(expiry as i64, 0)
            .unwrap()
            .with_timezone(&chrono::Local);
        assert!(text.contains(&local.format("%Y-%m-%d %H:%M %Z").to_string()));
    });
    let details = visual.debug_bounds("manager-details-toggle").unwrap();
    visual.simulate_click(details.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("manager-details").is_none());
    assert!(gateway.sends.lock().unwrap().is_empty());
    assert_eq!(visual.opened_url(), None);
    drop(root);
}

#[gpui::test]
fn shared_sign_out_stops_matching_panels_and_preserves_drafts(cx: &mut TestAppContext) {
    struct Panels(Vec<Entity<ProjectView>>);
    impl Render for Panels {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            h_flex().size_full().children(self.0.iter().cloned())
        }
    }
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let panels = [
            "https://t3.example.test",
            "https://t3.example.test",
            "https://other.example.test",
        ]
        .into_iter()
        .map(|origin| {
            cx.new(|cx| {
                let mut view = fixture::view(
                    state.path().to_owned(),
                    Arc::new(fixture::Fake::default()),
                    window,
                    cx,
                );
                view.environment = Some(Environment::new("example", origin).unwrap());
                view.compose.update(cx, |input, cx| {
                    input.set_value("Keep this draft", window, cx)
                });
                view
            })
        })
        .collect();
        let group = cx.new(|_| Panels(panels));
        Root::new(group, window, cx)
    });
    visual.run_until_parked();
    let group = root.read_with(visual, |root, _| {
        root.view().clone().downcast::<Panels>().unwrap()
    });
    let panels = group.read_with(visual, |group, _| group.0.clone());
    for panel in &panels {
        panel.read_with(visual, |view, _| assert!(view.can_send()));
    }
    visual.update(|_, cx| {
        reader_t3_settings::sign_out(
            Environment::new("example", "https://t3.example.test").unwrap(),
            cx,
        );
    });
    visual.run_until_parked();
    for panel in &panels[..2] {
        panel.read_with(visual, |view, cx| {
            assert!(view.gateway.is_none());
            assert!(!view.can_send());
            assert!(view.chat.is_some());
            assert_eq!(view.compose.read(cx).value().as_ref(), "Keep this draft");
        });
    }
    panels[2].read_with(visual, |view, cx| {
        assert!(view.can_send(), "another origin must stay connected");
        assert_eq!(view.compose.read(cx).value().as_ref(), "Keep this draft");
    });
}

#[gpui::test]
fn reconnect_browser_guidance_opens_only_the_entered_t3_origin(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake {
        connection_status: Some(GatewayStatus::Revoked),
        ..Default::default()
    });
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| fixture::view(state.path().to_owned(), gateway.clone(), window, cx));
        Root::new(view, window, cx)
    });
    visual.run_until_parked();
    let button = visual
        .debug_bounds("project-browser-sign-in")
        .expect("BROWSER1773: explicit browser sign-in link");
    assert_eq!(visual.opened_url(), None);
    visual.simulate_click(button.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        visual.opened_url().as_deref(),
        Some("https://t3.example.test")
    );
    assert!(gateway.sends.lock().unwrap().is_empty());
}

#[cfg(native_manager)]
#[gpui::test]
fn narrow_manager_keeps_identity_above_history_and_all_message_controls_together(
    cx: &mut TestAppContext,
) {
    use okilum_t3::stream::{NativeClient, NativeToken};
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let gateway = Arc::new(fixture::Fake::default());
    let (root, visual) = cx.add_window_view(|window,cx| {
        let view = cx.new(|cx| {
            let mut view = fixture::view(temp.path().into(),gateway.clone(),window,cx);
            view.active=false;
            let env=view.environment.clone().unwrap();
            let token=NativeToken::from_secret(&env,&serde_json::to_vec(&serde_json::json!({"origin":env.origin(),"access_token":"layout-fixture","expires_at":chrono::Utc::now().timestamp() as u64+3600,"scope":"orchestration:read orchestration:operate"})).unwrap()).unwrap();
            view.native.set_detail_test_client(Arc::new(NativeClient::new(env,token,Duration::from_millis(10)).unwrap()));
            let target=view.manager_model_target().unwrap();
            view.models=Some(target.load(gateway.as_ref()).unwrap());
            view.model_target=Some(target);
            view.native.actions.update(cx,|actions,cx| {
                actions.state = okilum_t3::actions::State { ready:true,active_run:Some("layout-run".into()),..Default::default() };
                cx.notify();
            });
            view
        });
        Root::new(view,window,cx)
    });
    visual.simulate_resize(size(px(320.), px(900.)));
    visual.run_until_parked();
    let mut bounds = |selector| {
        visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing visible {selector}"))
    };
    let header = bounds("project-chat-header");
    let composer = bounds("project-chat-composer");
    let model = bounds("project-manager-model");
    let input = bounds("project-composer-input");
    let stop = bounds("project-interrupt");
    assert!(
        header.size.height <= px(56.),
        "identity has one compact row: {header:?}"
    );
    assert!(
        header.bottom() < model.top(),
        "model configuration belongs to the composer, not the identity header"
    );
    assert!(
        model.top() >= input.bottom() && model.bottom() <= composer.bottom(),
        "model belongs to the unified composer toolbar below text entry"
    );
    assert!(
        input.size.width >= px(240.),
        "text entry keeps the full narrow-panel width: {input:?}"
    );
    assert!(
        stop.top() >= composer.top() && stop.bottom() <= input.top(),
        "active status and Stop belong to the same message surface above text entry"
    );
    for selector in [
        "project-manager-title",
        "project-chat-status",
        "project-open-t3",
        "project-manager-model",
        "project-interrupt",
        "project-attach",
        "project-send-mode",
        "project-send",
    ] {
        let control = bounds(selector);
        assert!(
            control.left() >= px(0.) && control.right() <= px(320.) && control.bottom() <= px(900.),
            "control escaped narrow panel: {selector} {control:?}"
        );
    }
    assert!(bounds("project-attach").top() >= input.bottom());
    assert!(bounds("project-send").top() >= input.bottom());
    visual.simulate_click(model.center(), Modifiers::default());
    visual.run_until_parked();
    let choice = visual
        .debug_bounds("project-model-choice-0")
        .expect("usable models are visible in a narrow popup");
    assert!(
        choice.left() >= px(0.)
            && choice.right() <= px(320.)
            && choice.top() >= px(0.)
            && choice.bottom() <= px(900.),
        "model popup escaped the narrow viewport: {choice:?}"
    );
    assert!(
        gateway.model_writes.lock().unwrap().is_empty(),
        "layout and opening the picker cannot select a model"
    );
    assert!(gateway.sends.lock().unwrap().is_empty());
    let _ = root;
}

#[cfg(not(target_os = "macos"))]
#[gpui::test]
fn mounted_manager_has_one_compact_header_with_status_menu_and_open(cx: &mut TestAppContext) {
    cx.update(crate::test_init);
    let temp = tempfile::tempdir().unwrap();
    let vault = temp.path().join("vault");
    std::fs::create_dir_all(vault.join("ops")).unwrap();
    std::fs::write(vault.join("ops/project.toml"), "kind = 'knowledge'\n").unwrap();
    std::fs::write(vault.join("home.md"), "# Home\n").unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    note: Some("home.md".into()),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(temp.path().join("state")),
                    panel_settings_override: Some(temp.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(800.)));
    visual.run_until_parked();
    let reader = reader.unwrap();
    reader.update_in(visual, |reader, window, cx| {
        let project = cx.new(|cx| {
            let mut project = fixture::view(
                temp.path().join("fixture"),
                Arc::new(fixture::Fake::default()),
                window,
                cx,
            );
            project.active = false;
            project.identity = Some(ServerIdentity {
                environment_id: "environment:fixture".into(),
                label: "Fixture".into(),
            });
            project
        });
        reader.project_navigation.active =
            Some(okilum_t3::projects::Project::read(&vault).unwrap());
        reader.project = Some(project);
        reader.project_open = true;
        reader.panels.close(reader_layout::Panel::Notes);
        reader.panels.open(reader_layout::Panel::Backlinks);
        reader.panel_widths.backlinks = 560.;
        cx.notify();
    });
    // The real mounted panel, including section navigation, must retain the
    // identity header at its top at both ordinary and narrow panel widths.
    for width in [560., 320.] {
        reader.update(visual, |reader, cx| {
            reader.panel_widths.backlinks = width;
            cx.notify();
        });
        visual.run_until_parked();
        // A same-width parent notification can reuse the cached Manager scene.
        visual.update(|window, _| window.refresh());
        visual.run_until_parked();
        let panel = visual
            .debug_bounds("reader-backlinks-panel")
            .expect("mounted Manager panel");
        let header = visual
            .debug_bounds("project-chat-header")
            .expect("workspace owns one header");
        assert!(
            visual.debug_bounds("reader-backlinks-header").is_none(),
            "no duplicate outer Manager title"
        );
        assert_eq!(panel.size.width, px(width));
        assert_eq!(header.top(), panel.top());
        assert_eq!(header.size.height, px(40.));
        assert_eq!(header.right(), panel.right());
        let mut previous_right = header.left();
        for selector in [
            "project-manager-title",
            "project-chat-status",
            "project-connection-menu",
            "project-open-t3",
        ] {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("functional header control: {selector}"));
            assert!(
                bounds.top() >= header.top() && bounds.bottom() <= header.bottom(),
                "{selector} stays in the single header at {width}px: {bounds:?}"
            );
            assert!(
                bounds.left() >= previous_right && bounds.right() <= header.right(),
                "{selector} keeps its order without overlap or horizontal overflow at {width}px: {bounds:?}"
            );
            assert!(
                (bounds.center().y - header.center().y).abs() <= px(1.),
                "{selector} is vertically centered at {width}px: {bounds:?}"
            );
            previous_right = bounds.right();
        }
        let title = visual.debug_bounds("project-manager-title").unwrap();
        let open = visual.debug_bounds("project-open-t3").unwrap();
        assert_eq!(title.left() - header.left(), px(design::INSET));
        assert_eq!(header.right() - open.right(), px(design::INSET));

        // #2463 was the identity heading plus a section tab also labelled
        // Manager. Selectors use each button's actual rendered label, so
        // restoring that label fails this check even with only one title node.
        // Exercise every section through its real button, including the return
        // to Chat: Rules/Skills must remain usable without repeating identity.
        for (selector, section) in [
            ("project-section-label-Rules", ProjectSection::Rules),
            ("project-section-label-Skills", ProjectSection::Skills),
            ("project-section-label-Chat", ProjectSection::Manager),
        ] {
            let tab = visual.debug_bounds(selector).expect("visible section tab");
            visual.simulate_click(tab.center(), Modifiers::default());
            visual.run_until_parked();
            reader.read_with(visual, |reader, cx| {
                assert!(reader.project.as_ref().unwrap().read(cx).section == section);
            });
            assert!(
                visual.debug_bounds("project-manager-title").is_some(),
                "positive control: the Manager identity is mounted after selecting {selector}"
            );
            assert!(
                visual
                    .debug_bounds("project-section-label-Manager")
                    .is_none(),
                "Manager identity must not be repeated by section navigation after selecting {selector}"
            );
            for selector in [
                "project-section-label-Chat",
                "project-section-label-Rules",
                "project-section-label-Skills",
            ] {
                assert!(
                    visual.debug_bounds(selector).is_some(),
                    "visible {selector}"
                );
            }
        }
    }
}
