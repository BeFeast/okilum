//! Read-only Rules surface (#1590). Discovery runs on the background executor;
//! the owner forwards existing vault write batches. No timers/provider calls.
use super::*;
use okilum_core::agent_rules::{discover, Provenance, Refresh, RuleStack};

/// Open a vault-relative rule source in the Reader (#2970). The owning
/// ProjectView forwards it through its existing source-open path.
pub(crate) struct RuleSourceOpen(pub(crate) PathBuf);

pub(crate) struct RulesView {
    stack: RuleStack,
    opened: Option<usize>,
    refresh: Refresh,
    scroll: gpui::ScrollHandle,
    source_scroll: gpui::ScrollHandle,
}

impl RulesView {
    pub(crate) fn new(stack: RuleStack) -> Self {
        Self {
            stack,
            opened: None,
            refresh: Refresh::default(),
            scroll: gpui::ScrollHandle::new(),
            source_scroll: gpui::ScrollHandle::new(),
        }
    }

    /// Create only when Rules is enabled/active. No filesystem access on the UI.
    pub(crate) fn for_project(vault: PathBuf, project: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut view = Self::new(RuleStack::default());
        view.bind(vault, project, cx);
        view
    }

    /// Explicit owner scope change; immediately hide the previous project's rules.
    pub(crate) fn bind(&mut self, vault: PathBuf, project: PathBuf, cx: &mut Context<Self>) {
        self.stack = RuleStack::default();
        self.opened = None;
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.source_scroll.set_offset(point(px(0.), px(0.)));
        self.refresh.bind(vault, project);
        self.start_discovery(cx);
        cx.notify();
    }

    /// Forward the existing watcher's coalesced write/remove/directory/rescan
    /// batches, not raw notify read/access events. Arbitrary @import names and
    /// symlink targets make filename-only filtering unsafe here.
    pub(crate) fn external_changed(
        &mut self,
        changes: &okilum_core::watch::Changes,
        cx: &mut Context<Self>,
    ) {
        if !changes.is_empty() {
            self.reload(cx);
        }
    }

    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self.refresh.request();
        self.start_discovery(cx);
        cx.notify();
    }

    /// Call when Rules/Agents is disabled or its owner leaves this view. A running
    /// read may finish, but cannot install data or schedule another read.
    pub(crate) fn deactivate(&mut self, cx: &mut Context<Self>) {
        self.refresh.deactivate();
        self.stack = RuleStack::default();
        self.opened = None;
        cx.notify();
    }

    /// Compatibility hook for an owner-provided snapshot; supersedes worker reads.
    #[allow(dead_code, reason = "owner snapshot hook, not wired yet")]
    pub(crate) fn replace(&mut self, stack: RuleStack, cx: &mut Context<Self>) {
        self.refresh.supersede();
        self.install(stack);
        cx.notify();
    }

    fn install(&mut self, stack: RuleStack) {
        // Track the source identity rather than an index: creating/deleting a
        // preceding layer must never redirect an already-open rule to another file.
        self.opened = self.opened.and_then(|index| {
            let old = self.stack.layers.get(index)?;
            stack.layers.iter().position(|layer| {
                layer.path == old.path
                    && layer.provenance == old.provenance
                    && layer.source.is_some()
            })
        });
        self.stack = stack;
    }

    fn start_discovery(&mut self, cx: &mut Context<Self>) {
        let Some(ticket) = self.refresh.begin() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let job = ticket.clone();
            let stack = cx
                .background_executor()
                .spawn(async move { discover(&job.vault, &job.project) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.refresh.finish(&ticket) {
                    this.install(stack);
                }
                // At most one coalesced follow-up; no periodic reads or polling.
                this.start_discovery(cx);
                cx.notify();
            });
        })
        .detach();
    }
}

impl EventEmitter<RuleSourceOpen> for RulesView {}

impl Render for RulesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panel = v_flex()
            .id("project-rules")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .track_scroll(if self.opened.is_some() {
                &self.source_scroll
            } else {
                &self.scroll
            })
            .debug_selector(|| "rules-scroll-viewport".into())
            .gap_3()
            .p_3()
            .child(div().child("Rules"))
            .child(div().text_color(brand::palette(cx).text_muted).child(
                "Local instruction files. Remote agent loading is unknown. Tokens are approximate.",
            ));
        panel = panel.child(
            Button::new("rules-refresh")
                .ghost()
                .label("Refresh")
                .disabled(!self.refresh.is_bound())
                .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
        );
        if self.refresh.is_pending() {
            panel = panel.child(
                "Refreshing local rules; showing the previous snapshot until discovery finishes.",
            );
        }
        if let Some(index) = self.opened {
            if let Some(layer) = self.stack.layers.get(index) {
                panel = panel
                    .child(
                        Button::new("rules-back")
                            .ghost()
                            .label("Back to layers")
                            .debug_selector(|| "rules-back".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.opened = None;
                                cx.notify();
                            })),
                    )
                    .child(div().child(format!(
                        "{} · Read only · Discovery snapshot",
                        layer.path.display()
                    )))
                    .child(
                        div()
                            .debug_selector(|| "rules-source-snapshot".into())
                            .child(layer.source.clone().unwrap_or_default()),
                    );
                return panel.into_any_element();
            }
        }
        panel = panel.child(div().child(format!(
            "{} bytes · ≈ {} tokens · {} entries (totals count unique local instruction sources; repeated references and provider candidates excluded)",
            self.stack.total_bytes(), self.stack.approximate_tokens(), self.stack.layers.len()
        )));
        if self.stack.layers.is_empty() && !self.refresh.is_pending() {
            panel = panel.child("No ancestor instruction files found for this project.");
        }
        // Claude Code reads AGENTS.md only without any CLAUDE.md in the folder
        // chain (#2970); say which file switches that off and open it.
        for (index, (path, warning)) in self
            .stack
            .agents_fallback_warnings()
            .into_iter()
            .enumerate()
        {
            panel = panel.child(
                h_flex()
                    .id(("rules-claude-fallback", index))
                    .debug_selector(move || format!("rules-claude-fallback-{index}"))
                    .gap_2()
                    .flex_wrap()
                    .child(div().text_color(brand::palette(cx).warning).child(warning))
                    .child(
                        Button::new(("rules-claude-open", index))
                            .ghost()
                            .label(format!("Open {}", path.display()))
                            .debug_selector(move || format!("rules-claude-open-{index}"))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(RuleSourceOpen(path.clone()))
                            })),
                    ),
            );
        }
        for diagnostic in &self.stack.diagnostics {
            panel = panel.child(
                div()
                    .text_color(cx.theme().danger)
                    .child(diagnostic.clone()),
            );
        }
        for (index, layer) in self.stack.layers.iter().enumerate() {
            let provenance = match &layer.provenance {
                Provenance::Ancestor => "Ancestor / project instruction".to_owned(),
                Provenance::Import { from, line } => {
                    format!("Imported from {}:{line}", from.display())
                }
                Provenance::Alternative => "Provider candidate · order unknown".to_owned(),
            };
            let mut row = v_flex()
                .gap_1()
                .id(("rule-layer", index))
                .debug_selector(move || format!("rules-layer-{index}"))
                .child(div().child(format!("{} · {}", index + 1, layer.path.display())))
                .child(
                    div()
                        .text_color(brand::palette(cx).text_muted)
                        .child(provenance),
                )
                .child(div().child(format!(
                    "{} bytes · ≈ {} tokens",
                    layer.bytes(),
                    layer.approximate_tokens()
                )));
            if !layer.duplicate_lines.is_empty() {
                row = row.child(div().child(format!(
                        "Repeated text across layers at lines {}",
                        layer
                            .duplicate_lines
                            .iter()
                            .map(usize::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
            }
            if let Some(error) = &layer.error {
                row = row.child(div().text_color(cx.theme().danger).child(error.clone()));
            }
            for hint in &layer.similar_lines {
                let peer = &self.stack.layers[hint.other_layer];
                row = row.child(div().child(format!(
                    "Similar wording: line {} ↔ {}:{} (review wording)",
                    hint.line,
                    peer.path.display(),
                    hint.other_line
                )));
            }
            row = row.child(
                Button::new(("rule-open", index))
                    .ghost()
                    .label("Open read only")
                    .debug_selector(move || format!("rules-open-{index}"))
                    .disabled(layer.source.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.opened = Some(index);
                        this.source_scroll.set_offset(point(px(0.), px(0.)));
                        cx.notify();
                    })),
            );
            panel = panel.child(row);
        }
        panel.into_any_element()
    }
}

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
                view.bind_resources(
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
