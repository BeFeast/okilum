#[cfg(test)]
pub(crate) use okilum_shell_project::project::agent_rules::*;

#[cfg(test)]
use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn rules_1590_mounted_layers_scroll_and_open_at_last_layer(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let temp = fixture::canonical_tempdir();
        let vault = temp.path().join("vault");
        std::fs::create_dir_all(vault.join("ops")).unwrap();
        std::fs::create_dir_all(vault.join("instructions")).unwrap();
        std::fs::write(vault.join("ops/project.toml"), "kind = 'knowledge'\n").unwrap();
        std::fs::write(vault.join("home.md"), "# Home\n").unwrap();
        let mut imports = String::new();
        for index in 0..8 {
            let name = format!("instructions/layer-{index}-with-a-distinct-name.md");
            imports.push_str(&format!("@{name}\n"));
            std::fs::write(
                vault.join(name),
                format!("# UniqueRule{index}\nThisLayerHasUniqueRuleNumber{index}\n"),
            )
            .unwrap();
        }
        std::fs::write(vault.join("AGENTS.md"), imports).unwrap();
        let stack = discover(&vault, &vault);
        assert_eq!(stack.layers.len(), 9);
        let last = stack.layers.len() - 1;
        let original = std::fs::read(vault.join(&stack.layers[last].path)).unwrap();
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
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(1400.), px(950.)));
        visual.run_until_parked();
        let mut rules = None;
        reader.update_in(visual, |reader, window, cx| {
            let rule_view = cx.new(|_| RulesView::new(stack));
            rules = Some(rule_view.clone());
            let project = cx.new(|cx| {
                let mut view = fixture::view(
                    temp.path().join("fixture"),
                    Arc::new(fixture::Fake::default()),
                    window,
                    cx,
                );
                view.active = false;
                view.rules = Some(rule_view);
                view.section = ProjectSection::Rules;
                view
            });
            reader.project_navigation.active =
                Some(okilum_t3::projects::Project::read(&vault).unwrap());
            reader.project = Some(project);
            reader.project_open = true;
            reader.panels.close(reader_layout::Panel::Notes);
            reader.panels.open(reader_layout::Panel::Backlinks);
            reader.panel_widths.backlinks = 440.;
            cx.notify();
        });
        visual.run_until_parked();
        let rules = rules.unwrap();
        let viewport = visual
            .debug_bounds("rules-scroll-viewport")
            .expect("mounted Rules viewport");
        let panel = visual.debug_bounds("reader-backlinks-panel").unwrap();
        assert!(
            viewport.size.height > px(200.) && viewport.bottom() <= panel.bottom(),
            "Rules must have a finite height inside the actual panel: {viewport:?}, {panel:?}"
        );
        // Nine-layer fixture; debug_bounds requires a static selector.
        let selector = "rules-open-8";
        assert!(
            visual
                .debug_bounds(selector)
                .is_none_or(|bounds| bounds.bottom() > viewport.bottom()),
            "positive control: last layer needs scrolling"
        );
        let header_top = visual.debug_bounds("project-chat-header").unwrap().top();
        visual.simulate_event(ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-10_000.))),
            ..Default::default()
        });
        visual.run_until_parked();
        assert!(
            rules.read_with(visual, |rules, _| rules.scroll.offset().y) < px(0.),
            "real wheel input scrolls the Rules list"
        );
        // Native Manager chrome reuses its scene on scroll. GPUI's frame debug
        // map does not replay cached selectors; redraw before measuring it.
        visual.update(|window, _| window.refresh());
        visual.run_until_parked();
        assert_eq!(
            visual.debug_bounds("project-chat-header").unwrap().top(),
            header_top
        );
        let button = visual
            .debug_bounds(selector)
            .expect("last layer is reachable after scrolling");
        assert!(button.top() >= viewport.top() && button.bottom() <= viewport.bottom());
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("rules-source-snapshot").is_some());
        rules.read_with(visual, |rules, _| {
            assert_eq!(rules.opened, Some(last));
            assert_eq!(
                rules.stack.layers[last].source.as_ref().unwrap().as_bytes(),
                original.as_slice()
            );
        });
        let back = visual.debug_bounds("rules-back").unwrap();
        visual.simulate_click(back.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(
            rules.read_with(visual, |rules, _| rules.scroll.offset().y) < px(0.),
            "returning from a snapshot retains the list position"
        );
        assert_eq!(
            std::fs::read(vault.join("instructions/layer-7-with-a-distinct-name.md")).unwrap(),
            original
        );
    }

    /// #2970: a CLAUDE.md in the project folder chain shows one warning line
    /// whose link opens that exact file through the Project source-open path.
    #[gpui::test]
    async fn issue2970_claude_md_warning_opens_the_file(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_features::enable_test_modules(cx);
        });
        let state = tempfile::tempdir().unwrap();
        let root = state.path().join("vault");
        std::fs::create_dir_all(root.join("p")).unwrap();
        std::fs::write(root.join("p/AGENTS.md"), "# Project rules\n").unwrap();
        let mut entities = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                let mut reader = Reader::new(
                    Opts {
                        defer_loading: true,
                        session_directory: Some(state.path().join("state")),
                        panel_settings_override: Some(state.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                reader.vault_root = root.clone();
                reader
            });
            let project = cx.new(|cx| {
                let mut view = ProjectView::empty(Some(state.path().join("state")), window, cx);
                view.area = Some(root.join("p"));
                crate::project::bind_reader_resources(
                    &mut view,
                    &reader,
                    root.clone(),
                    reader.read(cx).vault.clone(),
                    reader.read(cx).source_publication_epoch,
                    cx,
                );
                view.select_section(ProjectSection::Rules, cx);
                view
            });
            entities = Some(project.clone());
            Root::new(project, window, cx)
        });
        let project = entities.unwrap();
        visual.simulate_resize(size(px(1200.), px(900.)));
        visual
            .condition(&project, |view, cx| {
                view.rules.as_ref().is_some_and(|rules| {
                    let rules = rules.read(cx);
                    !rules.refresh.is_pending() && !rules.stack.layers.is_empty()
                })
            })
            .await;
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("rules-layer-0").is_some(),
            "positive control: the AGENTS.md layer is painted"
        );
        assert!(
            visual.debug_bounds("rules-claude-fallback-0").is_none(),
            "control: no warning without a CLAUDE.md"
        );
        std::fs::write(root.join("p/CLAUDE.md"), "Claude-only rules\n").unwrap();
        let rules = project.read_with(visual, |view, _| view.rules.clone().unwrap());
        rules.update(visual, |rules, cx| rules.reload(cx));
        visual
            .condition(&rules, |rules, _| {
                !rules.refresh.is_pending() && !rules.stack.claude_md.is_empty()
            })
            .await;
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
        assert!(visual.debug_bounds("rules-claude-fallback-0").is_some());
        assert!(visual.debug_bounds("rules-claude-fallback-1").is_none());
        rules.read_with(visual, |rules, _| {
            assert_eq!(
                rules.stack.agents_fallback_warnings(),
                [(
                    PathBuf::from("p/CLAUDE.md"),
                    "CLAUDE.md in p switches off AGENTS.md for Claude Code".to_owned()
                )]
            );
        });
        let open = visual.debug_bounds("rules-claude-open-0").unwrap();
        visual.simulate_click(open.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(*opened.lock().unwrap(), [PathBuf::from("p/CLAUDE.md")]);
        assert_eq!(
            std::fs::read_to_string(root.join("p/CLAUDE.md")).unwrap(),
            "Claude-only rules\n"
        );
    }
}
