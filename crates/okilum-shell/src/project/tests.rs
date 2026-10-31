use super::*;
use ::core::prelude::v1::test;

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
    assert!(dropdown.size.width <= px(272.));
    assert!(dropdown.size.height <= px(320.));
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
        first_label.origin.x - dropdown.origin.x < px(40.),
        "model names must align near the dropdown's left edge"
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
    assert!(
        gateway.model_writes.lock().unwrap().is_empty(),
        "selection requires explicit Apply"
    );
    let apply = visual.debug_bounds("project-model-apply").unwrap();
    visual.simulate_click(apply.center(), Modifiers::default());
    visual.run_until_parked();
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
    let original = visual.debug_bounds("project-model-choice-0").unwrap();
    visual.simulate_click(original.center(), Modifiers::default());
    visual.run_until_parked();
    let apply = visual.debug_bounds("project-model-apply").unwrap();
    visual.simulate_click(apply.center(), Modifiers::default());
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
    assert!(visual.debug_bounds("project-connection-status").is_some());
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
    assert!(visual.debug_bounds("project-error").is_some());
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
    let fixture = tempfile::tempdir().unwrap();
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
    let fixture = tempfile::tempdir().unwrap();
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
    let files = tempfile::tempdir().unwrap();
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
    let tab = visual
        .debug_bounds("project-executors")
        .expect("real Executors segment");
    visual.simulate_click(tab.center(), Modifiers::default());
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
    *gateway.list_error.lock().unwrap() = Some("metadata unavailable".into());
    visual.executor().advance_clock(Duration::from_secs(20));
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
    // A registry/manifest mismatch is equally visible and never rebinds ownership.
    *gateway.list_error.lock().unwrap() = None;
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
            .contains("Registry Manager differs"));
        assert_eq!(
            v.executors.as_ref().unwrap().registered[0].entry.thread_id,
            "mcp:ui"
        );
        assert_eq!(v.compose.read(cx).value().as_ref(), "Keep manager draft");
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

#[gpui::test]
fn connected_controls_collapse_until_requested_and_keep_the_renew_reminder(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let expiry = chrono::DateTime::parse_from_rfc3339("2026-10-30T15:00:00Z")
        .unwrap()
        .timestamp() as u64;
    assert_eq!(
        expiry_note(expiry),
        "Connection token expires 2026-10-30 15:00 UTC"
    );
    for status in [
        GatewayStatus::ExpiresSoon { expires_at: expiry },
        GatewayStatus::Connected { expires_at: expiry },
    ] {
        let soon = matches!(status, GatewayStatus::ExpiresSoon { .. });
        let gateway = Arc::new(fixture::Fake {
            connection_status: Some(status),
            ..Default::default()
        });
        let (root, visual) = cx.add_window_view(|window, cx| {
            let view =
                cx.new(|cx| fixture::view(state.path().to_owned(), gateway.clone(), window, cx));
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("project-connected-line").is_some());
        assert!(visual.debug_bounds("project-connection-menu").is_some());
        assert!(visual.debug_bounds("project-token-expiry").is_none());
        assert!(visual.debug_bounds("project-renew").is_none());
        assert!(visual.debug_bounds("project-sign-in-again").is_none());
        assert!(visual.debug_bounds("project-live-connect").is_none());
        let view = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<ProjectView>().unwrap()
        });
        // The menu's explicit Change/Renew request reveals the existing form.
        view.update_in(visual, |view, _, cx| {
            view.connection_details = true;
            cx.notify();
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("project-token-expiry").is_some(),
            "EXPIRY1773: token expiry must be visible"
        );
        assert_eq!(visual.debug_bounds("project-renew").is_some(), soon);
        assert_eq!(
            visual.debug_bounds("project-sign-in-again").is_some(),
            !soon
        );
        assert!(gateway.sends.lock().unwrap().is_empty());
        assert_eq!(
            visual.opened_url(),
            None,
            "a reminder cannot open OAuth automatically"
        );
        let done = visual.debug_bounds("project-connection-done").unwrap();
        visual.simulate_click(done.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("project-connected-line").is_some());
        assert!(visual.debug_bounds("project-token-expiry").is_none());
    }
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
