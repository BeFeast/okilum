//! Projects is an application section; entering one temporarily scopes the vault.
use super::*;
use okilum_shell::projects_picker::{
    picker::{ManagerObservation, PickerModel},
    LeaveProject, Presentation, ProjectMenu, ProjectsPicker, Selected,
};
use okilum_t3::projects::{
    icon::{Counterpart, ProjectIcon, Saved},
    Discovery, Project,
};

type Gateways = std::collections::BTreeMap<String, Arc<dyn okilum_t3::ManagerGateway>>;

/// One MCP gateway per T3 connection saved on this machine, with its token
/// from the keychain. `None` once the Reader is gone.
async fn t3_gateways(
    this: &WeakEntity<Reader>,
    state: Option<PathBuf>,
    cx: &mut AsyncWindowContext,
) -> Option<Gateways> {
    let connections = cx
        .background_executor()
        .spawn(async move {
            let Some(state) = state else {
                return Ok(std::collections::BTreeMap::<String, String>::new());
            };
            match std::fs::read(state.join("t3-connections.json")) {
                Ok(source) => serde_json::from_slice(&source).map_err(anyhow::Error::from),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
                Err(e) => Err(e.into()),
            }
        })
        .await;
    let mut gateways = Gateways::new();
    if let Ok(connections) = connections {
        for (alias, origin) in connections {
            let Ok(env) = okilum_t3::Environment::new(&alias, &origin) else {
                continue;
            };
            let task = this
                .update_in_own_window(cx, |_, _, cx| cx.read_credentials(&env.credential_url()))
                .ok()?;
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
    Some(gateways)
}

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
    /// The T3 connections of the last status sweep, kept for icon writes.
    pub t3_gateways: Gateways,
    /// `discovery` holds this vault's scan, possibly empty (#1518).
    pub discovered: bool,
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
        cx.set_global(okilum_shell::project_avatar::ProjectIcons::default());
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
                this.project_navigation.discovered = true;
                this.project_navigation.loading = false;
                this.sync_projects_picker(cx);
                this.refresh_project_statuses(window, cx);
                this.refresh_palette_sections(cx);
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

    /// ⌘K lists projects too (#1518). The first open in a vault scans the
    /// manifests once, without the manager status sweep the switcher runs;
    /// the switcher and the Projects section rescan as before.
    pub(super) fn ensure_palette_projects(&mut self, cx: &mut Context<Self>) {
        if self.single_file || self.project_navigation.discovered || !self.vault_root.is_dir() {
            return;
        }
        let generation = self.project_navigation.generation;
        let vault = self.vault_root.clone();
        cx.spawn(async move |this, cx| {
            let scan = vault.clone();
            let discovery = cx
                .background_executor()
                .spawn(async move { okilum_t3::projects::discover(&scan) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let navigation = &mut this.project_navigation;
                if this.vault_root != vault
                    || navigation.generation != generation
                    || navigation.discovered
                {
                    return;
                }
                navigation.discovery = discovery;
                navigation.discovered = true;
                this.sync_projects_picker(cx);
                this.refresh_palette_sections(cx);
            });
        })
        .detach();
    }

    /// The palette's Projects section: the shared #1520 picker model's rows
    /// for `query` (recent projects first, then by domain), each project
    /// once. «All projects…» opens the full switcher with the same text.
    pub(super) fn palette_projects(
        &self,
        query: &str,
        limit: usize,
    ) -> Vec<okilum_shell::projects_picker::picker::Row> {
        let navigation = &self.project_navigation;
        if navigation.discovery.projects.is_empty() {
            return Vec::new();
        }
        let mut model = PickerModel::new(
            self.vault_root.clone(),
            navigation.discovery.clone(),
            self.recent_project_folders(),
        );
        model.set_query(query);
        let mut seen = std::collections::BTreeSet::new();
        model
            .groups()
            .into_iter()
            .flat_map(|group| group.rows)
            .filter(|row| seen.insert(row.project.folder.clone()))
            .take(limit)
            .collect()
    }

    fn refresh_project_statuses(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let projects = self.project_navigation.discovery.projects.clone();
        let state = self.session_directory.clone();
        let generation = self.project_navigation.generation;
        let vault = self.vault_root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Some(gateways) = t3_gateways(&this, state, cx).await else {
                return;
            };
            let managers = projects
                .iter()
                .map(|p| (p.folder.clone(), p.manager.clone()))
                .collect();
            let folders: Vec<PathBuf> = projects.iter().map(|p| p.folder.clone()).collect();
            let kept = gateways.clone();
            let (statuses, links) = cx
                .background_executor()
                .spawn(async move {
                    // The same connections answer the icons (#1627): one
                    // project listing per environment.
                    let links =
                        okilum_t3::projects::icon::counterparts(&vault, &projects, &gateways);
                    (okilum_t3::projects::statuses(&projects, &gateways), links)
                })
                .await;
            let _ = this.update_in_own_window(cx, |this, _, cx| {
                if this.project_navigation.generation == generation {
                    this.project_navigation.statuses = statuses;
                    this.project_navigation.status_managers = managers;
                    this.project_navigation.t3_gateways = kept;
                    this.sync_projects_picker_statuses(cx);
                    this.merge_icon_links(folders, links, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// T3's word on these projects' icons replaces what was known of them.
    fn merge_icon_links(
        &mut self,
        folders: Vec<PathBuf>,
        links: std::collections::BTreeMap<PathBuf, Counterpart>,
        cx: &mut Context<Self>,
    ) {
        let icons = cx.default_global::<okilum_shell::project_avatar::ProjectIcons>();
        for folder in folders {
            icons.links.remove(&folder);
        }
        icons.links.extend(links);
        for picker in self.pickers() {
            picker.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    /// The entered project's T3 icon, without the full status sweep.
    fn refresh_project_icon(
        &mut self,
        project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if project.environment.is_none() {
            return;
        }
        let state = self.session_directory.clone();
        let vault = self.vault_root.clone();
        let known = self.project_navigation.t3_gateways.clone();
        cx.spawn_in(window, async move |this, cx| {
            let gateways = if known.is_empty() {
                match t3_gateways(&this, state, cx).await {
                    Some(gateways) => gateways,
                    None => return,
                }
            } else {
                known
            };
            let folder = project.folder.clone();
            let kept = gateways.clone();
            let scan = vault.clone();
            let links = cx
                .background_executor()
                .spawn(async move {
                    okilum_t3::projects::icon::counterparts(
                        &scan,
                        std::slice::from_ref(&project),
                        &gateways,
                    )
                })
                .await;
            let _ = this.update_in_own_window(cx, |this, _, cx| {
                if this.vault_root != vault {
                    return;
                }
                if this.project_navigation.t3_gateways.is_empty() {
                    this.project_navigation.t3_gateways = kept;
                }
                this.merge_icon_links(vec![folder], links, cx);
            });
        })
        .detach();
    }

    /// Right-click on a project (#1627): «Change icon…» at the pointer.
    pub(super) fn open_project_menu(
        &mut self,
        project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::menu::{PopupMenu, PopupMenuItem};
        self.close_project_switcher(window, cx);
        let reader = cx.entity().downgrade();
        let popup = PopupMenu::build(window, cx, move |menu, _, _| {
            let reader = reader.clone();
            let project = project.clone();
            menu.item(
                PopupMenuItem::new("Change icon…").on_click(move |_, window, cx| {
                    let project = project.clone();
                    let _ = reader.update(cx, |this, cx| {
                        this.open_project_icon_dialog(project, window, cx)
                    });
                }),
            )
        });
        cx.subscribe(&popup, |this, dismissed, _: &DismissEvent, cx| {
            if this
                .file_menu
                .as_ref()
                .is_some_and(|(menu, _)| menu.entity_id() == dismissed.entity_id())
            {
                this.file_menu = None;
                cx.notify();
            }
        })
        .detach();
        popup.focus_handle(cx).focus(window, cx);
        self.file_menu = Some((popup, window.mouse_position()));
        cx.notify();
    }

    /// T3 Code's «Choose project icon»; Save and Reset store the choice.
    pub(super) fn open_project_icon_dialog(
        &mut self,
        project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::WindowExt as _;
        use okilum_shell::project_icon_dialog::{IconDialogEvent, ProjectIconDialog, TITLE};
        self.file_menu = None;
        let current = okilum_shell::project_avatar::manual_icon(&project, cx);
        let name = project.name.clone();
        let dialog = cx.new(|cx| ProjectIconDialog::new(&name, current.as_ref(), window, cx));
        cx.subscribe_in(
            &dialog,
            window,
            move |this, _, event: &IconDialogEvent, window, cx| {
                let icon = match event {
                    IconDialogEvent::Cancel => {
                        window.close_dialog(cx);
                        return;
                    }
                    IconDialogEvent::Reset => None,
                    IconDialogEvent::Save(icon) => Some(icon.clone()),
                };
                window.close_dialog(cx);
                this.save_project_icon(project.clone(), icon, window, cx);
            },
        )
        .detach();
        let content = dialog.clone();
        window.open_dialog(cx, move |d, _, _| {
            d.title(TITLE)
                .width(px(520.))
                .on_ok(|_, _, _| false)
                .child(content.clone())
        });
        // The dialog takes the focus as it opens; the search gets it after.
        window.defer(cx, move |window, cx| {
            dialog.update(cx, |dialog, cx| {
                let tab = dialog.draft().tab;
                dialog.set_tab(tab, window, cx)
            });
        });
        cx.notify();
    }

    /// In T3 for a project T3 has, in project.yaml otherwise (#1627).
    fn save_project_icon(
        &mut self,
        project: Project,
        icon: Option<ProjectIcon>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vault = self.vault_root.clone();
        let known = project
            .environment
            .as_ref()
            .and_then(|alias| self.project_navigation.t3_gateways.get(alias).cloned());
        let connect = known.is_none()
            && project.environment.is_some()
            && self.project_navigation.t3_gateways.is_empty();
        let state = self.session_directory.clone();
        cx.spawn_in(window, async move |this, cx| {
            let gateway = match known {
                Some(gateway) => Some(gateway),
                None if connect => {
                    let gateways = t3_gateways(&this, state, cx).await.unwrap_or_default();
                    project
                        .environment
                        .as_ref()
                        .and_then(|alias| gateways.get(alias).cloned())
                }
                None => None,
            };
            let target = project.clone();
            let scan = vault.clone();
            let saved = cx
                .background_executor()
                .spawn(async move {
                    okilum_t3::projects::icon::save_icon(
                        &scan,
                        &target,
                        gateway.as_deref(),
                        icon.as_ref(),
                    )
                })
                .await;
            let _ = this.update_in_own_window(cx, |this, _, cx| {
                if this.vault_root == vault {
                    this.finish_project_icon(project, saved, cx);
                }
            });
        })
        .detach();
    }

    fn finish_project_icon(
        &mut self,
        project: Project,
        saved: anyhow::Result<Saved>,
        cx: &mut Context<Self>,
    ) {
        let folder = project.folder.clone();
        match saved {
            Ok(Saved::T3(counterpart)) => {
                let links = std::collections::BTreeMap::from([(folder.clone(), counterpart)]);
                self.merge_icon_links(vec![folder], links, cx);
            }
            Ok(Saved::Manifest(read_back)) => {
                let appearance = read_back.appearance.clone();
                let navigation = &mut self.project_navigation;
                for known in navigation
                    .discovery
                    .projects
                    .iter_mut()
                    .chain(navigation.active.iter_mut())
                    .filter(|p| p.folder == folder)
                {
                    known.appearance = appearance.clone();
                }
                self.sync_projects_picker(cx);
                self.merge_icon_links(vec![folder], Default::default(), cx);
            }
            Err(error) => {
                self.link_notice =
                    Some(format!("Cannot change the project icon: {error:#}").into());
            }
        }
        cx.notify();
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
        self.refresh_project_icon(project.clone(), window, cx);
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
        cx.subscribe_in(
            &picker,
            window,
            |this, _, event: &ProjectMenu, window, cx| {
                this.open_project_menu(event.0.clone(), window, cx);
            },
        )
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
