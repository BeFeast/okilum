use super::*;
use crate::project::*;
use ::core::prelude::v1::test;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

fn init(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        reader_features::enable_test_modules(cx);
    });
}
fn write_skill(root: &std::path::Path, name: &str) {
    let source = root.join("p/.agents/skills/card/SKILL.md");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(
        source,
        format!("---\nname: {name}\ndescription: Local fixture\n---\nExact local body\n"),
    )
    .unwrap();
}
fn publication(root: &std::path::Path, empty: bool) -> Arc<Vault> {
    let mut vault = Vault::from_note_paths(if empty {
        vec![]
    } else {
        vec!["p/.agents/skills/card/SKILL.md".into()]
    });
    vault.root = root.to_owned();
    Arc::new(vault)
}
fn mount<'a>(
    cx: &'a mut TestAppContext,
    state: &std::path::Path,
) -> (
    Entity<Reader>,
    Entity<ProjectView>,
    &'a mut VisualTestContext,
) {
    init(cx);
    let root = state.join("vault");
    write_skill(&root, "alpha");
    let mut entities = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            let mut reader = Reader::new(
                Opts {
                    defer_loading: true,
                    session_directory: Some(state.join("state")),
                    panel_settings_override: Some(state.join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            );
            reader.vault_root = root.clone();
            reader.vault = publication(&root, false);
            reader
        });
        let project = cx.new(|cx| {
            let mut view = ProjectView::empty(Some(state.join("state")), window, cx);
            view.area = Some(root.join("p"));
            view.bind_resources(
                &reader,
                root.clone(),
                reader.read(cx).vault.clone(),
                reader.read(cx).source_publication_epoch,
                cx,
            );
            view.select_section(ProjectSection::Skills, cx);
            view
        });
        entities = Some((reader, project.clone()));
        Root::new(project, window, cx)
    });
    let (reader, project) = entities.unwrap();
    (reader, project, visual)
}
fn is_fresh(view: &ProjectView) -> bool {
    matches!(view.skills.freshness(), Freshness::Fresh)
}

#[gpui::test]
async fn skills_1591_actual_reload_keeps_lastgood_on_partial_then_fresh_retry(
    cx: &mut TestAppContext,
) {
    let state = tempfile::tempdir().unwrap();
    let (_reader, project, visual) = mount(cx, state.path());
    visual.condition(&project, |view, _| is_fresh(view)).await;
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-skill-0").is_some(),
        "positive control: actual Skills row painted"
    );
    let root = state.path().join("vault");
    let directory = root.join("p/.agents/skills");
    let parked = root.join("p/.agents/skills-held");
    std::fs::rename(&directory, &parked).unwrap();
    std::fs::write(&directory, b"not a directory").unwrap();
    let reload_button = visual.debug_bounds("refresh-project-skills").unwrap();
    visual.simulate_click(reload_button.center(), Modifiers::default());
    visual
        .condition(&project, |view, _| {
            matches!(view.skills.freshness(), Freshness::Incomplete)
        })
        .await;
    visual.run_until_parked();
    project.read_with(visual, |view, _| {
        assert_eq!(view.skills.snapshot().unwrap().skills[0].name, "alpha");
        assert!(!view.skills.reload_diagnostics().is_empty());
        assert!(view.gateway.is_none());
    });
    assert!(visual.debug_bounds("project-skill-0").is_some());
    assert!(
        visual.debug_bounds("project-skills-diagnostic-0").is_some(),
        "actual reload diagnostics are mounted beside lastgood"
    );
    project.update_in(visual, |view, _, cx| {
        let failed = begin(view, cx).unwrap();
        assert!(finish(
            view,
            &failed,
            Err("Offline reload worker unavailable".into()),
            cx
        ));
        assert!(matches!(view.skills.freshness(), Freshness::Failed(_)));
        assert_eq!(view.skills.snapshot().unwrap().skills[0].name, "alpha");
        cx.notify();
    });
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-skill-0").is_some(),
        "failed reload also keeps the actual lastgood row"
    );
    std::fs::remove_file(&directory).unwrap();
    std::fs::rename(&parked, &directory).unwrap();
    write_skill(&root, "bravo");
    let canonical = std::fs::read(directory.join("card/SKILL.md")).unwrap();
    let reload_button = visual.debug_bounds("refresh-project-skills").unwrap();
    visual.simulate_click(reload_button.center(), Modifiers::default());
    visual
        .condition(&project, |view, _| {
            is_fresh(view) && view.skills.snapshot().unwrap().skills[0].name == "bravo"
        })
        .await;
    project.read_with(visual, |view, _| {
        assert!(view.skills.reload_diagnostics().is_empty());
        assert!(view.records.is_empty());
    });
    assert_eq!(
        std::fs::read(directory.join("card/SKILL.md")).unwrap(),
        canonical,
        "Reload only reads source"
    );
}

#[gpui::test]
async fn skills_1591_content_only_publication_refuses_completion_before_observer_drains(
    cx: &mut TestAppContext,
) {
    let state = tempfile::tempdir().unwrap();
    let (reader, project, visual) = mount(cx, state.path());
    visual.condition(&project, |view, _| is_fresh(view)).await;
    let (ticket, old_scope, old_revision) = project.update_in(visual, |view, _, cx| {
        (
            begin(view, cx).unwrap(),
            view.skills.scope().cloned(),
            view.skills.inventory_revision().cloned(),
        )
    });
    let root = state.path().join("vault");
    write_skill(&root, "bravo");
    // #2405 retains the inventory Arc across content-only saves. The source
    // publication epoch must still reject an old result before observers drain.
    reader.update_in(visual, |reader, _, _| {
        let next = publication(&root, false);
        assert_eq!(
            reader.vault.link_inventory_revision(),
            next.link_inventory_revision()
        );
        let original = reader.vault.clone();
        assert!(reader.vault.publish_semantics(&next));
        reader.source_publication_epoch = reader.source_publication_epoch.wrapping_add(1);
        assert!(
            Arc::ptr_eq(&reader.vault, &original),
            "content-only publication keeps inventory ownership"
        );
    });
    let obsolete_result = ticket.request.discover();
    project.update_in(visual, |view, _, cx| {
        assert!(
            view.skills.inventory_revision() == old_revision.as_ref(),
            "positive control: the publication observer has NOT updated the cached revision"
        );
        assert!(!finish(view, &ticket, Ok(obsolete_result), cx));
        assert_eq!(view.skills.snapshot().unwrap().skills[0].name, "alpha");
        assert!(!display_current(
            view,
            old_scope.as_ref(),
            old_revision.as_ref(),
            cx
        ));
        view.refresh_skills(cx);
    });
    visual
        .condition(&project, |view, _| {
            is_fresh(view) && view.skills.snapshot().unwrap().skills[0].name == "bravo"
        })
        .await;
    project.read_with(visual, |view, cx| {
        assert!(
            display_current(
                view,
                view.skills.scope(),
                view.skills.inventory_revision(),
                cx
            ),
            "positive control: exact CURRENT publication applies without an observer notification"
        );
    });
}

#[gpui::test]
async fn skills_1591_empty_publication_invalidates_old_cards_and_explicit_reload_is_fresh(
    cx: &mut TestAppContext,
) {
    let state = tempfile::tempdir().unwrap();
    let (reader, project, visual) = mount(cx, state.path());
    visual.condition(&project, |view, _| is_fresh(view)).await;
    let (ticket, old_result) = project.update_in(visual, |view, _, cx| {
        let ticket = begin(view, cx).unwrap();
        let result = ticket.request.discover();
        (ticket, result)
    });
    let root = state.path().join("vault");
    std::fs::remove_dir_all(root.join("p/.agents/skills")).unwrap();
    reader.update_in(visual, |reader, _, cx| {
        reader.vault = publication(&root, true);
        cx.notify();
    });
    project.update_in(visual, |view, _, cx| {
        assert!(!finish(view, &ticket, Ok(old_result), cx));
    });
    visual
        .condition(&project, |view, _| {
            is_fresh(view) && view.skills.snapshot().unwrap().skills.is_empty()
        })
        .await;
    visual.run_until_parked();
    assert!(visual.debug_bounds("project-skill-0").is_none());
    let reload_button = visual.debug_bounds("refresh-project-skills").unwrap();
    visual.simulate_click(reload_button.center(), Modifiers::default());
    visual
        .condition(&project, |view, _| {
            is_fresh(view) && view.skills.snapshot().unwrap().skills.is_empty()
        })
        .await;
    project.read_with(visual, |view, _| {
        assert!(view.skills.reload_diagnostics().is_empty());
        assert!(view.gateway.is_none());
    });
}

#[gpui::test]
async fn skills_1591_hide_project_switch_and_same_owner_rebind_reject_held_reload(
    cx: &mut TestAppContext,
) {
    let state = tempfile::tempdir().unwrap();
    let (reader, project, visual) = mount(cx, state.path());
    visual.condition(&project, |view, _| is_fresh(view)).await;
    let root = state.path().join("vault");
    for action in ["hide", "rebind", "project"] {
        let (ticket, result) = project.update_in(visual, |view, _, cx| {
            let ticket = begin(view, cx).unwrap();
            let result = ticket.request.discover();
            (ticket, result)
        });
        project.update_in(visual, |view, _, cx| {
            match action {
                "hide" => view.select_section(ProjectSection::Manager, cx),
                "rebind" => view.bind_resources(
                    &reader,
                    root.clone(),
                    reader.read(cx).vault.clone(),
                    reader.read(cx).source_publication_epoch,
                    cx,
                ),
                _ => {
                    std::fs::create_dir_all(root.join("other")).unwrap();
                    view.area = Some(root.join("other"));
                    view.generation += 1;
                    view.bind_resources(
                        &reader,
                        root.clone(),
                        reader.read(cx).vault.clone(),
                        reader.read(cx).source_publication_epoch,
                        cx,
                    );
                    assert!(
                        view.skills.snapshot().is_none(),
                        "a new project cannot inherit lastgood rows"
                    );
                }
            }
            assert!(!finish(view, &ticket, Ok(result), cx));
            view.select_section(ProjectSection::Skills, cx);
            view.refresh_skills(cx);
        });
        visual.condition(&project, |view, _| is_fresh(view)).await;
        project.update_in(visual, |view, _, cx| {
            let fresh = begin(view, cx).unwrap();
            assert!(
                finish(view, &fresh, Ok(fresh.request.discover()), cx),
                "positive control: current owner applies"
            );
        });
    }
}

#[gpui::test]
async fn skills_1591_stale_open_refuses_rebind_and_fresh_reload_opens_exact_source(
    cx: &mut TestAppContext,
) {
    let state = tempfile::tempdir().unwrap();
    let (reader, project, visual) = mount(cx, state.path());
    visual.condition(&project, |view, _| is_fresh(view)).await;
    visual.run_until_parked();
    let opened = Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = opened.clone();
    let _subscription = project.update_in(visual, |_, window, cx| {
        cx.subscribe_in(
            &project,
            window,
            move |_, _, event: &ProjectSkillOpen, _, _| {
                capture.lock().unwrap().push(event.0.clone())
            },
        )
    });
    let root = state.path().join("vault");
    project.update_in(visual, |view, _, cx| {
        view.bind_resources(
            &reader,
            root.clone(),
            reader.read(cx).vault.clone(),
            reader.read(cx).source_publication_epoch,
            cx,
        );
        cx.notify();
    });
    visual.run_until_parked();
    let open = visual.debug_bounds("open-project-skill-0").unwrap();
    visual.simulate_click(open.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        opened.lock().unwrap().is_empty(),
        "same source paths/publication cannot revive a stale owner lease"
    );
    let reload_button = visual.debug_bounds("refresh-project-skills").unwrap();
    visual.simulate_click(reload_button.center(), Modifiers::default());
    visual.condition(&project, |view, _| is_fresh(view)).await;
    visual.run_until_parked();
    let open = visual.debug_bounds("open-project-skill-0").unwrap();
    visual.simulate_click(open.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        *opened.lock().unwrap(),
        vec![PathBuf::from("p/.agents/skills/card/SKILL.md")],
        "positive control: fresh exact source opens"
    );
    project.read_with(visual, |view, _| {
        assert!(view.records.is_empty());
        assert!(view.gateway.is_none());
    });
}

#[test]
fn skills_2938_rows_name_their_provider_and_note_differing_copies() {
    use okilum_core::agent_skills::{Counterpart, Provider, Skill};
    let mut skill = Skill {
        source: "p/.claude/skills/card/SKILL.md".into(),
        provider: Provider::Claude,
        name: "card".into(),
        description: String::new(),
        availability: Availability::Available,
        counterparts: Vec::new(),
    };
    assert_eq!(provider_line(&skill), ".claude (Claude Code)");
    assert_eq!(counterpart_line(&skill), None);
    skill.counterparts.push(Counterpart {
        source: "p/.agents/skills/card/SKILL.md".into(),
        provider: Provider::Agents,
        identical: false,
    });
    assert_eq!(
        counterpart_line(&skill).unwrap(),
        "Also in .agents: p/.agents/skills/card/SKILL.md (SKILL.md differs)"
    );
    skill.counterparts[0].identical = true;
    assert!(counterpart_line(&skill)
        .unwrap()
        .ends_with("(identical SKILL.md)"));
    skill.provider = Provider::Agents;
    assert_eq!(provider_line(&skill), ".agents (Codex and other agents)");
}

#[gpui::test]
async fn skills_2938_claude_skill_change_refreshes_list_and_opens_exact_source(
    cx: &mut TestAppContext,
) {
    let state = tempfile::tempdir().unwrap();
    let (_reader, project, visual) = mount(cx, state.path());
    visual.condition(&project, |view, _| is_fresh(view)).await;
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("project-skill-1").is_none(),
        "positive control: only the .agents copy before the change"
    );
    let opened = Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = opened.clone();
    let _subscription = project.update_in(visual, |_, window, cx| {
        cx.subscribe_in(
            &project,
            window,
            move |_, _, event: &ProjectSkillOpen, _, _| {
                capture.lock().unwrap().push(event.0.clone())
            },
        )
    });
    let root = state.path().join("vault");
    let claude = "p/.claude/skills/card/SKILL.md";
    std::fs::create_dir_all(root.join(claude).parent().unwrap()).unwrap();
    std::fs::write(
        root.join(claude),
        "---\nname: alpha\ndescription: Local fixture\n---\nClaude Code body\n",
    )
    .unwrap();
    // The watcher batch for a `.claude/skills` write drives the same refresh
    // as an `.agents/skills` write.
    project.update_in(visual, |view, _, cx| {
        view.resources_changed(
            &okilum_core::Changes {
                changed: [claude.to_owned()].into(),
                ..Default::default()
            },
            cx,
        );
    });
    visual
        .condition(&project, |view, _| {
            is_fresh(view) && view.skills.snapshot().unwrap().skills.len() == 2
        })
        .await;
    visual.run_until_parked();
    project.read_with(visual, |view, _| {
        let skills = &view.skills.snapshot().unwrap().skills;
        assert_eq!(skills[1].source, PathBuf::from(claude));
        assert_eq!(
            skills[1].provider,
            okilum_core::agent_skills::Provider::Claude
        );
        assert!(skills
            .iter()
            .all(|skill| skill.availability == Availability::Available));
        assert!(!skills[0].counterparts[0].identical);
    });
    for selector in [
        "project-skill-provider-0",
        "project-skill-provider-1",
        "project-skill-counterpart-0",
        "project-skill-counterpart-1",
    ] {
        assert!(visual.debug_bounds(selector).is_some(), "{selector}");
    }
    let open = visual.debug_bounds("open-project-skill-1").unwrap();
    visual.simulate_click(open.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(*opened.lock().unwrap(), vec![PathBuf::from(claude)]);
}
