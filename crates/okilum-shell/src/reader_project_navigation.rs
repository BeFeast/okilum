//! Projects is an application section; entering one temporarily scopes the vault.
use super::*;
use okilum_t3::projects::{Discovery, Project};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Section {
    #[default]
    Reader,
    Projects,
    Inbox,
    Logs,
}

#[derive(Default)]
pub(super) struct State {
    pub section: Section,
    pub discovery: Discovery,
    pub status_managers: std::collections::BTreeMap<PathBuf, okilum_t3::projects::Manager>,
    pub statuses: std::collections::BTreeMap<PathBuf, okilum_t3::projects::Status>,
    pub loading: bool,
    pub generation: u64,
    pub active: Option<Project>,
    saved: Option<Saved>,
    pub manager_panel: Option<Entity<reader_manager_panel::ManagerPanel>>,
}

struct Saved {
    tree: reader_tree::Tree,
    panels: reader_layout::Panels,
    widths: reader_layout::Widths,
    tree_offset: Point<Pixels>,
}

impl Reader {
    pub(super) fn reset_vault_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.project_navigation.manager_panel.take() {
            panel.update(cx, |panel, cx| panel.hide(cx));
        }
        self.close_project(window, cx);
        let generation = self.project_navigation.generation.wrapping_add(1);
        self.project_navigation = State {
            generation,
            ..Default::default()
        };
        self.tree_sticky.clear();
    }

    pub(super) fn show_app_section(
        &mut self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.project_navigation.section = section;
        self.project_navigation.generation = self.project_navigation.generation.wrapping_add(1);
        self.project_navigation.loading = false;
        if section == Section::Projects {
            let generation = self.project_navigation.generation;
            self.project_navigation.loading = true;
            let vault = self.vault_root.clone();
            cx.spawn_in(window, async move |this, cx| {
                let discovery = cx
                    .background_executor()
                    .spawn(async move { okilum_t3::projects::discover(&vault) })
                    .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if generation != this.project_navigation.generation {
                        return;
                    }
                    this.project_navigation.discovery = discovery;
                    this.project_navigation.loading = false;
                    this.refresh_project_statuses(window, cx);
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn refresh_project_statuses(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let projects = self.project_navigation.discovery.projects.clone();
        let state = self.session_directory.clone();
        let generation = self.project_navigation.generation;
        cx.spawn_in(window, async move |this, cx| {
            let connections = cx
                .background_executor()
                .spawn(async move {
                    let Some(state) = state else {
                        return Ok(std::collections::BTreeMap::<String, String>::new());
                    };
                    match std::fs::read(state.join("t3-connections.json")) {
                        Ok(source) => serde_json::from_slice(&source).map_err(anyhow::Error::from),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            Ok(Default::default())
                        }
                        Err(e) => Err(e.into()),
                    }
                })
                .await;
            let mut gateways =
                std::collections::BTreeMap::<String, Arc<dyn okilum_t3::ManagerGateway>>::new();
            if let Ok(connections) = connections {
                for (alias, origin) in connections {
                    let Ok(env) = okilum_t3::Environment::new(&alias, &origin) else {
                        continue;
                    };
                    let Ok(task) =
                        this.update_in(cx, |_, _, cx| cx.read_credentials(&env.credential_url()))
                    else {
                        return;
                    };
                    let token = match task.await {
                        Ok(Some((_, bytes))) => okilum_t3::StoredToken::from_secret(&bytes).ok(),
                        _ => None,
                    };
                    let gateway = cx
                        .background_executor()
                        .spawn(async move { okilum_t3::McpGateway::new(env, token) })
                        .await;
                    if let Ok(gateway) = gateway {
                        gateways.insert(alias, Arc::new(gateway));
                    }
                }
            }
            let managers = projects
                .iter()
                .map(|p| (p.folder.clone(), p.manager.clone()))
                .collect();
            let statuses = cx
                .background_executor()
                .spawn(async move { okilum_t3::projects::statuses(&projects, &gateways) })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.project_navigation.generation == generation {
                    this.project_navigation.statuses = statuses;
                    this.project_navigation.status_managers = managers;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn enter_vault_project(
        &mut self,
        project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.project_navigation.generation = self.project_navigation.generation.wrapping_add(1);
        let generation = self.project_navigation.generation;
        let vault = self.vault_root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let root = vault.clone();
            let fresh = cx
                .background_executor()
                .spawn(async move {
                    anyhow::ensure!(
                        project
                            .folder
                            .canonicalize()?
                            .starts_with(root.canonicalize()?),
                        "Project moved outside this vault"
                    );
                    Project::read(&project.folder)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.vault_root != vault || this.project_navigation.generation != generation {
                    return;
                }
                match fresh {
                    Ok(project) => this.finish_enter_vault_project(project, window, cx),
                    Err(error) => {
                        this.project_navigation
                            .discovery
                            .errors
                            .push((vault, format!("{error:#}")));
                        this.project_navigation.section = Section::Projects;
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn finish_enter_vault_project(
        &mut self,
        mut project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project_navigation.status_managers.get(&project.folder) == Some(&project.manager)
            && self
                .project_navigation
                .statuses
                .get(&project.folder)
                .is_some_and(|s| s.restartable())
        {
            project.manager = okilum_t3::projects::Manager::NotStarted;
        }
        let Ok(relative) = project.folder.strip_prefix(&self.vault_root) else {
            return;
        };
        let relative = okilum_core::vault::note_path(relative);
        if self.project_navigation.saved.is_none() {
            self.project_navigation.saved = Some(Saved {
                tree: self.tree.clone(),
                panels: self.panels.clone(),
                widths: self.panel_widths.clone(),
                tree_offset: self.tree_scroll.0.borrow().base_handle.offset(),
            });
        }
        self.tree.scope_to(relative.as_str());
        self.tree_revealed = self.selected_file().to_owned();
        self.panel_widths.backlinks = self.panel_widths.backlinks.max(560.);
        self.tree_sticky.clear();
        self.panels.open(reader_layout::Panel::Notes);
        self.panels.open(reader_layout::Panel::Backlinks);
        if let Some(panel) = self.project_navigation.manager_panel.take() {
            panel.update(cx, |panel, cx| panel.hide(cx));
        }
        self.project_navigation.manager_panel = Some(cx.new(|cx| {
            reader_manager_panel::ManagerPanel::new(
                project.clone(),
                self.session_directory.clone(),
                window,
                cx,
            )
        }));
        if let Some(panel) = &self.project_navigation.manager_panel {
            cx.observe(panel, |_, _, cx| cx.notify()).detach();
        }
        self.project_navigation.active = Some(project.clone());
        self.project_navigation.section = Section::Reader;
        self.open_project_area(project.folder, window, cx);
        cx.notify();
    }

    pub(super) fn leave_vault_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.project_navigation.generation = self.project_navigation.generation.wrapping_add(1);
        if let Some(panel) = self.project_navigation.manager_panel.take() {
            panel.update(cx, |panel, cx| panel.hide(cx));
        }
        self.close_project(window, cx);
        self.project_navigation.active = None;
        self.project_navigation.section = Section::Reader;
        if let Some(saved) = self.project_navigation.saved.take() {
            self.tree = saved.tree;
            // Refresh inventory while retaining the original expansion/cursor.
            self.tree.refresh(&self.vault_root, &self.vault.entries);
            self.tree_revealed = self.selected_file().to_owned();
            self.panels = saved.panels;
            self.panel_widths = saved.widths;
            self.tree_sticky.clear();
            self.tree_scroll
                .0
                .borrow()
                .base_handle
                .set_offset(saved.tree_offset);
        }
        cx.notify();
    }

    pub(super) fn render_app_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let section = self.project_navigation.section;
        let mut body = v_flex()
            .id("app-section")
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .gap_3();
        match section {
            Section::Projects => {
                body = body.child(div().text_lg().child("Projects"));
                if self.project_navigation.loading {
                    body = body.child("Loading projects…");
                }
                for (index, project) in self
                    .project_navigation
                    .discovery
                    .projects
                    .iter()
                    .enumerate()
                {
                    let project = project.clone();
                    let selected = project.clone();
                    let path = project
                        .folder
                        .strip_prefix(&self.vault_root)
                        .unwrap_or(&project.folder)
                        .display()
                        .to_string();
                    let state = self
                        .project_navigation
                        .statuses
                        .get(&project.folder)
                        .map(okilum_t3::projects::Status::label)
                        .unwrap_or_else(|| {
                            if project.manager == okilum_t3::projects::Manager::NotStarted {
                                "Manager не запущен".into()
                            } else {
                                "Checking manager…".into()
                            }
                        });
                    body = body.child(
                        Button::new(("vault-project", index))
                            .ghost()
                            .label(format!("{} · {} · {}", project.name, path, state))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.enter_vault_project(selected.clone(), window, cx)
                            })),
                    );
                }
                for (path, error) in &self.project_navigation.discovery.errors {
                    body = body.child(div().text_color(cx.theme().danger).child(format!(
                        "{}: {}",
                        path.display(),
                        error
                    )));
                }
                if !self.project_navigation.loading
                    && self.project_navigation.discovery.projects.is_empty()
                {
                    body = body.child("No project manifests found in this vault.");
                }
            }
            Section::Inbox => {
                body = body.child(div().text_lg().child("Inbox"));
                for (index, item) in self.inbox.iter().enumerate() {
                    let path = item.path.clone();
                    body = body.child(
                        Button::new(("inbox-note", index))
                            .ghost()
                            .label(self.note_label(&path))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.project_navigation.section = Section::Reader;
                                this.open_note(&path, None, window, cx);
                            })),
                    );
                }
            }
            Section::Logs => {
                body = body.child(div().text_lg().child("Logs")).child(
                    Button::new("app-kibana-sources")
                        .ghost()
                        .label("Kibana sources…")
                        .on_click(cx.listener(|this, _, _, cx| {
                            reader_log::remote::show(this.vault_root.clone(), cx)
                        })),
                );
                for (index, entry) in self
                    .vault
                    .entries
                    .iter()
                    .filter(|entry| okilum_core::log::is_log_path(Path::new(&entry.path)))
                    .enumerate()
                {
                    let path = entry.path.clone();
                    body = body.child(
                        Button::new(("app-log-file", index))
                            .ghost()
                            .label(path.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.project_navigation.section = Section::Reader;
                                this.preview_file(&path, window, cx);
                            })),
                    );
                }
            }
            Section::Reader => {}
        }
        body.into_any_element()
    }
}
