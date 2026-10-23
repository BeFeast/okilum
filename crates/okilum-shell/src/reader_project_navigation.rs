//! Projects is an application section; entering one temporarily scopes the vault.
use super::*;
use okilum_shell::projects_picker::{
    picker::{ManagerObservation, PickerModel},
    LeaveProject, Presentation, ProjectsPicker, Selected,
};
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
    /// The shared #1520 picker is the Projects section surface.
    pub picker: Option<Entity<ProjectsPicker>>,
    /// The same picker as the title-bar switcher's popover (#1519), fed by
    /// the same discovery and status sync.
    pub switcher: Option<Entity<ProjectsPicker>>,
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
        self.title_nav.close();
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
        if section != Section::Projects {
            self.release_picker_focus(window, cx);
        }
        if section == Section::Projects {
            let generation = self.project_navigation.generation;
            self.project_navigation.loading = true;
            self.ensure_projects_picker(window, cx);
            if let Some(picker) = &self.project_navigation.picker {
                let active = self
                    .project_navigation
                    .active
                    .as_ref()
                    .map(|p| p.folder.clone());
                picker.update(cx, |picker, cx| {
                    picker.set_loading(true, cx);
                    picker.set_active(active, cx);
                    picker.focus_input(window, cx);
                });
            }
            self.discover_projects(generation, window, cx);
        }
        cx.notify();
    }

    /// Rescan manifests off the UI thread and feed every mounted picker.
    fn discover_projects(&mut self, generation: u64, window: &mut Window, cx: &mut Context<Self>) {
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
                this.sync_projects_picker(cx);
                this.refresh_project_statuses(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// «<project> ▾» opens the shared picker below the switcher (#1519).
    /// Opening does not cancel an entry in flight: no generation bump.
    pub(super) fn show_project_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picker = self.ensure_switcher_picker(window, cx);
        let navigation = &self.project_navigation;
        let active = navigation.active.as_ref().map(|p| p.folder.clone());
        let empty =
            navigation.discovery.projects.is_empty() && navigation.discovery.errors.is_empty();
        picker.update(cx, |picker, cx| {
            picker.set_query(String::new(), window, cx);
            picker.set_loading(empty, cx);
            picker.set_active(active, cx);
            picker.focus_input(window, cx);
        });
        let generation = self.project_navigation.generation;
        self.discover_projects(generation, window, cx);
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
                    this.sync_projects_picker_statuses(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Every mounted picker: the Projects section and the title-bar switcher.
    fn pickers(&self) -> Vec<Entity<ProjectsPicker>> {
        let navigation = &self.project_navigation;
        navigation
            .picker
            .iter()
            .chain(navigation.switcher.iter())
            .cloned()
            .collect()
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
        let folder = project.folder.clone();
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
                        // The failed project becomes a muted problem row, not a recent.
                        let navigation = &mut this.project_navigation;
                        navigation.discovery.projects.retain(|p| p.folder != folder);
                        navigation
                            .discovery
                            .errors
                            .retain(|(path, _)| path != &folder);
                        navigation
                            .discovery
                            .errors
                            .push((folder, format!("{error:#}")));
                        navigation.section = Section::Projects;
                        this.sync_projects_picker(cx);
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
        self.record_recent_project(&project.folder, cx);
        self.release_picker_focus(window, cx);
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
        for picker in self.pickers() {
            picker.update(cx, |picker, cx| picker.set_active(None, cx));
        }
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

    /// The hidden picker's search must not keep keyboard focus from the note.
    pub(super) fn release_picker_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.pickers().iter().any(|picker| {
            gpui::Focusable::focus_handle(picker.read(cx), cx).contains_focused(window, cx)
        });
        if focused {
            self.restore_document_focus(window, cx);
        }
    }

    fn recent_project_folders(&self) -> Vec<PathBuf> {
        self.sidebar
            .recent_projects
            .iter()
            .map(|rel| self.vault_root.join(rel))
            .collect()
    }

    /// One picker per vault generation; `reset_vault_projects` drops it.
    fn ensure_projects_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_navigation.picker.is_some() {
            return;
        }
        let picker = self.new_projects_picker(Presentation::Page, window, cx);
        self.project_navigation.picker = Some(picker);
    }

    /// The switcher's popover: the same component, discovery and recents.
    fn ensure_switcher_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ProjectsPicker> {
        if let Some(picker) = &self.project_navigation.switcher {
            return picker.clone();
        }
        let picker = self.new_projects_picker(Presentation::Popover, window, cx);
        self.sync_picker_statuses(&picker, cx);
        cx.subscribe_in(&picker, window, |this, _, _: &DismissEvent, window, cx| {
            // Esc closes. A press on the switcher itself is its own toggle
            // click, so the outside-press dismissal leaves it to that click.
            if window.last_input_was_keyboard()
                || !this.title_nav.on_switcher(window.mouse_position())
            {
                this.close_project_switcher(window, cx);
            }
        })
        .detach();
        cx.subscribe_in(&picker, window, |this, _, _: &LeaveProject, window, cx| {
            this.close_project_switcher(window, cx);
            this.leave_vault_project(window, cx);
        })
        .detach();
        self.project_navigation.switcher = Some(picker.clone());
        picker
    }

    fn new_projects_picker(
        &mut self,
        presentation: Presentation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ProjectsPicker> {
        let model = PickerModel::new(
            self.vault_root.clone(),
            self.project_navigation.discovery.clone(),
            self.recent_project_folders(),
        );
        let picker = cx.new(|cx| ProjectsPicker::new(model, presentation, window, cx));
        cx.subscribe_in(&picker, window, |this, _, event: &Selected, window, cx| {
            this.close_project_switcher(window, cx);
            // Same revalidating activation pipeline as before; recents are
            // recorded only after a successful entry.
            this.enter_vault_project(event.0.clone(), window, cx);
        })
        .detach();
        picker
    }

    fn sync_projects_picker(&mut self, cx: &mut Context<Self>) {
        let discovery = self.project_navigation.discovery.clone();
        for picker in self.pickers() {
            let discovery = discovery.clone();
            picker.update(cx, |picker, cx| picker.update_discovery(discovery, cx));
        }
        self.sync_projects_picker_statuses(cx);
    }

    fn sync_projects_picker_statuses(&mut self, cx: &mut Context<Self>) {
        for picker in self.pickers() {
            self.sync_picker_statuses(&picker, cx);
        }
    }

    fn sync_picker_statuses(&self, picker: &Entity<ProjectsPicker>, cx: &mut Context<Self>) {
        let navigation = &self.project_navigation;
        let observations = navigation
            .statuses
            .iter()
            .filter_map(|(folder, status)| {
                let manager = navigation.status_managers.get(folder)?.clone();
                Some((
                    folder.clone(),
                    ManagerObservation {
                        manager,
                        status: status.clone(),
                    },
                ))
            })
            .collect();
        picker.update(cx, |picker, cx| picker.update_statuses(observations, cx));
    }

    fn record_recent_project(&mut self, folder: &Path, cx: &mut Context<Self>) {
        let Ok(relative) = folder.strip_prefix(&self.vault_root) else {
            return;
        };
        let relative = okilum_core::vault::note_path(relative);
        let recent = &mut self.sidebar.recent_projects;
        if recent.first() != Some(&relative) {
            recent.retain(|p| p != &relative);
            recent.insert(0, relative);
            recent.truncate(32);
            self.save_sidebar(cx);
        }
        for picker in self.pickers() {
            let folder = folder.to_owned();
            picker.update(cx, |picker, cx| {
                picker.record_opened(&folder, cx);
                picker.set_active(Some(folder.clone()), cx);
            });
        }
    }

    pub(super) fn render_app_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let section = self.project_navigation.section;
        let mut body = v_flex()
            .id("app-section")
            .debug_selector(|| "app-section".into())
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .gap_3();
        match section {
            Section::Projects => {
                body = body
                    .overflow_hidden()
                    .child(div().text_lg().child("Projects"));
                body = match &self.project_navigation.picker {
                    Some(picker) => body.child(div().flex_1().min_h_0().child(picker.clone())),
                    None => body.child("Loading projects…"),
                };
                if !self.project_navigation.loading
                    && self.project_navigation.discovery.projects.is_empty()
                    && self.project_navigation.discovery.errors.is_empty()
                {
                    body = body.child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child("No project manifests found in this vault."),
                    );
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
