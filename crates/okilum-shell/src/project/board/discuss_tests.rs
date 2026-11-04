use super::*;
use crate::project::discuss_handoff::native_tests as native_fixture;
use ::core::prelude::v1::test;
use std::{cell::RefCell, rc::Rc};

#[gpui::test]
fn discuss_1784_real_board_click_opens_existing_panel_before_exact_append_and_close_revokes(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        reader_features::enable_test_modules(cx);
    });
    let temp = tempfile::tempdir().unwrap();
    let vault = temp.path().join("vault");
    let area = vault.join("project");
    std::fs::create_dir_all(area.join("ops")).unwrap();
    std::fs::write(vault.join("start.md"), "# Local fixture\n").unwrap();
    std::fs::write(
        area.join("ops/project.yaml"),
        "display_name: Fixture\nrepo: {host: git.oklabs.uk, full_name: BeFeast/okilum}\n",
    )
    .unwrap();
    let mut binding: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../okilum-t3/tests/fixtures/board-legacy.yaml"
    ))
    .unwrap();
    binding["issues"]
        .as_array_mut()
        .unwrap()
        .retain(|issue| issue["number"] == 1767);
    std::fs::write(
        area.join("ops/board.yaml"),
        serde_json::to_vec(&binding).unwrap(),
    )
    .unwrap();
    let mut retained_reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(vault.clone()),
                    open_path: Some(vault.join("start.md")),
                    index_dir: Some(temp.path().join("index")),
                    session_directory: Some(temp.path().join("state")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        retained_reader = Some(reader.clone());
        Root::new(reader, window, cx)
    });
    let reader = retained_reader.unwrap();
    visual.simulate_resize(size(px(1400.), px(1000.)));
    visual.run_until_parked();
    let manager = reader.update_in(visual, |reader, window, cx| {
        let manager = cx.new(|cx| {
            let mut view = native_fixture::view_with_draft(
                &temp.path().join("drafts"),
                "Keep my draft\n",
                window,
                cx,
            );
            view.area = Some(area.clone());
            view
        });
        let mut project = okilum_t3::projects::Project::read(&area).unwrap();
        project.manager = okilum_t3::projects::Manager::Thread("thread".into());
        project.manager_project = Some("project".into());
        reader.project_navigation.active = Some(project);
        reader.project_navigation.board_visible = true;
        reader.project_open = true;
        reader.project = Some(manager.clone());
        reader.ensure_project_board_using(
            area.clone(),
            |binding| {
                Ok(binding
                    .issues
                    .iter()
                    .map(|scope| okilum_t3::board::Issue {
                        number: scope.number,
                        title: "Exact Board context".into(),
                        updated_at: String::new(),
                        state: "open".into(),
                        body: String::new(),
                        labels: Vec::new(),
                        open_pull_requests: Vec::new(),
                        live_pulls: Vec::new(),
                        report: Default::default(),
                    })
                    .collect())
            },
            window,
            cx,
        );
        reader.project_navigation.boards[&area].update(cx, |board, cx| {
            board.list.preferences.mode = list::Mode::Board;
            cx.notify();
        });
        reader.close_panel(reader_layout::Panel::Backlinks, window, cx);
        cx.notify();
        manager
    });
    visual.run_until_parked();
    manager.update(visual, |manager, cx| {
        assert!(manager.draft_handoff_target(cx).is_none())
    });
    let expand = visual
        .debug_bounds("board-expand-1767")
        .expect("positive control: real Board card mounted");
    visual.simulate_click(expand.center(), Modifiers::default());
    visual.run_until_parked();
    let contexts = Rc::new(RefCell::new(Vec::new()));
    let observed = contexts.clone();
    let board = reader.read_with(visual, |reader, _| {
        reader.project_navigation.boards[&area].clone()
    });
    board.update(visual, |_, cx| {
        cx.subscribe(&cx.entity(), move |_, _, action: &Action, _| {
            if let Action::Discuss(text) = action {
                observed.borrow_mut().push(text.clone());
            }
        })
        .detach();
    });
    let discuss = visual
        .debug_bounds("board-discuss-1767")
        .expect("actual user action is visible");
    visual.simulate_click(discuss.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(contexts.borrow().len(), 1);
    let expected = format!("Keep my draft\n\n\n---\n{}", contexts.borrow()[0]);
    let target = manager.update(visual, |view, cx| {
        assert_eq!(
            view.composer.read(cx).typed_source_text(cx),
            expected,
            "one exact context append, not both caller and coordinator"
        );
        view.draft_handoff_target(cx).unwrap()
    });
    board.read_with(visual, |board, _| {
        assert!(board
            .notice
            .as_deref()
            .unwrap()
            .contains("Context appended"))
    });
    reader.read_with(visual, |reader, _| assert!(reader.panels.backlinks));
    for _ in 0..2 {
        reader.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        manager.update(visual, |view, cx| {
            assert_eq!(view.draft_handoff_target(cx), Some(target.clone()))
        });
    }
    reader.update_in(visual, |reader, window, cx| {
        reader.close_panel(reader_layout::Panel::Backlinks, window, cx);
        manager.update(cx, |view, cx| {
            assert!(
                view.draft_handoff_target(cx).is_none(),
                "explicit close revokes before next paint"
            )
        });
    });
    visual.run_until_parked();
    manager.update(visual, |view, cx| {
        assert_eq!(view.composer.read(cx).typed_source_text(cx), expected);
        let env = view.environment.as_ref().unwrap();
        assert!(
            okilum_t3::chat::MessageJournal::new(&temp.path().join("drafts"), env, "thread")
                .records()
                .unwrap()
                .is_empty()
        );
        assert!(
            okilum_t3::actions::Journal::new(&temp.path().join("drafts"), env, "thread")
                .records()
                .unwrap()
                .is_empty()
        );
    });
}
