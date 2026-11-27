//! Reader-coupled glue and Reader-integration tests for `okilum-shell-project`
//! (#3237, epic #3241). All non-Reader Project/Board/Manager/Records code lives
//! in `crates/okilum-shell-project`.
use super::*;
pub(crate) use okilum_shell_project::project::*;

pub(crate) mod agent_rules;
#[cfg(test)]
pub(crate) mod agent_skills;
pub(crate) mod board;
#[cfg(native_manager)]
pub(crate) mod links;
#[cfg(all(target_os = "linux", any(test, feature = "project-ui-harness")))]
pub(crate) mod performance;
pub(crate) mod records;
#[cfg(test)]
pub(crate) mod rules_reread;

#[cfg(all(test, native_manager))]
mod panel_tests;

#[cfg(test)]
pub(crate) fn bind_reader_resources(
    view: &mut ProjectView,
    reader: &Entity<Reader>,
    root: PathBuf,
    mut published: Arc<Vault>,
    mut publication_epoch: u64,
    cx: &mut Context<ProjectView>,
) {
    let weak_reader = reader.downgrade();
    let source_root = root.clone();
    let source: okilum_shell_project::project::agent_skills::PublicationSource =
        Arc::new(move |cx: &App| {
            let reader = weak_reader.upgrade().ok_or("Project Reader is closed")?;
            let reader = reader.read(cx);
            if reader.vault_root != source_root || reader.vault.root != source_root {
                return Err("Project inventory belongs to a different vault".into());
            }
            Ok((reader.vault.clone(), reader.source_publication_epoch))
        });
    let observe_root = root.clone();
    let observer = cx.observe(reader, move |this, reader, cx| {
        let reader = reader.read(cx);
        if reader.vault_root != observe_root
            || (Arc::ptr_eq(&published, &reader.vault)
                && publication_epoch == reader.source_publication_epoch)
        {
            return;
        }
        published = reader.vault.clone();
        publication_epoch = reader.source_publication_epoch;
        this.on_vault_publication(cx);
    });
    view.bind_resources(root, source, observer, cx);
}

#[cfg(test)]
mod tests;

impl Reader {
    pub(super) fn refresh_project_resources(
        &mut self,
        changes: &okilum_core::Changes,
        cx: &mut Context<Self>,
    ) {
        if !changes.is_empty() {
            for records in self.project_navigation.records.values() {
                records.update(cx, |records, cx| records.changed(cx));
            }
        }
        if let Some(project) = self.project.as_ref().filter(|_| self.project_open) {
            project.update(cx, |view, cx| view.resources_changed(changes, cx));
        }
    }

    pub(super) fn close_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.project_request = self.project_request.wrapping_add(1);
        if let Some(project) = self.project.as_ref().filter(|_| self.project_open) {
            project.update(cx, |project, cx| project.set_active(false, window, cx));
        }
        self.project_open = false;
    }

    #[cfg(test)]
    pub(super) fn toggle_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_open {
            self.close_project(window, cx);
            cx.notify();
            return;
        }
        let note = self.vault_root.join(&self.pane.current_rel);
        let area = connections::area_for(&self.vault_root, &note)
            .unwrap_or_else(|_| self.vault_root.clone());
        self.open_project_area(area, window, cx);
    }

    pub(super) fn open_project_area(
        &mut self,
        area: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
        if let Some(previous) = &self.project {
            previous.update(cx, |view, cx| view.set_active(false, window, cx));
        }
        let opened = std::time::Instant::now();
        self.project_open = true;
        self.project = None;
        self.project_request = self.project_request.wrapping_add(1);
        let request = self.project_request;
        let unavailable_manager = self
            .project_navigation
            .statuses
            .get(&area)
            .filter(|status| status.restartable())
            .and_then(|_| self.project_navigation.status_managers.get(&area))
            .cloned();
        cx.spawn_in(window, async move |this, cx| {
            // Re-read the explicit pointer on every activation. A rotation gets
            // its own view; returning to an existing binding retains its draft.
            let binding = cx
                .background_executor()
                .spawn(async move {
                    let project = okilum_t3::projects::Project::read(&area)?;
                    let mut binding = project.native_binding()?;
                    if unavailable_manager.as_ref() == Some(&project.manager) {
                        if let Some(binding) = &mut binding {
                            binding.manager.thread_id = None;
                        }
                    }
                    let key = (area, serde_json::to_string(&binding)?);
                    Ok::<_, anyhow::Error>((key, binding))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.project_request != request || !this.project_open {
                    return;
                }
                let project = match binding {
                    Ok((key, binding)) => match this.project_views.get(&key).cloned() {
                        Some(project) => {
                            project.update(cx, |project, cx| {
                                project.set_active_since(true, opened, window, cx)
                            });
                            project
                        }
                        None => {
                            let state = this.session_directory.clone();
                            let project = cx.new(|cx| {
                                let mut view = ProjectView::new(
                                    binding,
                                    state,
                                    key.0.clone(),
                                    opened,
                                    window,
                                    cx,
                                );
                                view.context_path = key
                                    .0
                                    .strip_prefix(&this.vault_root)
                                    .unwrap_or(&key.0)
                                    .to_string_lossy()
                                    .replace('\\', "/");
                                view
                            });
                            #[cfg(native_manager)]
                            this.connect_manager_links(&project, window, cx);
                            #[cfg(native_manager)]
                            cx.subscribe_in(
                                &project,
                                window,
                                |this, _, _: &NativeConnected, window, cx| {
                                    // Discard pre-pairing snapshots before refreshing
                                    // project status and icon gateways with the saved bearer.
                                    this.project_navigation.gateway_generation =
                                        this.project_navigation.gateway_generation.wrapping_add(1);
                                    this.project_navigation.t3_gateways.clear();
                                    this.refresh_project_statuses(window, cx);
                                },
                            )
                            .detach();
                            cx.subscribe_in(
                                &project,
                                window,
                                |this, _, _: &ManagerStarted, window, cx| {
                                    if let Some(active) = this.project_navigation.active.clone() {
                                        this.enter_vault_project(active, window, cx);
                                    }
                                },
                            )
                            .detach();
                            this.project_views.insert(key, project.clone());
                            project
                        }
                    },
                    Err(error) => cx.new(|cx| {
                        let mut view =
                            ProjectView::empty(this.session_directory.clone(), window, cx);
                        view.reason = None;
                        view.error = Some(format!("{error:#}"));
                        view
                    }),
                };
                let root = this.vault_root.clone();
                let mut published = this.vault.clone();
                let mut publication_epoch = this.source_publication_epoch;
                let reader = cx.entity();
                let weak_reader = reader.downgrade();
                let source_root = root.clone();
                let source: okilum_shell_project::project::agent_skills::PublicationSource =
                    Arc::new(move |cx: &App| {
                        let reader = weak_reader.upgrade().ok_or("Project Reader is closed")?;
                        let reader = reader.read(cx);
                        if reader.vault_root != source_root || reader.vault.root != source_root {
                            return Err("Project inventory belongs to a different vault".into());
                        }
                        Ok((reader.vault.clone(), reader.source_publication_epoch))
                    });
                project.update(cx, |view, cx| {
                    let observe_root = root.clone();
                    let observer = cx.observe(&reader, move |this, reader, cx| {
                        let reader = reader.read(cx);
                        if reader.vault_root != observe_root
                            || (Arc::ptr_eq(&published, &reader.vault)
                                && publication_epoch == reader.source_publication_epoch)
                        {
                            return;
                        }
                        published = reader.vault.clone();
                        publication_epoch = reader.source_publication_epoch;
                        this.on_vault_publication(cx);
                    });
                    view.bind_resources(root, source, observer, cx);
                });
                let _resource_open = cx.subscribe_in(
                    &project,
                    window,
                    |this, project, request: &ProjectSkillOpen, window, cx| {
                        if this.project.as_ref() != Some(project)
                            || !this.project_open
                            || !reader_features::enabled(reader_features::Feature::Agents, cx)
                        {
                            return;
                        }
                        let relative = okilum_core::vault::note_path(&request.0);
                        this.open_note_landing(&relative, None, None, None, window, cx);
                    },
                );
                project.update(cx, |view, _| view._resource_open = Some(_resource_open));
                this.connect_project_board(project.clone(), window, cx);
                this.project = Some(project);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

/// Repeatable native screenshots of the production Reader-mounted Manager.
/// Only the explicit harness feature/mode enables this synthetic local vault.
#[cfg(feature = "project-ui-harness")]
fn open_mounted_header(
    state: PathBuf,
    gateway: Arc<okilum_shell_project::project::fixture::Fake>,
    cx: &mut App,
) {
    let vault = state.join("mounted-header-vault");
    std::fs::create_dir_all(vault.join("ops")).expect("fixture vault");
    std::fs::write(vault.join("ops/project.toml"), "kind = 'knowledge'\n")
        .expect("fixture project");
    std::fs::write(
        vault.join("home.md"),
        "# Project notes\n\nThe Manager stays beside your notes.\n",
    )
    .expect("fixture note");
    #[cfg(target_os = "linux")]
    let performance =
        std::env::var("OKILUM_PROJECT_UI_FIXTURE").is_ok_and(|mode| mode == "performance");
    let width = match std::env::var("OKILUM_HEADER_QA_WIDTH").as_deref() {
        Ok("320") => 320.,
        _ => 560.,
    };
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1200.), px(800.)),
                cx,
            ))),
            ..Default::default()
        },
        move |window, cx| {
            window.set_window_title("Okilum — Mounted Manager fixture");
            let reader = cx.new(|cx| {
                let mut reader = Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("home.md".into()),
                        index_dir: Some(state.join("index")),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                let project = cx.new(|cx| {
                    let mut project =
                        okilum_shell_project::project::fixture::view(state, gateway, window, cx);
                    project.identity = Some(okilum_t3::ServerIdentity {
                        environment_id: "environment:fixture".into(),
                        label: "Fixture".into(),
                    });
                    project
                });
                reader.project_navigation.active =
                    Some(okilum_t3::projects::Project::read(&vault).expect("fixture project"));
                #[cfg(target_os = "linux")]
                if performance {
                    performance::populate(&project, cx);
                    let board =
                        cx.new(|cx| board::Board::performance_fixture(vault.clone(), window, cx));
                    cx.observe(&board, |reader, _, cx| reader.project_board_changed(cx))
                        .detach();
                    reader
                        .project_navigation
                        .boards
                        .insert(vault.clone(), board);
                    reader.project_navigation.board_visible = true;
                }
                reader.project = Some(project);
                reader.project_open = true;
                reader.panels.close(reader_layout::Panel::Notes);
                reader.panels.open(reader_layout::Panel::Backlinks);
                reader.panel_widths.backlinks = width;
                reader
            });
            cx.new(|cx| Root::new(reader, window, cx))
        },
    )
    .expect("mounted Manager fixture window");
}

#[cfg(feature = "project-ui-harness")]
pub(crate) fn open_harness(cx: &mut App) -> bool {
    okilum_shell_project::project::fixture::open_with_mounted_header(open_mounted_header, cx)
}
