//! Contextual, folder-scoped Agents opt-in. Relevance never contacts T3.
use super::*;

#[derive(Default)]
pub(super) struct Offer {
    root: Option<PathBuf>,
    relevant: bool,
    busy: bool,
    generation: u64,
    rules: Option<RulesUi>,
}

struct RulesUi {
    // A returning worker belongs to this mounted offer, not a later reopen of
    // the same vault/project that happens to reuse its ProjectView entity.
    lease: Arc<()>,
    root: PathBuf,
    project: Entity<project::ProjectView>,
    refresh: okilum_t3::rules_refresh::RulesRefresh,
    busy: bool,
    status: Option<String>,
}

impl Reader {
    pub(super) fn refresh_agents_offer(&mut self, cx: &mut Context<Self>) {
        if self.single_file
            || reader_features::enabled(reader_features::Feature::Agents, cx)
            || self.sidebar.agents_offer_answered
            || self.agents_offer.root.as_ref() == Some(&self.vault_root)
        {
            return;
        }
        let root = self.vault_root.clone();
        let generation = self.agents_offer.generation.wrapping_add(1);
        let rules = self.agents_offer.rules.take();
        self.agents_offer = Offer {
            rules,
            root: Some(root.clone()),
            generation,
            ..Default::default()
        };
        cx.spawn(async move |this, cx| {
            let folder = root.clone();
            let relevant = cx
                .background_executor()
                .spawn(async move {
                    reader_agents_context::project_folder(&folder)
                        || reader_agents_context::local_t3_running()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.vault_root == root
                    && this.agents_offer.root.as_ref() == Some(&root)
                    && this.agents_offer.generation == generation
                {
                    this.agents_offer.relevant = relevant;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn agents_offer_slot(&self, cx: &App) -> bool {
        self.has_rules_offer(cx)
            || (!self.single_file
                && !reader_features::enabled(reader_features::Feature::Agents, cx))
    }

    pub(super) fn show_agents_offer(&self, cx: &App) -> bool {
        self.has_rules_offer(cx)
            || (self.agents_offer_slot(cx)
                && !self.sidebar.agents_offer_answered
                && self.agents_offer.root.as_ref() == Some(&self.vault_root)
                && self.agents_offer.relevant)
    }

    pub(super) fn render_agents_offer(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.has_rules_offer(cx) {
            return self.render_rules_offer(cx);
        }
        h_flex()
            .id("agents-offer")
            .debug_selector(|| "agents-offer".into())
            .w_full()
            .h(px(32.))
            .px_2()
            .gap_1()
            .items_center()
            .bg(cx.theme().sidebar)
            .child(
                div()
                    .id("agents-offer-label")
                    .flex_1()
                    .min_w_0()
                    .text_size(px(11.))
                    .truncate()
                    .child("Board + Manager?")
                    .tooltip(|window, cx| {
                        gpui_component::tooltip::Tooltip::new(
                            "This looks like a project. Enable Board and Manager?",
                        )
                        .build(window, cx)
                    }),
            )
            .child(
                Button::new("agents-offer-enable")
                    .debug_selector(|| "agents-offer-enable".into())
                    .ghost()
                    .small()
                    .label("Enable")
                    .disabled(self.agents_offer.busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.enable_agents_offer(window, cx)),
                    ),
            )
            .child(
                Button::new("agents-offer-dismiss")
                    .debug_selector(|| "agents-offer-dismiss".into())
                    .ghost()
                    .small()
                    .label("Not now")
                    .disabled(self.agents_offer.busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar.agents_offer_answered = true;
                        this.save_sidebar(cx);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// Parent save completion hook: pass the successfully saved absolute path
    /// and revision receipt. Never call for buffer changes, conflicts, external
    /// refreshes, unchanged saves or failed saves. No network/I/O occurs here.
    pub(super) fn agents_rules_saved(
        &mut self,
        source: &Path,
        revision: &str,
        cx: &mut Context<Self>,
    ) {
        if self.single_file || !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
        let Some(project) = self.project.clone() else {
            return;
        };
        let Some(offer) = project
            .read(cx)
            .saved_rules_offer(&self.vault_root, source, revision)
        else {
            return;
        };
        if let Some(ui) = &mut self.agents_offer.rules {
            if ui.root == self.vault_root && ui.project == project {
                match ui.refresh.saved(offer) {
                    Ok(true) => ui.status = None,
                    _ => return,
                }
                cx.notify();
                return;
            }
        }
        let mut refresh = okilum_t3::rules_refresh::RulesRefresh::default();
        if refresh.saved(offer).is_err() {
            return;
        }
        self.agents_offer.rules = Some(RulesUi {
            lease: Arc::new(()),
            root: self.vault_root.clone(),
            project,
            refresh,
            busy: false,
            status: None,
        });
        cx.notify();
    }

    fn has_rules_offer(&self, cx: &App) -> bool {
        !self.single_file
            && reader_features::enabled(reader_features::Feature::Agents, cx)
            && self.agents_offer.rules.as_ref().is_some_and(|ui| {
                ui.root == self.vault_root && self.project.as_ref() == Some(&ui.project)
            })
    }

    /// Mount in the Reader-owned project layout, outside the sidebar (the
    /// sidebar is intentionally absent for an active project). Returns no node
    /// for a different vault/project or when Agents is disabled.
    pub(super) fn render_agents_rules_offer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.has_rules_offer(cx)
            .then(|| self.render_rules_offer(cx))
    }

    fn render_rules_offer(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = self
            .agents_offer
            .rules
            .as_ref()
            .expect("visible rules offer");
        let acknowledged = ui
            .refresh
            .acknowledged()
            .is_some_and(|r| Some(&r.offer) == ui.refresh.offer());
        let label = ui.status.clone().unwrap_or_else(|| {
            if ui.refresh.pending().is_some_and(|receipt| Some(&receipt.offer) != ui.refresh.offer()) {
                "An earlier rules request is unconfirmed. Retry it before sending the latest saved rules.".into()
            } else {
                "AGENTS.md saved. Ask Manager to read the updated rules?".into()
            }
        });
        h_flex()
            .id("rules-refresh-offer")
            .w_full()
            .gap_1()
            .items_center()
            .child(div().flex_1().min_w_0().child(label))
            .child(
                Button::new("rules-refresh-send")
                    .ghost()
                    .small()
                    .label(if ui.refresh.pending().is_some() {
                        "Retry"
                    } else {
                        "Tell Manager"
                    })
                    .disabled(ui.busy || acknowledged)
                    .on_click(cx.listener(|this, _, window, cx| this.send_rules_offer(window, cx))),
            )
            .into_any_element()
    }

    fn send_rules_offer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_rules_offer(cx) {
            return;
        }
        let ui = self
            .agents_offer
            .rules
            .as_mut()
            .expect("visible rules offer");
        if ui.busy {
            return;
        }
        // Resolve the original ambiguous operation first, even if another
        // source revision was saved meanwhile. Never retry with new contents.
        let clicked = ui
            .refresh
            .pending()
            .map(|r| r.offer.clone())
            .or_else(|| ui.refresh.offer().cloned());
        let Some(clicked) = clicked else {
            return;
        };
        let job = match ui
            .project
            .read(cx)
            .rules_send_job(&ui.refresh, &clicked, cx)
        {
            Ok(job) => job,
            Err(error) => {
                ui.status = Some(error.into());
                cx.notify();
                return;
            }
        };
        if let Err(error) = ui.refresh.begin_delivery(&clicked) {
            ui.status = Some(error.into());
            cx.notify();
            return;
        }
        ui.busy = true;
        let root = ui.root.clone();
        let project = ui.project.clone();
        let lease = ui.lease.clone();
        cx.spawn_in(window, async move |this, cx| {
            let delivery = cx.background_executor().spawn(async move { job() }).await;
            let _ = this.update_in(cx, |this, _window, cx| {
                let Some(ui) = this.agents_offer.rules.as_mut() else {
                    return;
                };
                if ui.root != root || ui.project != project || !Arc::ptr_eq(&ui.lease, &lease) {
                    return;
                }
                ui.busy = false;
                // Apply only the complete captured durable identity. This also
                // settles a known old pending request after a newer source save.
                if let Some(receipt) = &delivery.receipt {
                    ui.refresh.record_delivery(receipt, delivery.acknowledged);
                }
                ui.refresh.end_delivery(&clicked);
                if ui.refresh.offer() != Some(&clicked) {
                    cx.notify();
                    return;
                }
                ui.status = Some(match delivery.error {
                    Some(error) => {
                        format!("{error}. Delivery is unconfirmed; retry the retained request.")
                    }
                    None => {
                        "Manager accepted the message. Rules reload is not yet confirmed.".into()
                    }
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn enable_agents_offer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.show_agents_offer(cx) || self.agents_offer.busy {
            return;
        }
        reader_features::set_enabled(reader_features::Feature::Agents, true, cx);
        if !reader_features::enabled(reader_features::Feature::Agents, cx) {
            reader_toast::error(
                "Could not enable Agents. Check Settings → Features.",
                window,
                cx,
            );
            return;
        }
        self.sidebar.agents_offer_answered = true;
        self.save_sidebar(cx);
        self.agents_offer.busy = true;
        let root = self.vault_root.clone();
        let generation = self.agents_offer.generation;
        cx.spawn_in(window, async move |this, cx| {
            let folder = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move { reader_agents_context::open_local_board(&folder) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.vault_root != root || this.agents_offer.generation != generation {
                    return;
                }
                this.agents_offer.busy = false;
                match result {
                    Ok(project)
                        if reader_features::enabled(reader_features::Feature::Agents, cx) =>
                    {
                        this.enter_vault_project(project, window, cx)
                    }
                    Ok(_) => reader_toast::transient(
                        "Board saved locally. Enable Agents in Settings → Features to open it.",
                        window,
                        cx,
                    ),
                    Err(error) => reader_toast::error(
                        format!("Could not open the local Board: {error:#}"),
                        window,
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn offer_keeps_tree_geometry_and_answers_survive_reopen(cx: &mut TestAppContext) {
        for accept in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("checkout");
            let state = temp.path().join("state");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join(".git"), "gitdir: /unused-worktree-control").unwrap();
            let source = b"# Note\r\nKeep exact bytes.\r\n";
            std::fs::write(root.join("note.md"), source).unwrap();
            cx.update(|cx| {
                crate::test_init(cx);
                reader_features::install(&state, cx);
            });
            let mut entity = None;
            let (_, visual) = cx.add_window_view(|window, cx| {
                let view = cx.new(|cx| {
                    Reader::new(
                        Opts {
                            defer_loading: true,
                            vault: Some(root.clone()),
                            session_directory: Some(state.clone()),
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                });
                view.update(cx, |view, cx| {
                    view.loading = None;
                    view.vault_root = root.clone();
                    view.vault = Arc::new(Vault::from_note_paths(["note.md".into()]));
                    view.panels.open(reader_layout::Panel::Notes);
                    view.sync_tree();
                    view.load_sidebar_state();
                    // Hold the result so the test observes the exact arrival boundary.
                    view.agents_offer = Offer {
                        root: Some(root.clone()),
                        ..Default::default()
                    };
                    cx.notify();
                });
                entity = Some(view.clone());
                Root::new(view, window, cx)
            });
            visual.run_until_parked();
            assert!(visual.debug_bounds("agents-offer").is_none());
            let before = visual.debug_bounds("sidebar-tree-body").unwrap();
            let reader = entity.unwrap();
            reader.update(visual, |reader, cx| {
                reader.agents_offer.relevant = reader_agents_context::project_folder(&root);
                cx.notify();
            });
            visual.run_until_parked();
            assert!(
                visual.debug_bounds("agents-offer").is_some(),
                "checkout positive control"
            );
            assert_eq!(
                visual.debug_bounds("sidebar-tree-body"),
                Some(before),
                "arrival cannot move the tree"
            );
            assert!(
                !root.join("ops").exists(),
                "an offer never writes project metadata"
            );
            assert!(!reader.read_with(visual, |_, cx| reader_features::enabled(
                reader_features::Feature::Agents,
                cx
            )));
            let button = visual
                .debug_bounds(if accept {
                    "agents-offer-enable"
                } else {
                    "agents-offer-dismiss"
                })
                .unwrap();
            visual.simulate_click(button.center(), Modifiers::default());
            visual.run_until_parked();
            let saved = reader_sidebar::State::load(&reader_sidebar::State::path(&state, &root));
            assert!(saved.agents_offer_answered);
            assert!(visual.debug_bounds("agents-offer").is_none());
            assert_eq!(std::fs::read(root.join("note.md")).unwrap(), source);
            assert_eq!(
                reader.read_with(visual, |_, cx| reader_features::enabled(
                    reader_features::Feature::Agents,
                    cx
                )),
                accept
            );
            if accept {
                assert!(root.join(".okilum/board.toml").is_file());
                assert!(
                    reader.read_with(visual, |reader, _| reader.project_navigation.board_visible)
                );
            } else {
                assert!(!root.join("ops").exists());
                assert_eq!(
                    visual.debug_bounds("sidebar-tree-body"),
                    Some(before),
                    "dismissal cannot move the tree"
                );
            }
            reader.update(visual, |reader, cx| {
                reader.sidebar = saved;
                cx.notify();
            });
            visual.run_until_parked();
            assert!(
                visual.debug_bounds("agents-offer").is_none(),
                "one answer per folder"
            );
            assert!(
                !reader_sidebar::State::load(&reader_sidebar::State::path(
                    &state,
                    &temp.path().join("another-folder")
                ))
                .agents_offer_answered,
                "answer is not shared with a different folder"
            );
        }
    }
}
