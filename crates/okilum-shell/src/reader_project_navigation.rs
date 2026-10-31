//! Projects is an application section; entering one temporarily scopes the vault.
use super::*;
use okilum_shell::projects_picker::{
    picker::{ManagerObservation, PickerModel},
    LeaveProject, NewProject, Presentation, ProjectMenu, ProjectsPicker, Selected,
};
use okilum_t3::projects::{
    icon::{Counterpart, ProjectIcon, Saved as IconSaved},
    Discovery, Project,
};

pub(super) type Gateways = std::collections::BTreeMap<String, Arc<dyn okilum_t3::ManagerGateway>>;

/// Local metadata and logo paths are available before any network operation.
fn cached_project_icons(
    state: Option<&Path>,
    projects: &[Project],
) -> std::collections::BTreeMap<PathBuf, Counterpart> {
    let Some(state) = state else {
        return Default::default();
    };
    let origins = project::browser_connections(state, projects).unwrap_or_default();
    okilum_t3::projects::icon::cache::load(&state.join("project-logos"), projects, &origins)
}

fn store_project_icons(
    state: Option<&Path>,
    projects: &[Project],
    refresh: &okilum_t3::projects::icon::CounterpartRefresh,
) {
    let Some(state) = state else { return };
    let origins = project::browser_connections(state, projects).unwrap_or_default();
    if let Err(error) = okilum_t3::projects::icon::cache::store(
        &state.join("project-logos"),
        projects,
        &origins,
        refresh,
    ) {
        eprintln!("Project icon cache: {error:#}");
    }
}

fn refresh_project_icons(
    vault: &Path,
    projects: &[Project],
    gateways: &Gateways,
    state: Option<&Path>,
) -> okilum_t3::projects::icon::CounterpartRefresh {
    let logos = state.map(|state| state.join("project-logos"));
    let refresh = okilum_t3::projects::icon::refresh_counterparts(
        vault,
        projects,
        gateways,
        logos.as_deref(),
    );
    store_project_icons(state, projects, &refresh);
    refresh
}

/// A project's icon as shown before a save, put back if the save fails.
struct ShownIcon {
    link: Option<Counterpart>,
    manifest: Option<ProjectIcon>,
}

/// Where the Manager panel's T3 sign-in lives (#1627): its web view's store.
#[cfg(target_os = "macos")]
fn manager_session() -> Arc<dyn okilum_t3::SessionSource> {
    Arc::new(crate::reader_manager_store::ManagerCookies)
}

/// No Manager web view here: icons use an MCP token if one is connected.
#[cfg(not(target_os = "macos"))]
fn manager_session() -> Arc<dyn okilum_t3::SessionSource> {
    Arc::new(okilum_t3::NoSessionSource)
}

/// The paired gateway of one saved T3 connection: projects and icons with
/// the Manager panel's T3 session (connected on the first call, within
/// `okilum_t3::CONNECT_TIMEOUT`), including read-only thread summaries. MCP
/// remains the fallback when no browser session exists.
fn paired_gateway(
    env: okilum_t3::Environment,
    token: Option<okilum_t3::StoredToken>,
    session: Arc<dyn okilum_t3::SessionSource>,
) -> anyhow::Result<Arc<dyn okilum_t3::ManagerGateway>> {
    let mcp = okilum_t3::McpGateway::new(env.clone(), token)?;
    with_manager_session(env, Arc::new(mcp), session)
}

fn with_manager_session(
    env: okilum_t3::Environment,
    mcp: Arc<dyn okilum_t3::ManagerGateway>,
    source: Arc<dyn okilum_t3::SessionSource>,
) -> anyhow::Result<Arc<dyn okilum_t3::ManagerGateway>> {
    let session = okilum_t3::SessionGateway::new(env, source)?;
    Ok(Arc::new(okilum_t3::PairedGateway::new(session, mcp)))
}

/// Native Board reads on macOS use the Manager's persistent browser session,
/// through the same gateway as project icons. MCP-only native UI keeps its
/// existing transport and credential lifecycle on the other platforms.
pub(super) fn manager_gateway(
    env: okilum_t3::Environment,
    mcp: Arc<dyn okilum_t3::ManagerGateway>,
) -> anyhow::Result<Arc<dyn okilum_t3::ManagerGateway>> {
    #[cfg(test)]
    t3_probe::record_manager_gateway();
    #[cfg(target_os = "macos")]
    {
        with_manager_session(env, mcp, manager_session())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = env;
        Ok(mcp)
    }
}

/// One gateway per T3 connection saved on this machine (t3-connections.json,
/// written when the Manager panel connects): the panel's T3 session for
/// projects, icons and read-only Board threads, MCP for commands. `None` once
/// the Reader is gone.
pub(super) async fn t3_gateways(
    this: &WeakEntity<Reader>,
    state: Option<PathBuf>,
    cx: &mut AsyncWindowContext,
) -> Option<Gateways> {
    #[cfg(test)]
    let probe = t3_probe::sweep().await;
    let projects = this
        .update_in_own_window(cx, |reader, _, _| {
            reader.project_navigation.discovery.projects.clone()
        })
        .ok()?;
    let connections = cx
        .background_executor()
        .spawn(async move {
            match state {
                Some(state) => project::browser_connections(&state, &projects),
                None => Ok(std::collections::BTreeMap::<String, String>::new()),
            }
        })
        .await;
    let mut gateways = Gateways::new();
    if let Ok(connections) = connections {
        for (alias, origin) in connections {
            let Ok(env) = okilum_t3::Environment::new(&alias, &origin) else {
                continue;
            };
            #[cfg(test)]
            if let Some(gateway) = probe.as_ref().and_then(|p| p.connect(&alias, &origin)) {
                gateways.insert(alias.clone(), gateway);
                continue;
            }
            let task = this
                .update_in_own_window(cx, |_, _, cx| cx.read_credentials(&env.credential_url()))
                .ok()?;
            let token = match task.await {
                Ok(Some((_, bytes))) => okilum_t3::StoredToken::from_secret(&bytes).ok(),
                _ => None,
            };
            #[cfg(test)]
            let token = token.or_else(|| probe.as_ref().and_then(|p| p.token.clone()));
            let gateway = cx
                .background_executor()
                .spawn(async move { paired_gateway(env, token, manager_session()) })
                .await;
            match gateway {
                Ok(gateway) => {
                    gateways.insert(alias, gateway);
                }
                Err(error) => eprintln!("okilum: T3 connection {alias} unusable: {error:#}"),
            }
        }
    }
    Some(gateways)
}

/// Test-only instrument (#1521): every attempt this file makes to reach T3
/// is counted, also those that would fail, and a probed connection gets a
/// fake gateway in place of the MCP and T3 session transports.
#[cfg(test)]
mod t3_probe {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::Mutex;

    #[derive(Default)]
    pub(super) struct Probe {
        /// `t3_gateways` entries: status sweeps and on-demand icon connects.
        pub sweeps: AtomicUsize,
        /// The (alias, origin) of every T3 connection a sweep opened.
        pub connects: Mutex<Vec<(String, String)>>,
        /// Board and Manager transports built through `manager_gateway`.
        pub manager_gateways: AtomicUsize,
        /// What every probed connection gets instead of a real transport;
        /// without one, the real transports are built.
        pub gateway: Option<Arc<dyn okilum_t3::ManagerGateway>>,
        /// The stored token real transports get, as the keychain would
        /// give it; the test platform's keychain is always empty.
        pub token: Option<okilum_t3::StoredToken>,
        /// While set, a sweep waits here before it reads any connection.
        pub gate: Mutex<Option<async_channel::Receiver<()>>>,
        /// While set, a sweep blocks its thread this long, as a hung T3
        /// connect would.
        pub delay: Mutex<Option<std::time::Duration>>,
    }

    impl Probe {
        pub(super) fn connect(
            &self,
            alias: &str,
            origin: &str,
        ) -> Option<Arc<dyn okilum_t3::ManagerGateway>> {
            self.connects
                .lock()
                .unwrap()
                .push((alias.into(), origin.into()));
            self.gateway.clone()
        }
    }

    thread_local! {
        static CURRENT: std::cell::RefCell<Option<Arc<Probe>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// The probe of this test thread, until the returned guard drops.
    pub(super) fn install(probe: Arc<Probe>) -> Installed {
        CURRENT.with(|current| *current.borrow_mut() = Some(probe));
        Installed
    }

    pub(super) struct Installed;
    impl Drop for Installed {
        fn drop(&mut self) {
            CURRENT.with(|current| current.borrow_mut().take());
        }
    }

    fn current() -> Option<Arc<Probe>> {
        CURRENT.with(|current| current.borrow().clone())
    }

    pub(super) async fn sweep() -> Option<Arc<Probe>> {
        let probe = current()?;
        probe.sweeps.fetch_add(1, SeqCst);
        let delay = *probe.delay.lock().unwrap();
        if let Some(delay) = delay {
            std::thread::sleep(delay);
        }
        let gate = probe.gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            let _ = gate.recv().await;
        }
        Some(probe)
    }

    pub(super) fn record_manager_gateway() {
        if let Some(probe) = current() {
            probe.manager_gateways.fetch_add(1, SeqCst);
        }
    }
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
    pub boards: std::collections::BTreeMap<PathBuf, Entity<project::board::Board>>,
    pub board_visible: bool,
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
        let state = self.session_directory.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (discovery, icons) = cx
                .background_executor()
                .spawn(async move {
                    let discovery = okilum_t3::projects::discover(&vault);
                    let icons = cached_project_icons(state.as_deref(), &discovery.projects);
                    (discovery, icons)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if generation != this.project_navigation.generation {
                    return;
                }
                let folders = discovery
                    .projects
                    .iter()
                    .map(|p| p.folder.clone())
                    .collect();
                this.merge_icon_links(folders, icons, cx);
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
        let state = self.session_directory.clone();
        cx.spawn(async move |this, cx| {
            let scan = vault.clone();
            let (discovery, icons) = cx
                .background_executor()
                .spawn(async move {
                    let discovery = okilum_t3::projects::discover(&scan);
                    let icons = cached_project_icons(state.as_deref(), &discovery.projects);
                    (discovery, icons)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let navigation = &mut this.project_navigation;
                if this.vault_root != vault
                    || navigation.generation != generation
                    || navigation.discovered
                {
                    return;
                }
                navigation.discovered = true;
                let folders = discovery
                    .projects
                    .iter()
                    .map(|p| p.folder.clone())
                    .collect();
                navigation.discovery = discovery;
                this.merge_icon_links(folders, icons, cx);
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
            let Some(gateways) = t3_gateways(&this, state.clone(), cx).await else {
                return;
            };
            let managers = projects
                .iter()
                .map(|p| (p.folder.clone(), p.manager.clone()))
                .collect();
            let kept = gateways.clone();
            let scan = vault.clone();
            let icon_projects = projects.clone();
            let icon_gateways = gateways.clone();
            let refresh = cx
                .background_executor()
                .spawn(async move {
                    refresh_project_icons(&scan, &icon_projects, &icon_gateways, state.as_deref())
                })
                .await;
            let current = this
                .update_in_own_window(cx, |this, _, cx| {
                    if this.vault_root != vault || this.project_navigation.generation != generation
                    {
                        return false;
                    }
                    this.project_navigation.t3_gateways = kept;
                    this.merge_icon_links(
                        refresh.confirmed.into_iter().collect(),
                        refresh.links,
                        cx,
                    );
                    true
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            let statuses = cx
                .background_executor()
                .spawn(async move { okilum_t3::projects::statuses(&projects, &gateways) })
                .await;
            let _ = this.update_in_own_window(cx, |this, _, cx| {
                if this.vault_root == vault && this.project_navigation.generation == generation {
                    this.project_navigation.statuses = statuses;
                    this.project_navigation.status_managers = managers;
                    this.sync_projects_picker_statuses(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// T3's word on these projects' icons replaces what was known of them.
    pub(super) fn merge_icon_links(
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
                match t3_gateways(&this, state.clone(), cx).await {
                    Some(gateways) => gateways,
                    None => return,
                }
            } else {
                known
            };
            let kept = gateways.clone();
            let scan = vault.clone();
            let refresh = cx
                .background_executor()
                .spawn(async move {
                    refresh_project_icons(
                        &scan,
                        std::slice::from_ref(&project),
                        &gateways,
                        state.as_deref(),
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
                this.merge_icon_links(refresh.confirmed.into_iter().collect(), refresh.links, cx);
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
        // The choice arrives with the window of the press. A subscription
        // would run in the Reader's "current window" instead: whichever
        // window drew the Reader last (another vault window, Settings), or
        // none, and Save then did nothing at all.
        let reader = cx.entity().downgrade();
        dialog.update(cx, |dialog, _| {
            dialog.on_choice(move |event, window, cx| {
                let icon = match event {
                    IconDialogEvent::Cancel => {
                        window.close_dialog(cx);
                        return;
                    }
                    IconDialogEvent::Reset => None,
                    IconDialogEvent::Save(icon) => Some(icon.clone()),
                };
                window.close_dialog(cx);
                let project = project.clone();
                if reader
                    .update(cx, |this, cx| {
                        this.save_project_icon(project, icon, window, cx)
                    })
                    .is_err()
                {
                    eprintln!("Project icon: the window that asked for the icon is gone");
                }
            })
        });
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

    /// Where T3 logos of repositories checked out elsewhere are kept.
    fn project_logo_cache(&self) -> Option<PathBuf> {
        self.session_directory
            .as_ref()
            .map(|state| state.join("project-logos"))
    }

    /// In T3 for a project T3 has, in project.yaml otherwise (#1627). The
    /// project shows the choice at once; T3 or project.yaml confirms it,
    /// and a failure puts the old icon back and says why in a toast.
    pub(super) fn save_project_icon(
        &mut self,
        project: Project,
        icon: Option<ProjectIcon>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kind = icon.as_ref().map_or("reset", ProjectIcon::kind);
        let target = match project.environment.as_deref() {
            Some(alias) => format!("t3:{alias}"),
            None => "project manifest".into(),
        };
        eprintln!(
            "Project icon: saving project={:?} folder={} kind={kind} target={target}",
            project.name,
            project.folder.display()
        );
        let shown = self.shown_project_icon(&project.folder, cx);
        self.show_project_icon(&project.folder, icon.as_ref(), cx);
        let vault = self.vault_root.clone();
        let known = project
            .environment
            .as_ref()
            .and_then(|alias| self.project_navigation.t3_gateways.get(alias).cloned());
        let connect = known.is_none()
            && project.environment.is_some()
            && self.project_navigation.t3_gateways.is_empty();
        let state = self.session_directory.clone();
        let logos = self.project_logo_cache();
        cx.spawn_in(window, async move |this, cx| {
            let gateway = match known {
                Some(gateway) => Some(gateway),
                None if connect => {
                    let gateways = t3_gateways(&this, state.clone(), cx)
                        .await
                        .unwrap_or_default();
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
                    let saved = okilum_t3::projects::icon::save_icon_with_logos(
                        &scan,
                        &target,
                        gateway.as_deref(),
                        icon.as_ref(),
                        logos.as_deref(),
                    );
                    if let Ok(IconSaved::T3(link)) = &saved {
                        let refresh = okilum_t3::projects::icon::CounterpartRefresh {
                            confirmed: [target.folder.clone()].into_iter().collect(),
                            links: [(target.folder.clone(), link.clone())]
                                .into_iter()
                                .collect(),
                        };
                        store_project_icons(
                            state.as_deref(),
                            std::slice::from_ref(&target),
                            &refresh,
                        );
                    }
                    saved
                })
                .await;
            let result = match &saved {
                Ok(IconSaved::T3(link)) => format!("ok t3 project={}", link.project_id),
                Ok(IconSaved::Manifest(_)) => "ok project manifest".into(),
                Err(error) => format!("error: {error:#}"),
            };
            eprintln!(
                "Project icon: saved project={:?} kind={kind} result={result}",
                project.name
            );
            let delivered = this.update_in_own_window(cx, |this, window, cx| {
                if this.vault_root == vault {
                    this.finish_project_icon(project, saved, shown, window, cx);
                }
            });
            if let Err(error) = delivered {
                eprintln!("Project icon: the result did not reach the window: {error:#}");
            }
        })
        .detach();
    }

    /// What a project shows now: its T3 link and its project.yaml icon.
    fn shown_project_icon(&self, folder: &Path, cx: &App) -> ShownIcon {
        let navigation = &self.project_navigation;
        ShownIcon {
            link: cx
                .try_global::<okilum_shell::project_avatar::ProjectIcons>()
                .and_then(|icons| icons.links.get(folder).cloned()),
            manifest: navigation
                .active
                .iter()
                .chain(navigation.discovery.projects.iter())
                .find(|p| p.folder == folder)
                .and_then(|p| p.appearance.icon.clone()),
        }
    }

    /// Shows `icon` on the project before T3 or project.yaml has it.
    fn show_project_icon(
        &mut self,
        folder: &Path,
        icon: Option<&ProjectIcon>,
        cx: &mut Context<Self>,
    ) {
        let linked = match cx
            .default_global::<okilum_shell::project_avatar::ProjectIcons>()
            .links
            .get_mut(folder)
        {
            Some(link) => {
                link.icon = icon.cloned();
                true
            }
            None => false,
        };
        if linked {
            self.refresh_project_avatars(cx);
        } else {
            self.set_manifest_icon(folder, icon.cloned(), cx);
        }
    }

    /// Puts back what the project showed before a failed save.
    fn restore_project_icon(&mut self, folder: &Path, shown: ShownIcon, cx: &mut Context<Self>) {
        let icons = cx.default_global::<okilum_shell::project_avatar::ProjectIcons>();
        match shown.link {
            Some(link) => {
                icons.links.insert(folder.to_owned(), link);
            }
            None => {
                icons.links.remove(folder);
            }
        }
        self.set_manifest_icon(folder, shown.manifest, cx);
    }

    fn set_manifest_icon(
        &mut self,
        folder: &Path,
        icon: Option<ProjectIcon>,
        cx: &mut Context<Self>,
    ) {
        let navigation = &mut self.project_navigation;
        for known in navigation
            .discovery
            .projects
            .iter_mut()
            .chain(navigation.active.iter_mut())
            .filter(|p| p.folder == folder)
        {
            known.appearance.icon = icon.clone();
        }
        self.sync_projects_picker(cx);
        self.refresh_project_avatars(cx);
    }

    /// Every avatar redraws: the pickers and the sidebar header.
    fn refresh_project_avatars(&mut self, cx: &mut Context<Self>) {
        for picker in self.pickers() {
            picker.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    fn finish_project_icon(
        &mut self,
        project: Project,
        saved: anyhow::Result<IconSaved>,
        shown: ShownIcon,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let folder = project.folder.clone();
        match saved {
            Ok(IconSaved::T3(counterpart)) => {
                let links = std::collections::BTreeMap::from([(folder.clone(), counterpart)]);
                self.merge_icon_links(vec![folder], links, cx);
            }
            Ok(IconSaved::Manifest(read_back)) => {
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
                self.restore_project_icon(&folder, shown, cx);
                reader_toast::error(
                    format!("Cannot change the icon of {}: {error:#}", project.name),
                    window,
                    cx,
                );
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
        // The retained document will not lay out behind Board. A previous
        // landing must not keep polling its now-hidden TextView.
        if !self.current_rel.is_empty() {
            let position = self
                .navigation
                .pending_landing
                .unwrap_or_else(|| self.content.read(cx).list_state().logical_scroll_top());
            self.navigation.places.insert(
                (self.vault_root.clone(), self.current_rel.clone()),
                position,
            );
        }
        self.cancel_pending_landing();
        // A note requested before this explicit project selection cannot
        // replace its Board when its asynchronous preparation finishes later.
        self.navigation.preparation_generation =
            self.navigation.preparation_generation.wrapping_add(1);
        self.tree.scope_to(relative.as_str());
        self.tree_revealed = self.selected_file().to_owned();
        self.panel_widths.backlinks = self.panel_widths.backlinks.max(560.);
        self.tree_sticky.clear();
        self.panels.close(reader_layout::Panel::Notes);
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
        self.focus_handle.focus(window, cx);
        self.refresh_project_icon(project.clone(), window, cx);
        self.ensure_project_board(project.folder.clone(), window, cx);
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
        // «+ New project…» asks for the folder (#1475).
        cx.subscribe_in(&picker, window, |this, _, _: &NewProject, window, cx| {
            this.open_new_project_dialog(None, window, cx);
        })
        .detach();
        picker
    }

    pub(super) fn sync_projects_picker(&mut self, cx: &mut Context<Self>) {
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

#[cfg(test)]
mod icon_save_tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::TestAppContext;
    use okilum_t3::projects::icon::{Avatar, IconColor};
    use std::sync::Mutex;

    type GatewayResult<T> = std::result::Result<T, okilum_t3::GatewayError>;

    /// T3 with one project, `okilum`. `refuse` answers the icon update as
    /// T3 does for a client without full access.
    #[derive(Default)]
    struct IconT3 {
        refuse: bool,
        fail_list: bool,
        reads: std::sync::atomic::AtomicUsize,
        icon: Mutex<Option<ProjectIcon>>,
        writes: Mutex<Vec<Option<ProjectIcon>>>,
    }
    impl IconT3 {
        fn project(&self) -> okilum_t3::Project {
            okilum_t3::Project {
                id: "okilum".into(),
                title: "Okilum".into(),
                workspace_root: "/srv/t3-host/okilum".into(),
                project_icon: self.icon.lock().unwrap().clone(),
                ..Default::default()
            }
        }
    }
    impl okilum_t3::ManagerGateway for IconT3 {
        fn status(&self) -> okilum_t3::GatewayStatus {
            okilum_t3::GatewayStatus::Connected {
                expires_at: u64::MAX,
            }
        }
        fn server_identity(&self) -> GatewayResult<okilum_t3::ServerIdentity> {
            panic!("unused identity")
        }
        fn list_projects(&self) -> GatewayResult<Vec<okilum_t3::Project>> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_list {
                return Err(okilum_t3::GatewayError::Unreachable("offline".into()));
            }
            Ok(vec![self.project()])
        }
        fn set_project_icon(
            &self,
            project_id: &str,
            icon: Option<&ProjectIcon>,
        ) -> GatewayResult<okilum_t3::Project> {
            assert_eq!(project_id, "okilum");
            self.writes.lock().unwrap().push(icon.cloned());
            if self.refuse {
                return Err(okilum_t3::GatewayError::Tool {
                    code: "capability_denied".into(),
                    message: "Changing projects needs a full-access client.".into(),
                });
            }
            *self.icon.lock().unwrap() = icon.cloned();
            Ok(self.project())
        }
        fn list_threads(&self, _: &str) -> GatewayResult<Vec<okilum_t3::ThreadSummary>> {
            panic!("unused threads")
        }
        fn read_thread(&self, _: &okilum_t3::ThreadQuery) -> GatewayResult<okilum_t3::ThreadPage> {
            panic!("unused read")
        }
        fn send_message(
            &self,
            _: &okilum_t3::OutgoingMessage,
        ) -> GatewayResult<okilum_t3::SendReceipt> {
            panic!("unused send")
        }
        fn pending_requests(&self, _: &str) -> GatewayResult<Vec<String>> {
            panic!("unused questions")
        }
        fn capabilities(&self) -> GatewayResult<okilum_t3::Capabilities> {
            panic!("unused catalog")
        }
        fn thread_configuration(&self, _: &str) -> GatewayResult<okilum_t3::ThreadConfiguration> {
            panic!("unused configuration")
        }
        fn configure_model(
            &self,
            _: &str,
            _: &okilum_t3::ModelSelection,
        ) -> GatewayResult<okilum_t3::ThreadConfiguration> {
            panic!("unused configure")
        }
    }

    fn connect(reader: &mut Reader, t3: &Arc<IconT3>) {
        let gateway: Arc<dyn okilum_t3::ManagerGateway> = t3.clone();
        reader.project_navigation.t3_gateways = Gateways::from([("test".to_owned(), gateway)]);
    }

    #[gpui::test]
    fn cached_project_icon_is_shown_before_network_and_survives_an_offline_refresh(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let state = temp.path().join("state");
        let folder = root.join("Dev/Areas/okilum");
        std::fs::create_dir_all(folder.join("ops")).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n").unwrap();
        std::fs::write(folder.join("ops/project.yaml"), "name: Okilum\nt3:\n  environment: test\n  origin: https://t3.example\n  executor: {project_id: okilum}\n").unwrap();
        let project = Project::read(&folder).unwrap();
        let old_icon = ProjectIcon::Emoji {
            emoji: "🚀".into()
        };
        let initial = okilum_t3::projects::icon::CounterpartRefresh {
            confirmed: [folder.clone()].into_iter().collect(),
            links: [(
                folder.clone(),
                Counterpart {
                    environment: "test".into(),
                    project_id: "okilum".into(),
                    icon: Some(old_icon.clone()),
                    logo: None,
                },
            )]
            .into_iter()
            .collect(),
        };
        store_project_icons(Some(&state), std::slice::from_ref(&project), &initial);
        let t3 = Arc::new(IconT3 {
            icon: Mutex::new(Some(ProjectIcon::Lucide {
                name: "calculator".into(),
                color: IconColor::Green,
            })),
            ..Default::default()
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, _, cx| r.ensure_palette_projects(cx));
        visual.run_until_parked();
        reader.read_with(visual, |_, cx| {
            assert_eq!(
                okilum_shell::project_avatar::avatar(&project, cx),
                Avatar::Icon(old_icon.clone())
            )
        });
        assert_eq!(
            t3.reads.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "cache displayed before any T3 request"
        );
        let gateway: Arc<dyn okilum_t3::ManagerGateway> = t3.clone();
        let gateways = Gateways::from([("test".into(), gateway)]);
        let fresh = refresh_project_icons(
            &root,
            std::slice::from_ref(&project),
            &gateways,
            Some(&state),
        );
        assert_eq!(
            t3.reads.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "positive control: refresh really contacts T3"
        );
        reader.update_in(visual, |r, _, cx| {
            r.merge_icon_links(fresh.confirmed.into_iter().collect(), fresh.links, cx)
        });
        let refreshed = Avatar::Icon(t3.icon.lock().unwrap().clone().unwrap());
        reader.read_with(visual, |_, cx| {
            assert_eq!(
                okilum_shell::project_avatar::avatar(&project, cx),
                refreshed
            )
        });
        let offline: Arc<dyn okilum_t3::ManagerGateway> = Arc::new(IconT3 {
            fail_list: true,
            ..Default::default()
        });
        let fresh = refresh_project_icons(
            &root,
            std::slice::from_ref(&project),
            &Gateways::from([("test".into(), offline)]),
            Some(&state),
        );
        assert!(fresh.confirmed.is_empty());
        reader.update_in(visual, |r, _, cx| {
            r.merge_icon_links(fresh.confirmed.into_iter().collect(), fresh.links, cx)
        });
        reader.read_with(visual, |_, cx| {
            assert_eq!(
                okilum_shell::project_avatar::avatar(&project, cx),
                refreshed
            )
        });
        assert_eq!(
            cached_project_icons(Some(&state), &[project])[&folder].icon,
            t3.icon.lock().unwrap().clone()
        );
    }

    /// #1627: Save shows the icon at once and writes it to T3; a refusal
    /// puts the old icon back and is a toast, never silence, and nothing
    /// is written to project.yaml in T3's place.
    #[gpui::test]
    fn a_saved_icon_shows_at_once_and_a_refused_one_is_put_back_with_a_toast(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let folder = root.join("Dev/Areas/okilum");
        std::fs::create_dir_all(folder.join("ops")).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n").unwrap();
        let manifest = folder.join("ops/project.yaml");
        std::fs::write(
            &manifest,
            "name: Okilum\nt3:\n  environment: test\n  executor: {project_id: okilum}\n",
        )
        .unwrap();
        let project = Project::read(&folder).unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();

        let icon = ProjectIcon::Lucide {
            name: "calculator".into(),
            color: IconColor::Green,
        };
        let t3 = Arc::new(IconT3::default());
        reader.update_in(visual, |r, window, cx| {
            connect(r, &t3);
            cx.default_global::<okilum_shell::project_avatar::ProjectIcons>()
                .links
                .insert(
                    folder.clone(),
                    Counterpart {
                        environment: "test".into(),
                        project_id: "okilum".into(),
                        icon: None,
                        logo: None,
                    },
                );
            r.save_project_icon(project.clone(), Some(icon.clone()), window, cx);
            assert_eq!(
                okilum_shell::project_avatar::avatar(&project, cx),
                Avatar::Icon(icon.clone()),
                "the icon shows before T3 answers"
            );
        });
        visual.run_until_parked();
        assert_eq!(*t3.writes.lock().unwrap(), [Some(icon.clone())]);
        reader.read_with(visual, |_, cx| {
            assert_eq!(
                okilum_shell::project_avatar::avatar(&project, cx),
                Avatar::Icon(icon.clone())
            );
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("reader-error-toast").is_none());

        let refusing = Arc::new(IconT3 {
            refuse: true,
            icon: Mutex::new(Some(icon.clone())),
            ..Default::default()
        });
        let rocket = ProjectIcon::Emoji {
            emoji: "🚀".into()
        };
        reader.update_in(visual, |r, window, cx| {
            connect(r, &refusing);
            r.save_project_icon(project.clone(), Some(rocket.clone()), window, cx);
            assert_eq!(
                okilum_shell::project_avatar::avatar(&project, cx),
                Avatar::Icon(rocket.clone())
            );
        });
        visual.run_until_parked();
        assert_eq!(*refusing.writes.lock().unwrap(), [Some(rocket.clone())]);
        reader.read_with(visual, |_, cx| {
            assert_eq!(
                okilum_shell::project_avatar::avatar(&project, cx),
                Avatar::Icon(icon.clone()),
                "the icon T3 kept is back"
            );
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("reader-error-toast").is_some(),
            "the refusal is a toast"
        );
        assert!(!std::fs::read_to_string(&manifest)
            .unwrap()
            .contains("icon:"));
    }
}

/// Test-only libc boundary (#1521): every outbound socket, name lookup and
/// process spawn of a process goes through `connect`, `getaddrinfo`,
/// `posix_spawn`/`posix_spawnp` or `fork`, whichever crate opens it
/// (reqwest/hyper/mio for MCP and the T3 session over HTTP, tungstenite and
/// std sockets for T3 streams, std `Command` for helper processes).
///
/// The interposers are not linked into any test binary: `SHIM` is compiled
/// at test time with the system C compiler into a shared object that only
/// the re-executed child of `local_reader_flows_open_no_socket_lookup_or_process`
/// loads, through `LD_PRELOAD`. Each forwards to libc unchanged and appends
/// one line per call to the file named by `EVENTS`.
#[cfg(all(test, target_os = "linux"))]
mod libc_boundary {
    use std::path::{Path, PathBuf};

    /// The file the preloaded shim appends its events to.
    pub(super) const EVENTS: &str = "OKILUM_1521_LIBC_EVENTS";

    const SHIM: &str = r#"
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

static void record(const char *event) {
    const char *path = getenv("OKILUM_1521_LIBC_EVENTS");
    if (!path) return;
    char line[512];
    int length = snprintf(line, sizeof line, "%s\n", event);
    if (length <= 0) return;
    if (length >= (int)sizeof line) length = sizeof line - 1;
    int fd = open(path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0600);
    if (fd < 0) return;
    ssize_t written = write(fd, line, (size_t)length);
    (void)written;
    close(fd);
}

static void *real(void **cell, const char *name) {
    if (!*cell) *cell = dlsym(RTLD_NEXT, name);
    if (!*cell) abort();
    return *cell;
}

int connect(int fd, const struct sockaddr *address, socklen_t length) {
    static void *cell;
    char event[400] = "connect unknown";
    char host[INET6_ADDRSTRLEN] = "";
    if (address && length >= sizeof(sa_family_t)) {
        if (address->sa_family == AF_INET) {
            const struct sockaddr_in *ip = (const void *)address;
            inet_ntop(AF_INET, &ip->sin_addr, host, sizeof host);
            snprintf(event, sizeof event, "connect %s:%u", host, ntohs(ip->sin_port));
        } else if (address->sa_family == AF_INET6) {
            const struct sockaddr_in6 *ip = (const void *)address;
            inet_ntop(AF_INET6, &ip->sin6_addr, host, sizeof host);
            snprintf(event, sizeof event, "connect [%s]:%u", host, ntohs(ip->sin6_port));
        } else if (address->sa_family == AF_UNIX) {
            const struct sockaddr_un *unix_address = (const void *)address;
            snprintf(event, sizeof event, "connect unix:%.300s", unix_address->sun_path);
        } else {
            snprintf(event, sizeof event, "connect family %d", address->sa_family);
        }
    }
    record(event);
    int (*forward)(int, const struct sockaddr *, socklen_t) = real(&cell, "connect");
    return forward(fd, address, length);
}

int getaddrinfo(const char *node, const char *service, const struct addrinfo *hints,
                struct addrinfo **result) {
    static void *cell;
    char event[400];
    snprintf(event, sizeof event, "getaddrinfo %.300s", node ? node : "");
    record(event);
    int (*forward)(const char *, const char *, const struct addrinfo *, struct addrinfo **) =
        real(&cell, "getaddrinfo");
    return forward(node, service, hints, result);
}

typedef int (*spawn_fn)(pid_t *, const char *, const posix_spawn_file_actions_t *,
                        const posix_spawnattr_t *, char *const[], char *const[]);

int posix_spawn(pid_t *pid, const char *path, const posix_spawn_file_actions_t *actions,
                const posix_spawnattr_t *attributes, char *const argv[], char *const envp[]) {
    static void *cell;
    char event[400];
    snprintf(event, sizeof event, "posix_spawn %.300s", path);
    record(event);
    spawn_fn forward = real(&cell, "posix_spawn");
    return forward(pid, path, actions, attributes, argv, envp);
}

int posix_spawnp(pid_t *pid, const char *file, const posix_spawn_file_actions_t *actions,
                 const posix_spawnattr_t *attributes, char *const argv[], char *const envp[]) {
    static void *cell;
    char event[400];
    snprintf(event, sizeof event, "posix_spawnp %.300s", file);
    record(event);
    spawn_fn forward = real(&cell, "posix_spawnp");
    return forward(pid, file, actions, attributes, argv, envp);
}

/* std Command forks when it cannot use posix_spawn (uid, pre_exec);
   recorded before the fork. */
pid_t fork(void) {
    static void *cell;
    record("fork");
    pid_t (*forward)(void) = real(&cell, "fork");
    return forward();
}
"#;

    /// Compiles the shim into `directory`. A missing C compiler fails the
    /// test: without the shim, an empty event list would prove nothing.
    pub(super) fn build(directory: &Path) -> PathBuf {
        let source = directory.join("okilum-1521-shim.c");
        let shim = directory.join("okilum-1521-shim.so");
        std::fs::write(&source, SHIM).unwrap();
        let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
        let output = std::process::Command::new(&compiler)
            .args(["-shared", "-fPIC", "-O1", "-Wall", "-Werror", "-o"])
            .arg(&shim)
            .arg(&source)
            .arg("-ldl")
            .output()
            .unwrap_or_else(|error| panic!("cannot run the C compiler {compiler:?}: {error}"));
        assert!(
            output.status.success(),
            "the libc boundary shim does not compile\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        shim
    }

    fn path() -> PathBuf {
        std::env::var_os(EVENTS)
            .expect("the child runs with the libc boundary shim")
            .into()
    }

    /// Forgets what the child did before the flows under test.
    pub(super) fn arm() {
        std::fs::write(path(), "").unwrap();
    }

    /// What the child attempted since `arm`, failed attempts included.
    pub(super) fn events() -> Vec<String> {
        std::fs::read_to_string(path())
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub(super) fn spawns() -> usize {
        events()
            .iter()
            .filter(|event| event.starts_with("posix_spawn") || event.as_str() == "fork")
            .count()
    }
}

/// #1521: Projects and T3 are optional. A plain local Reader never contacts
/// them on its own; only an explicit Projects view does.
///
/// Two instruments, each with a positive control:
/// - `t3_probe` sees every entry into this file's T3 connection points
///   (`t3_gateways`, `manager_gateway`) and can block or gate them;
/// - on Linux, the `libc_boundary` shim, preloaded into a dedicated child
///   process only, sees every socket connect, name lookup and process
///   spawn of the same flows with the real T3 transports, so no path
///   elsewhere in the shell (MCP client, T3 session HTTP, T3 streams,
///   Board/native clients, Kibana, link fetches, helper processes) can
///   reach out unnoticed.
#[cfg(test)]
mod local_reader_without_t3_tests {
    use super::t3_probe::{self, Probe};
    use super::*;
    use crate::quick_open::{PaletteItem, PaletteSection};
    use ::core::prelude::v1::test;
    use gpui::{TestAppContext, VisualTestContext};
    use std::sync::atomic::Ordering::SeqCst;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    type GatewayResult<T> = std::result::Result<T, okilum_t3::GatewayError>;

    const UNREACHABLE: &str = "test T3 endpoint is unreachable";
    /// How long the blocking fake T3 holds the thread that calls it.
    const HUNG_T3: Duration = Duration::from_secs(30);
    /// The least slack over the same-host baseline a blocked run gets; only
    /// a wait on `HUNG_T3` could use it up.
    const MARGIN: Duration = Duration::from_secs(5);
    /// How long the libc boundary child may run before it is killed.
    const CHILD_DEADLINE: Duration = Duration::from_secs(60);
    /// How long a listener may take to report a connection made to it.
    const READY: Duration = Duration::from_secs(2);

    /// T3 that never answers: every attempted operation is recorded, then
    /// blocks for `delay`, if set, and fails as an unreachable server does.
    #[derive(Default)]
    struct UnreachableT3 {
        calls: Mutex<Vec<&'static str>>,
        delay: Mutex<Option<Duration>>,
    }
    impl UnreachableT3 {
        fn attempt(&self, operation: &'static str) {
            self.calls.lock().unwrap().push(operation);
            let delay = *self.delay.lock().unwrap();
            if let Some(delay) = delay {
                std::thread::sleep(delay);
            }
        }
        fn fail<T>(&self, operation: &'static str) -> GatewayResult<T> {
            self.attempt(operation);
            Err(okilum_t3::GatewayError::Unreachable(UNREACHABLE.into()))
        }
        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }
    impl okilum_t3::ManagerGateway for UnreachableT3 {
        fn start_manager(&self, _: &str, _: &str, _: &str) -> GatewayResult<String> {
            self.fail("start_manager")
        }
        fn status(&self) -> okilum_t3::GatewayStatus {
            self.attempt("status");
            okilum_t3::GatewayStatus::NotConnected
        }
        fn token_expires_at(&self) -> Option<u64> {
            self.attempt("token_expires_at");
            None
        }
        fn server_identity(&self) -> GatewayResult<okilum_t3::ServerIdentity> {
            self.fail("server_identity")
        }
        fn list_projects(&self) -> GatewayResult<Vec<okilum_t3::Project>> {
            self.fail("list_projects")
        }
        fn read_project(&self, _: &str) -> GatewayResult<okilum_t3::Project> {
            self.fail("read_project")
        }
        fn set_project_icon(
            &self,
            _: &str,
            _: Option<&ProjectIcon>,
        ) -> GatewayResult<okilum_t3::Project> {
            self.fail("set_project_icon")
        }
        fn create_project(&self, _: &str, _: &str) -> GatewayResult<okilum_t3::Project> {
            self.fail("create_project")
        }
        fn list_threads(&self, _: &str) -> GatewayResult<Vec<okilum_t3::ThreadSummary>> {
            self.fail("list_threads")
        }
        fn read_thread(&self, _: &okilum_t3::ThreadQuery) -> GatewayResult<okilum_t3::ThreadPage> {
            self.fail("read_thread")
        }
        fn send_message(
            &self,
            _: &okilum_t3::OutgoingMessage,
        ) -> GatewayResult<okilum_t3::SendReceipt> {
            self.fail("send_message")
        }
        fn pending_requests(&self, _: &str) -> GatewayResult<Vec<String>> {
            self.fail("pending_requests")
        }
        fn capabilities(&self) -> GatewayResult<okilum_t3::Capabilities> {
            self.fail("capabilities")
        }
        fn thread_configuration(&self, _: &str) -> GatewayResult<okilum_t3::ThreadConfiguration> {
            self.fail("thread_configuration")
        }
        fn configure_model(
            &self,
            _: &str,
            _: &okilum_t3::ModelSelection,
        ) -> GatewayResult<okilum_t3::ThreadConfiguration> {
            self.fail("configure_model")
        }
    }

    /// The configured T3 origin: a loopback listener that only counts the
    /// connections something opened to it.
    struct Endpoint {
        listener: std::net::TcpListener,
        accepted: usize,
    }
    impl Endpoint {
        fn new() -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            Self {
                listener,
                accepted: 0,
            }
        }
        fn origin(&self) -> String {
            format!("http://{}", self.listener.local_addr().unwrap())
        }
        /// Connections accepted so far, waiting up to `READY` for `expected`.
        fn accepted_within(&mut self, expected: usize) -> usize {
            let deadline = Instant::now() + READY;
            loop {
                while let Ok((stream, _)) = self.listener.accept() {
                    drop(stream);
                    self.accepted += 1;
                }
                if self.accepted >= expected || Instant::now() >= deadline {
                    return self.accepted;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn probe(t3: &Arc<UnreachableT3>) -> Arc<Probe> {
        let gateway: Arc<dyn okilum_t3::ManagerGateway> = t3.clone();
        Arc::new(Probe {
            gateway: Some(gateway),
            ..Default::default()
        })
    }

    /// Nothing reached T3: no sweep, connection, transport or operation.
    fn assert_no_t3(probe: &Probe, t3: &UnreachableT3, reader: &Reader, after: &str) {
        assert_eq!(probe.sweeps.load(SeqCst), 0, "{after}: no T3 sweep");
        assert!(
            probe.connects.lock().unwrap().is_empty(),
            "{after}: no T3 connection"
        );
        assert_eq!(
            probe.manager_gateways.load(SeqCst),
            0,
            "{after}: no Manager transport"
        );
        assert_eq!(t3.calls(), [] as [&str; 0], "{after}: no T3 operation");
        assert_quiet_reader(reader, after);
    }

    /// No T3 state, status or panel anywhere in the Reader.
    fn assert_quiet_reader(reader: &Reader, after: &str) {
        let navigation = &reader.project_navigation;
        assert!(navigation.t3_gateways.is_empty(), "{after}");
        assert!(navigation.statuses.is_empty(), "{after}: no T3 status");
        assert!(navigation.manager_panel.is_none(), "{after}");
        assert!(navigation.boards.is_empty(), "{after}");
        assert!(navigation.active.is_none(), "{after}");
    }

    fn assert_no_error_toast(visual: &mut VisualTestContext, after: &str) {
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("reader-error-toast").is_none(),
            "{after}: no T3 prompt or error"
        );
    }

    fn local_vault(root: &Path) {
        std::fs::create_dir_all(root.join("Notes")).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n\nA plain local note.\n").unwrap();
        std::fs::write(
            root.join("Notes/Second.md"),
            "# Second\n\nmeridian lives here\n",
        )
        .unwrap();
        std::fs::write(
            root.join("app.log"),
            "2026-10-30T10:00:00Z INFO reader started\n2026-10-30T10:00:01Z WARN local only\n",
        )
        .unwrap();
    }

    /// A project with a manager thread, its T3 at `origin`, saved as a
    /// connection too.
    fn configured_project(root: &Path, state: &Path, origin: &str) {
        let folder = root.join("Dev/Areas/okilum");
        std::fs::create_dir_all(folder.join("ops")).unwrap();
        std::fs::write(
            folder.join("ops/project.yaml"),
            format!(
                "name: Okilum\nkind: knowledge\nt3:\n  schema_version: 1\n  environment: test\n  origin: {origin}\n  manager: {{project_id: okilum, thread_id: 'mcp:manager', workspace_mode: local}}\n"
            ),
        )
        .unwrap();
        std::fs::create_dir_all(state).unwrap();
        std::fs::write(
            state.join("t3-connections.json"),
            serde_json::to_vec(&std::collections::BTreeMap::from([("test", origin)])).unwrap(),
        )
        .unwrap();
    }

    fn init(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
    }

    fn open_reader<'a>(
        cx: &'a mut TestAppContext,
        root: &Path,
        state: &Path,
    ) -> (Entity<Reader>, &'a mut VisualTestContext) {
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.to_owned()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(state.with_extension("index")),
                        session_directory: Some(state.to_owned()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        (entity.unwrap(), visual)
    }

    fn search(reader: &Entity<Reader>, visual: &mut VisualTestContext, text: &str) -> Vec<String> {
        reader.update_in(visual, |r, window, cx| {
            assert!(r.searcher.is_some(), "the index publishes before searching");
            r.open_quick_open(true, window, cx);
            let text = text.to_owned();
            r.quick_open
                .input
                .update(cx, |input, cx| input.set_value(text, window, cx));
            r.refresh_quick_open(cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            let rows = r
                .quick_open
                .rows
                .iter()
                .map(|hit| hit.path.clone())
                .collect();
            r.close_quick_open(window, cx);
            rows
        })
    }

    /// The ordinary local paths after startup, each through the Reader's own
    /// entry point: a note, search, Recent and Pinned in the command
    /// palette, and a log from the Logs section.
    fn use_local_reader(reader: &Entity<Reader>, visual: &mut VisualTestContext) {
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Notes/Second.md", None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "Notes/Second.md"));

        assert_eq!(search(reader, visual, "meridian"), ["Notes/Second.md"]);

        reader.update_in(visual, |r, window, cx| {
            r.open_note("start.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.toggle_pin("Notes/Second.md", cx);
            r.open_quick_open(false, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert!(
                r.sidebar.recent.iter().any(|(p, _)| p == "Notes/Second.md"),
                "the opened note is a recent"
            );
            let notes: Vec<_> = r
                .palette_items()
                .into_iter()
                .enumerate()
                .filter_map(|(ix, (section, item))| match item {
                    PaletteItem::Note(entry) => Some((ix, section, entry.path)),
                    _ => None,
                })
                .collect();
            let pinned = notes
                .iter()
                .find(|(_, section, path)| {
                    *section == PaletteSection::Pinned && path == "Notes/Second.md"
                })
                .unwrap_or_else(|| panic!("the pin is in the palette: {notes:?}"));
            r.open_palette_item(pinned.0, window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(!r.quick_open.open);
            assert_eq!(r.current_rel, "Notes/Second.md", "the pin opened");
        });

        reader.update_in(visual, |r, window, cx| {
            r.show_app_section(Section::Logs, window, cx)
        });
        visual.run_until_parked();
        // As the Logs section's row does.
        reader.update_in(visual, |r, window, cx| {
            r.project_navigation.section = Section::Reader;
            r.preview_file("app.log", window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            let preview = r.file_preview.as_ref().expect("the log is open");
            assert_eq!(preview.rel, "app.log");
            assert!(preview.log.is_some(), "the log viewer reads it");
        });
    }

    /// Startup and the local paths of a fresh profile in `directory`: no
    /// project.yaml, no saved T3 connection. The T3 counters are checked
    /// after each stage; the elapsed time is returned for the backstop.
    fn fresh_local_run<'a>(
        cx: &'a mut TestAppContext,
        directory: &Path,
        probe: &Probe,
        t3: &UnreachableT3,
    ) -> (Entity<Reader>, &'a mut VisualTestContext, Duration) {
        let root = directory.join("vault");
        local_vault(&root);
        let state = directory.join("state");
        let started = Instant::now();
        let (reader, visual) = open_reader(cx, &root, &state);
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert_no_t3(probe, t3, r, "startup"));
        assert_no_error_toast(visual, "startup");
        use_local_reader(&reader, visual);
        let elapsed = started.elapsed();
        reader.read_with(visual, |r, _| {
            assert_no_t3(probe, t3, r, "local paths");
            let navigation = &r.project_navigation;
            assert!(navigation.discovered, "the palette scanned for projects");
            assert!(navigation.discovery.projects.is_empty());
            assert!(navigation.picker.is_none() && navigation.switcher.is_none());
        });
        assert_no_error_toast(visual, "local paths");
        assert!(!state.join("t3-connections.json").exists());
        (reader, visual, elapsed)
    }

    /// A fresh profile never contacts T3, so it never waits on it either:
    /// the counters are the assertion. The backstop times the same flows on
    /// this host twice, first with a T3 that answers at once, then with one
    /// that blocks every sweep and call for `HUNG_T3`; the blocked run must
    /// stay within the baseline plus a generous margin and below `HUNG_T3`.
    #[gpui::test]
    fn a_fresh_local_profile_never_contacts_or_waits_on_t3(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let t3 = Arc::new(UnreachableT3::default());
        let probe = probe(&t3);
        let _installed = t3_probe::install(probe.clone());
        init(cx);

        let (_, _, baseline) = fresh_local_run(cx, &temp.path().join("baseline"), &probe, &t3);
        *probe.delay.lock().unwrap() = Some(HUNG_T3);
        *t3.delay.lock().unwrap() = Some(HUNG_T3);
        let (reader, visual, blocked) =
            fresh_local_run(cx, &temp.path().join("blocked"), &probe, &t3);
        let bound = baseline + baseline.max(MARGIN);
        eprintln!(
            "#1521 same-host timing: baseline {baseline:?}, blocking T3 {blocked:?}, bound {bound:?}"
        );
        assert!(
            blocked < bound && blocked < HUNG_T3,
            "with T3 blocked the local paths took {blocked:?} (baseline {baseline:?}, bound {bound:?}): they waited on T3"
        );

        // Positive control, same instrument: the Projects view asks T3, and
        // a sweep really waits on the hung T3 (shortened here).
        let control = Duration::from_millis(300);
        *probe.delay.lock().unwrap() = Some(control);
        let opened = Instant::now();
        reader.update_in(visual, |r, window, cx| {
            r.show_app_section(Section::Projects, window, cx)
        });
        visual.run_until_parked();
        assert!(opened.elapsed() >= control, "the sweep waited on T3");
        assert_eq!(probe.sweeps.load(SeqCst), 1, "the Projects view asks T3");
        assert!(
            probe.connects.lock().unwrap().is_empty(),
            "nothing is configured to connect to"
        );
        assert_eq!(t3.calls(), [] as [&str; 0]);
        reader.read_with(visual, |r, _| {
            assert!(r.project_navigation.picker.is_some());
            assert!(r.project_navigation.statuses.is_empty());
        });
        assert!(!temp
            .path()
            .join("blocked/state/t3-connections.json")
            .exists());
    }

    /// Projects configured, T3 unreachable: the Reader stays local and
    /// usable, also while the Projects view waits on a blocked T3, and the
    /// unreachable status shows in the Projects view only, once opened.
    #[gpui::test]
    fn configured_projects_stay_lazy_and_an_unreachable_t3_shows_only_in_projects(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        local_vault(&root);
        let mut endpoint = Endpoint::new();
        let origin = endpoint.origin();
        let state = temp.path().join("state");
        configured_project(&root, &state, &origin);
        let t3 = Arc::new(UnreachableT3::default());
        let probe = probe(&t3);
        let (release, gate) = async_channel::bounded(1);
        *probe.gate.lock().unwrap() = Some(gate);
        let _installed = t3_probe::install(probe.clone());
        init(cx);

        let (reader, visual) = open_reader(cx, &root, &state);
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert_no_t3(&probe, &t3, r, "startup"));
        assert_no_error_toast(visual, "startup");

        use_local_reader(&reader, visual);
        reader.read_with(visual, |r, _| {
            assert_no_t3(&probe, &t3, r, "local paths");
            assert!(r.project_navigation.discovered);
            let rows = r.palette_projects("", 8);
            assert_eq!(
                rows.iter()
                    .map(|row| row.project.name.as_str())
                    .collect::<Vec<_>>(),
                ["Okilum"],
                "the palette discovered the local project without T3"
            );
            assert!(r.project_navigation.picker.is_none());
        });
        assert_no_error_toast(visual, "local paths");

        // Positive control, same instrument: the Projects view asks T3,
        // whose answer is held back.
        reader.update_in(visual, |r, window, cx| {
            r.show_app_section(Section::Projects, window, cx)
        });
        visual.run_until_parked();
        assert_eq!(probe.sweeps.load(SeqCst), 1, "the Projects view asks T3");
        assert!(
            probe.connects.lock().unwrap().is_empty(),
            "T3 is still blocked"
        );

        // The blocked sweep does not hold up the local Reader.
        reader.update_in(visual, |r, window, cx| {
            r.open_note("start.md", None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "start.md"));
        assert_eq!(search(&reader, visual, "meridian"), ["Notes/Second.md"]);
        reader.read_with(visual, |r, _| {
            assert!(
                r.project_navigation.statuses.is_empty(),
                "T3 is still blocked"
            );
        });
        assert_eq!(t3.calls(), [] as [&str; 0]);
        assert_no_error_toast(visual, "a blocked T3");

        release.try_send(()).unwrap();
        visual.run_until_parked();
        assert_eq!(
            *probe.connects.lock().unwrap(),
            [("test".to_owned(), origin.clone())]
        );
        assert!(
            t3.calls().contains(&"list_threads"),
            "the sweep asked T3: {:?}",
            t3.calls()
        );
        assert_eq!(probe.manager_gateways.load(SeqCst), 0);
        reader.read_with(visual, |r, cx| {
            let navigation = &r.project_navigation;
            let folder = &navigation.discovery.projects[0].folder;
            assert!(
                matches!(
                    navigation.statuses.get(folder),
                    Some(okilum_t3::projects::Status::Unreachable(reason)) if reason.contains(UNREACHABLE)
                ),
                "{:?}",
                navigation.statuses
            );
            let picker = navigation.picker.as_ref().unwrap().read(cx);
            assert!(
                picker
                    .model()
                    .manager_detail(folder)
                    .is_some_and(|detail| detail.contains(UNREACHABLE)),
                "the Projects view says T3 is unreachable"
            );
            assert!(navigation.active.is_none() && navigation.manager_panel.is_none());
        });
        assert_no_error_toast(visual, "an unreachable T3");

        // No real transport bypassed the fake. Positive control: a
        // connection to the endpoint is counted once the listener has it.
        assert_eq!(
            endpoint.accepted_within(0),
            0,
            "nothing reached the T3 origin"
        );
        let _client =
            std::net::TcpStream::connect(endpoint.listener.local_addr().unwrap()).unwrap();
        assert_eq!(endpoint.accepted_within(1), 1);
    }

    const BOUNDARY_CHILD: &str = "OKILUM_1521_LIBC_BOUNDARY_CHILD";

    /// The same flows with the real T3 transports and no fake, watched at
    /// the libc boundary (see `libc_boundary`) in a child process of their
    /// own, run with a deadline and killed and reaped past it: a fresh profile, then a configured project whose T3 refuses
    /// every connection. Neither opens a socket, looks up a name or spawns
    /// a process; the Projects view then really connects to the configured
    /// origin, and only there shows T3 as unreachable.
    #[cfg(target_os = "linux")]
    #[gpui::test]
    fn local_reader_flows_open_no_socket_lookup_or_process(cx: &mut TestAppContext) {
        const NAME: &str = "reader_project_navigation::local_reader_without_t3_tests::local_reader_flows_open_no_socket_lookup_or_process";
        if std::env::var_os(BOUNDARY_CHILD).is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(BOUNDARY_CHILD, "1");
            // Loopback only: no proxy may stand between reqwest and the origin.
            for proxy in [
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "http_proxy",
                "https_proxy",
                "all_proxy",
            ] {
                child.env_remove(proxy);
            }
            let scratch = tempfile::tempdir().unwrap();
            let shim = super::libc_boundary::build(scratch.path());
            let (stdout, stderr) = (scratch.path().join("stdout"), scratch.path().join("stderr"));
            child
                .env("LD_PRELOAD", &shim)
                .env(super::libc_boundary::EVENTS, scratch.path().join("events"))
                .stdin(std::process::Stdio::null())
                .stdout(std::fs::File::create(&stdout).unwrap())
                .stderr(std::fs::File::create(&stderr).unwrap());
            let mut running = child.spawn().expect("run the libc boundary child");
            let deadline = Instant::now() + CHILD_DEADLINE;
            let status = loop {
                if let Some(status) = running.try_wait().unwrap() {
                    break Some(status);
                }
                if Instant::now() >= deadline {
                    let _ = running.kill();
                    running.wait().expect("reap the killed child");
                    break None;
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            let stdout = std::fs::read_to_string(&stdout).unwrap_or_default();
            let stderr = std::fs::read_to_string(&stderr).unwrap_or_default();
            let Some(status) = status else {
                panic!("libc boundary child killed after {CHILD_DEADLINE:?}\n{stdout}\n{stderr}");
            };
            assert!(
                status.success() && stdout.contains("test result: ok. 1 passed"),
                "libc boundary child failed: {status}\n{stdout}\n{stderr}"
            );
            return;
        }

        super::libc_boundary::arm();
        // Real transports, with the token a connected keychain would hold.
        let probe = Arc::new(Probe {
            token: Some(okilum_t3::StoredToken {
                access_token: "synthetic-1521".into(),
                expires_at: u64::MAX,
                scope: "mcp".into(),
                client_id: "okilum-test".into(),
            }),
            ..Default::default()
        });
        let _installed = t3_probe::install(probe.clone());
        init(cx);
        let temp = tempfile::tempdir().unwrap();

        let fresh = temp.path().join("fresh");
        local_vault(&fresh);
        let fresh_state = temp.path().join("fresh-state");
        {
            let (reader, visual) = open_reader(cx, &fresh, &fresh_state);
            visual.run_until_parked();
            use_local_reader(&reader, visual);
            reader.read_with(visual, |r, _| assert_quiet_reader(r, "fresh profile"));
            assert_no_error_toast(visual, "fresh profile");
        }
        assert_eq!(
            super::libc_boundary::events(),
            [] as [String; 0],
            "a fresh profile reaches out to nothing"
        );

        // T3 accepts and drops every connection, so each attempt fails fast.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = {
            let (accepted, stop) = (accepted.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            drop(stream);
                            accepted.fetch_add(1, SeqCst);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            })
        };
        let configured = temp.path().join("configured");
        local_vault(&configured);
        let configured_state = temp.path().join("configured-state");
        configured_project(&configured, &configured_state, &format!("http://{address}"));
        let (reader, visual) = open_reader(cx, &configured, &configured_state);
        visual.run_until_parked();
        use_local_reader(&reader, visual);
        reader.read_with(visual, |r, _| {
            assert_quiet_reader(r, "configured project");
            assert_eq!(r.palette_projects("", 8).len(), 1, "discovered locally");
        });
        assert_no_error_toast(visual, "configured project");
        assert_eq!(
            super::libc_boundary::events(),
            [] as [String; 0],
            "a configured project is not contacted until asked"
        );
        assert_eq!(accepted.load(SeqCst), 0);
        assert_eq!(probe.sweeps.load(SeqCst), 0);

        // Positive control, same instrument: the Projects view connects to
        // the configured T3 through the real MCP and session transports.
        reader.update_in(visual, |r, window, cx| {
            r.show_app_section(Section::Projects, window, cx)
        });
        visual.run_until_parked();
        assert!(probe.sweeps.load(SeqCst) > 0, "the Projects view asks T3");
        assert_eq!(
            *probe.connects.lock().unwrap(),
            [("test".to_owned(), format!("http://{address}"))]
        );
        let events = super::libc_boundary::events();
        assert!(
            events.contains(&format!("connect {address}")),
            "the Projects view connects to T3: {events:?}"
        );
        assert!(
            events
                .iter()
                .all(|event| event == &format!("connect {address}")),
            "nothing but the configured T3: {events:?}"
        );
        let deadline = Instant::now() + READY;
        while accepted.load(SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            accepted.load(SeqCst) > 0,
            "the T3 origin saw the connection"
        );
        stop.store(true, SeqCst);
        server.join().unwrap();
        reader.read_with(visual, |r, cx| {
            let navigation = &r.project_navigation;
            let folder = &navigation.discovery.projects[0].folder;
            assert!(
                matches!(
                    navigation.statuses.get(folder),
                    Some(okilum_t3::projects::Status::Unreachable(_))
                ),
                "{:?}",
                navigation.statuses
            );
            let picker = navigation.picker.as_ref().unwrap().read(cx);
            assert!(
                picker.model().manager_detail(folder).is_some(),
                "the Projects view says why T3 is unreachable"
            );
            assert!(navigation.active.is_none() && navigation.manager_panel.is_none());
        });
        assert_no_error_toast(visual, "an unreachable T3");

        // Positive controls for the T3 stream transport, lookup and spawn.
        let before = super::libc_boundary::events().len();
        let _ = tungstenite::connect(format!("ws://{address}/"));
        assert!(super::libc_boundary::events()[before..].contains(&format!("connect {address}")));
        let before = super::libc_boundary::events().len();
        let _ = std::net::ToSocketAddrs::to_socket_addrs(&("localhost", 80));
        assert!(super::libc_boundary::events()[before..]
            .iter()
            .any(|event| event == "getaddrinfo localhost"));
        assert_eq!(super::libc_boundary::spawns(), 0);
        let _ = std::process::Command::new("true").status();
        assert!(super::libc_boundary::spawns() > 0, "a spawn is seen");
    }
}
