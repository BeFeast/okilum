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
    /// Latest saved in-scope source; an offer exists only while connected.
    saved: okilum_t3::rules_refresh::RulesRevision,
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

    /// Whether a save of this absolute path belongs to the open project's
    /// rules scope. Pure path check on the UI thread; no I/O.
    pub(super) fn in_rules_scope(&self, source: &Path, cx: &App) -> bool {
        !self.single_file
            && reader_features::enabled(reader_features::Feature::Agents, cx)
            && self.project.as_ref().is_some_and(|project| {
                project
                    .read(cx)
                    .rules_source(&self.vault_root, source, "scope")
                    .is_some()
            })
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
        let Some(saved) = project
            .read(cx)
            .rules_source(&self.vault_root, source, revision)
        else {
            return;
        };
        // Connected: record the offer now. Offline: keep the saved source so
        // the notice explains why nothing can be sent yet.
        let offer = project.read(cx).rules_offer_for(&saved).ok();
        if let Some(ui) = &mut self.agents_offer.rules {
            if ui.root == self.vault_root && ui.project == project {
                if ui.saved == saved {
                    return;
                }
                ui.saved = saved;
                ui.status = None;
                if let Some(offer) = offer {
                    let _ = ui.refresh.saved(offer);
                }
                cx.notify();
                return;
            }
        }
        let mut refresh = okilum_t3::rules_refresh::RulesRefresh::default();
        if let Some(offer) = offer {
            if refresh.saved(offer).is_err() {
                return;
            }
        }
        self.agents_offer.rules = Some(RulesUi {
            lease: Arc::new(()),
            root: self.vault_root.clone(),
            project,
            refresh,
            saved,
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
        let readiness = ui.project.read(cx).rules_offer_for(&ui.saved);
        let acknowledged = ui
            .refresh
            .acknowledged()
            .is_some_and(|r| r.offer.source == ui.saved);
        let pending = ui.refresh.pending().is_some();
        let label = ui.status.clone().unwrap_or_else(|| {
            rules_notice(
                &ui.saved.path,
                readiness.as_ref().err().copied(),
                ui.refresh
                    .pending()
                    .is_some_and(|receipt| receipt.offer.source != ui.saved),
            )
        });
        let action = match readiness {
            _ if pending => Some(("Retry", "rules-refresh-send")),
            Ok(_) => Some(("Tell Manager", "rules-refresh-send")),
            Err(project::RulesUnready::Offline) => Some(("Connect", "rules-refresh-connect")),
            Err(project::RulesUnready::NoManager) => None,
        };
        h_flex()
            .id("rules-refresh-offer")
            .debug_selector(|| "rules-refresh-offer".into())
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .child(
                div()
                    .id("rules-refresh-label")
                    .debug_selector(|| "rules-refresh-label".into())
                    .flex_1()
                    .min_w_0()
                    .text_size(px(12.))
                    .whitespace_normal()
                    .child(label),
            )
            .when_some(action, |view, (label, id)| {
                view.child(
                    Button::new(id)
                        .debug_selector(move || id.into())
                        .ghost()
                        .small()
                        .label(label)
                        .disabled(ui.busy || (acknowledged && !pending))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.send_rules_offer(window, cx)),
                        ),
                )
            })
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
        let clicked = match ui.refresh.pending() {
            Some(receipt) => receipt.offer.clone(),
            None => match ui.project.read(cx).rules_offer_for(&ui.saved) {
                Ok(offer) => {
                    // A save made while offline becomes an offer once the
                    // Manager identity is confirmed.
                    let _ = ui.refresh.saved(offer.clone());
                    offer
                }
                Err(project::RulesUnready::Offline) => {
                    ui.status = None;
                    let project = ui.project.clone();
                    project.update(cx, |project, cx| project.connect_for_rules(window, cx));
                    cx.notify();
                    return;
                }
                Err(project::RulesUnready::NoManager) => return,
            },
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

/// Plain copy for the saved-rules notice. It never claims the Manager was told.
pub(super) fn rules_notice(
    path: &str,
    unready: Option<project::RulesUnready>,
    older_pending: bool,
) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    if older_pending {
        return "An earlier rules request is unconfirmed. Retry it before sending the latest saved rules.".into();
    }
    let start = format!("{name} saved. The running Manager reads its rules only when it starts.");
    match unready {
        None => format!("{start} Ask it to re-read them?"),
        Some(project::RulesUnready::Offline) => {
            format!(
                "{start} It is not connected, so it was not told. Connect to ask it to re-read."
            )
        }
        Some(project::RulesUnready::NoManager) => {
            format!("{start} This project has no Manager set up, so nobody was told.")
        }
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
