//! «Make this a project…» on a folder and «+ New project…» under the
//! Projects list (#1475): the Reader's side of the New project dialog.
//!
//! Local creation is independent of T3. A separate explicit local binding action
//! picks an existing verified owner without creating a T3 project or thread.
use super::*;
use crate::reader_project_navigation::{t3_gateways, Gateways};
use okilum_shell::new_project_dialog::{NewProjectDialog, NewProjectEvent, TITLE};
use okilum_t3::projects::{
    create::{self, Created, Draft, T3Created, T3Link},
    icon::Counterpart,
    local_binding::{Binding, Selection},
    Project,
};

/// The tree's folder menu item.
pub(super) const MAKE_PROJECT: &str = "Make this a project…";

type T3Outcome = anyhow::Result<(String, T3Created, anyhow::Result<Project>)>;

#[derive(Clone)]
struct LocalOwnerChoice {
    environment: okilum_t3::Environment,
    gateway: Arc<dyn okilum_t3::ManagerGateway>,
    selection: Selection,
    title: String,
}

/// Listing remains separate from binding: every candidate is still an
/// explicit choice, verified again by the revision-aware save operation.
fn verified_local_owner_choices(
    binding: &Binding,
    environment: okilum_t3::Environment,
    gateway: Arc<dyn okilum_t3::ManagerGateway>,
) -> anyhow::Result<(Vec<LocalOwnerChoice>, Vec<String>)> {
    okilum_t3::projects::local_manager::environment(&environment.origin())?;
    anyhow::ensure!(
        binding
            .pinned_environment()
            .is_none_or(|pinned| pinned == &environment),
        "Saved local endpoint changed; binding and drafts retained"
    );
    let identity = gateway.server_identity()?;
    let listing = gateway.list_projects()?;
    anyhow::ensure!(
        gateway.server_identity()?.environment_id == identity.environment_id,
        "T3 environment changed while loading Projects"
    );
    let mut choices = Vec::new();
    let mut unresolved = Vec::new();
    for target in &listing {
        if binding
            .pinned_owner()
            .is_some_and(|owner| owner.project_id != target.id)
        {
            continue;
        }
        // A path is validated only after the server identity has
        // been read. It never selects an owner automatically.
        let selected = Selection::select(
            &identity.environment_id,
            &target.id,
            Path::new(&target.workspace_root),
            &listing,
        )
        .and_then(|selection| {
            binding.check_selection(&environment, &selection)?;
            Ok(selection)
        });
        match selected {
            Ok(selection) => choices.push(LocalOwnerChoice {
                environment: environment.clone(),
                gateway: gateway.clone(),
                selection,
                title: target.title.clone(),
            }),
            Err(error) => unresolved.push(format!(
                "{} · {} · {}: {error:#}",
                environment.origin(),
                target.title,
                target.id
            )),
        }
    }
    if let Some(owner) = binding.pinned_owner() {
        if choices.is_empty() && unresolved.is_empty() {
            unresolved.push(format!("Saved local owner {} is absent from the complete project listing; binding and drafts retained", owner.project_id));
        }
    }
    Ok::<_, anyhow::Error>((choices, unresolved))
}

impl Reader {
    /// A held menu/dialog cannot bind a replaced project entry. The canonical
    /// manifest and Manager registry are checked again by Binding::save.
    pub(super) fn local_binding_target_current(&self, expected: &Project) -> bool {
        let mut found = false;
        for current in self
            .project_navigation
            .discovery
            .projects
            .iter()
            .chain(self.project_navigation.active.iter())
            .filter(|project| project.folder == expected.folder)
        {
            found = true;
            if current.environment != expected.environment
                || current.manager_project != expected.manager_project
                || current.manager != expected.manager
                || current.kind != expected.kind
                || current.appearance.source_checkout != expected.appearance.source_checkout
            {
                return false;
            }
        }
        found
    }
    /// The tree's folder menu offers «Make this a project…» only for a
    /// folder inside the vault that is not a project already.
    pub(super) fn can_make_project(&self, relative: &str) -> bool {
        if self.single_file || relative.starts_with('.') {
            return false;
        }
        let folder = self.vault_root.join(relative);
        !self
            .project_navigation
            .discovery
            .projects
            .iter()
            .any(|project| project.folder == folder)
            && !create::is_project(&folder)
    }

    /// «+ New project…» starts with the folder selected in the tree, else
    /// the open note's folder, else the tree's root.
    pub(super) fn default_new_project_folder(&self) -> String {
        self.tree
            .selected_creation_folder()
            .filter(|folder| !folder.is_empty())
            .or_else(|| {
                self.current_rel
                    .rsplit_once('/')
                    .map(|(folder, _)| folder.to_owned())
            })
            .unwrap_or_else(|| self.tree.scope().to_owned())
    }

    /// The New project dialog: `folder` (vault-relative) fixed from a
    /// folder's menu, or asked for when `None`.
    pub(super) fn open_new_project_dialog(
        &mut self,
        folder: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
        use gpui_component::WindowExt as _;
        let fixed = folder.is_some();
        let relative = folder.unwrap_or_else(|| self.default_new_project_folder());
        self.close_project_switcher(window, cx);
        self.file_menu = None;
        let vault = self.vault_root.clone();
        let dialog = cx.new(|cx| NewProjectDialog::new(&vault, &relative, fixed, window, cx));
        let reader = cx.entity().downgrade();
        let handle = dialog.downgrade();
        let local_binding = std::rc::Rc::new(std::cell::Cell::new(false));
        let local_intent = local_binding.clone();
        dialog.update(cx, |dialog, _| {
            dialog.on_choice(move |event, window, cx| {
                let handle = handle.clone();
                let delivered = match event {
                    NewProjectEvent::Cancel => {
                        window.close_dialog(cx);
                        Ok(())
                    }
                    NewProjectEvent::ChooseIcon => reader.update(cx, |this, cx| {
                        this.choose_new_project_icon(handle, window, cx)
                    }),
                    NewProjectEvent::Create { folder, draft } => {
                        let (folder, draft) = (folder.clone(), draft.clone());
                        reader.update(cx, |this, cx| {
                            if local_intent.replace(false) {
                                this.write_project(handle, folder, draft, true, window, cx);
                            } else {
                                this.create_project(handle, folder, draft, window, cx);
                            }
                        })
                    }
                };
                if delivered.is_err() {
                    eprintln!("New project: the window that asked is gone");
                }
            })
        });
        let content = dialog.clone();
        let local_dialog = dialog.clone();
        window.open_dialog(cx, move |d, _, _| {
            d.title(TITLE)
                .width(px(480.))
                .on_ok(|_, _, _| false)
                .child(content.clone())
                .child(
                    Button::new("create-and-bind-local")
                        .debug_selector(|| "create-and-bind-local".into())
                        .label("Create locally and connect an existing T3 project…")
                        .on_click({
                            let local_dialog = local_dialog.clone();
                            let local_binding = local_binding.clone();
                            move |_, window, cx| {
                                if local_binding.get() {
                                    return;
                                }
                                local_binding.set(true);
                                let submitted = local_dialog.update(cx, |dialog, cx| {
                                    dialog.set_create_in_t3(false, cx);
                                    dialog.create(window, cx)
                                });
                                if !submitted {
                                    local_binding.set(false);
                                }
                            }
                        }),
                )
        });
        window.defer(cx, move |window, cx| {
            dialog.update(cx, |dialog, cx| dialog.focus_first(window, cx));
        });
        cx.notify();
    }

    /// T3's «Choose project icon» over the New project dialog, the same
    /// component «Change icon…» opens (#1627). Save and Reset set the
    /// draft's icon; nothing is written yet.
    fn choose_new_project_icon(
        &mut self,
        dialog: WeakEntity<NewProjectDialog>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::WindowExt as _;
        use okilum_shell::project_icon_dialog::{IconDialogEvent, ProjectIconDialog};
        let Some(current) = dialog.upgrade() else {
            return;
        };
        let (name, icon) = {
            let draft = current.read(cx).draft();
            (draft.name.clone(), draft.icon.clone())
        };
        let picker = cx.new(|cx| ProjectIconDialog::new(&name, icon.as_ref(), window, cx));
        picker.update(cx, |picker, _| {
            picker.on_choice(move |event, window, cx| {
                let icon = match event {
                    IconDialogEvent::Cancel => {
                        window.close_dialog(cx);
                        return;
                    }
                    IconDialogEvent::Reset => None,
                    IconDialogEvent::Save(icon) => Some(icon.clone()),
                };
                window.close_dialog(cx);
                let _ = dialog.update(cx, |dialog, cx| dialog.set_icon(icon, cx));
            })
        });
        let content = picker.clone();
        window.open_dialog(cx, move |d, _, _| {
            d.title(okilum_shell::project_icon_dialog::TITLE)
                .width(px(520.))
                .on_ok(|_, _, _| false)
                .child(content.clone())
        });
        window.defer(cx, move |window, cx| {
            picker.update(cx, |picker, cx| {
                let tab = picker.draft().tab;
                picker.set_tab(tab, window, cx)
            });
        });
    }

    /// Write the project off the UI thread; the dialog closes on success
    /// and shows the error otherwise.
    pub(super) fn create_project(
        &mut self,
        dialog: WeakEntity<NewProjectDialog>,
        folder: PathBuf,
        draft: Draft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.write_project(dialog, folder, draft, false, window, cx);
    }

    fn write_project(
        &mut self,
        dialog: WeakEntity<NewProjectDialog>,
        folder: PathBuf,
        draft: Draft,
        local_binding: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vault = self.vault_root.clone();
        eprintln!(
            "New project: creating name={:?} kind={:?} folder={} t3={}",
            draft.name,
            draft.kind,
            folder.display(),
            draft.create_in_t3()
        );
        cx.spawn_in(window, async move |this, cx| {
            let written = cx
                .background_executor()
                .spawn({
                    let (vault, folder, draft) = (vault.clone(), folder.clone(), draft.clone());
                    async move { create::create(&vault, &folder, &draft) }
                })
                .await;
            let delivered = this.update_in_own_window(cx, |this, window, cx| {
                if this.vault_root == vault {
                    this.finish_create_project(dialog, written, draft, local_binding, window, cx);
                }
            });
            if let Err(error) = delivered {
                eprintln!("New project: the result did not reach the window: {error:#}");
            }
        })
        .detach();
    }

    fn finish_create_project(
        &mut self,
        dialog: WeakEntity<NewProjectDialog>,
        written: anyhow::Result<Created>,
        draft: Draft,
        local_binding: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::WindowExt as _;
        let created = match written {
            Ok(created) => created,
            Err(error) => {
                eprintln!("New project: not created: {error:#}");
                let _ = dialog.update(cx, |dialog, cx| {
                    dialog.set_error(Some(format!("{error:#}")), cx)
                });
                return;
            }
        };
        eprintln!(
            "New project: wrote {} index={}",
            created.project.folder.join(create::MANIFEST).display(),
            created.wrote_index
        );
        if dialog.upgrade().is_some() {
            window.close_dialog(cx);
        }
        let project = created.project;
        self.show_new_project(project.clone(), cx);
        reader_toast::transient(format!("{} is a project", project.name), window, cx);
        if local_binding {
            self.open_local_project_binding(project, window, cx);
        } else if draft.create_in_t3() {
            self.create_t3_project(project, draft, window, cx);
        }
    }

    /// The project is in the Projects list, the switcher and ⌘K at once;
    /// a project read again (its `t3:` block added) replaces itself.
    fn show_new_project(&mut self, project: Project, cx: &mut Context<Self>) {
        let navigation = &mut self.project_navigation;
        if let Some(active) = navigation
            .active
            .as_mut()
            .filter(|active| active.folder == project.folder)
        {
            *active = project.clone();
        }
        let projects = &mut navigation.discovery.projects;
        projects.retain(|known| known.folder != project.folder);
        projects.push(project);
        projects.sort_by(|a, b| a.name.cmp(&b.name).then(a.folder.cmp(&b.folder)));
        self.sync_projects_picker(cx);
        self.refresh_palette_sections(cx);
        cx.notify();
    }

    /// Setup for an unbound project, or explicit reconnect of its saved local owner.
    pub(super) fn open_local_project_binding(
        &mut self,
        project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !reader_features::enabled(reader_features::Feature::Agents, cx)
            || !self.local_binding_target_current(&project)
        {
            return;
        }
        let access = reader_features::access(reader_features::Feature::Agents, cx);
        let state = self.session_directory.clone();
        let projects = self.project_navigation.discovery.projects.clone();
        let vault = self.vault_root.clone();
        let request = self.project_request;
        cx.spawn_in(window, async move |this, cx| {
            let area = project.folder.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let binding = Binding::load(&area)?;
                    let environments = if let Some(environment) = binding.pinned_environment() {
                        vec![environment.clone()]
                    } else {
                        let origins = match state {
                            Some(state) => project::browser_connections(&state, &projects)?,
                            None => Default::default(),
                        };
                        origins
                            .into_iter()
                            .filter_map(|(alias, origin)| {
                                okilum_t3::projects::local_manager::environment(&origin).ok()?;
                                okilum_t3::Environment::new(alias, &origin).ok()
                            })
                            .collect::<Vec<_>>()
                    };
                    Ok::<_, anyhow::Error>((binding, environments))
                })
                .await;
            let (binding, environments) = match loaded {
                Ok(loaded) => loaded,
                Err(error) => {
                    let _ = this.update_in_own_window(cx, |_, window, cx| {
                        reader_toast::error(format!("Owner unresolved: {error:#}"), window, cx);
                    });
                    return;
                }
            };
            let mut choices = Vec::new();
            let mut errors = Vec::new();
            for environment in environments {
                let credential = match this.update_in_own_window(cx, |this, _, cx| {
                    (this.vault_root == vault
                        && this.project_request == request
                        && this.local_binding_target_current(&project)
                        && access.allowed())
                    .then(|| cx.read_credentials(&environment.credential_url()))
                }) {
                    Ok(Some(task)) => task,
                    _ => return,
                };
                let token = match credential.await {
                    Ok(Some((_, bytes))) => okilum_t3::StoredToken::from_secret(&bytes).ok(),
                    _ => None,
                };
                let request_access = access.clone();
                let expected_binding = binding.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        let _access = request_access.enter();
                        let gateway = crate::reader_project_navigation::manager_gateway(
                            environment.clone(),
                            Arc::new(
                                okilum_t3::McpGateway::new(environment.clone(), token)?
                                    .with_access(request_access.clone()),
                            ),
                            request_access,
                        )?;
                        verified_local_owner_choices(&expected_binding, environment, gateway)
                    })
                    .await;
                match result {
                    Ok((found, unresolved)) => {
                        choices.extend(found);
                        errors.extend(unresolved);
                    }
                    Err(error) => errors.push(format!("{error:#}")),
                }
            }
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                if this.vault_root != vault
                    || this.project_request != request
                    || !this.local_binding_target_current(&project)
                    || !reader_features::enabled(reader_features::Feature::Agents, cx)
                {
                    return;
                }
                this.show_local_owner_picker(project, binding, choices, errors, window, cx);
            });
        })
        .detach();
    }

    fn show_local_owner_picker(
        &mut self,
        project: Project,
        binding: Binding,
        choices: Vec<LocalOwnerChoice>,
        errors: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::WindowExt as _;
        let reader = cx.entity().downgrade();
        let busy = std::rc::Rc::new(std::cell::Cell::new(false));
        let diagnostic = cx.new(|_| None::<String>);
        window.open_dialog(cx, move |dialog, _, cx| {
            let mut content = v_flex().gap_2().child(
                "Select the existing T3 project that owns your local source checkout. No T3 project or Manager will be created."
            );
            content = content.debug_selector(|| "local-owner-picker".into());
            if let Some(owner) = binding.pinned_owner() {
                content = content.child(format!("Reconnect saved owner: {} · {} · {}. Manager, executor and drafts stay retained.",
                    owner.environment_id, owner.project_id, owner.checkout.display()));
            }
            if let Some(error) = diagnostic.read(cx) {
                content = content.child(div().debug_selector(|| "local-owner-error".into()).child(error.clone()));
            }
            if choices.is_empty() {
                content = content.child("Owner unresolved: no verified local checkout is available. Sign in to local T3 in Settings, then reopen setup. Your local project and drafts are retained.");
            }
            for error in &errors { content = content.child(format!("Owner unresolved: {error}")); }
            let mut rows = v_flex().id("local-owner-choices").gap_2().max_h(px(360.)).overflow_y_scroll();
            for (index, choice) in choices.iter().enumerate() {
                let label = format!("{} · {} · {} · {} · {}", choice.title,
                    choice.environment.origin(), choice.selection.environment_id,
                    choice.selection.project_id, choice.selection.checkout.display());
                let (reader, binding, project, choice, busy) =
                    (reader.clone(), binding.clone(), project.clone(), choice.clone(), busy.clone());
                let diagnostic = diagnostic.clone();
                rows = rows.child(Button::new(("local-owner", index)).debug_selector(move || format!("local-owner-{index}"))
                    .label(label).disabled(busy.get()).on_click(move |_, window, cx| {
                    if busy.replace(true) { return; }
                    let delivered = reader.update(cx, |this, cx| {
                        this.save_local_project_binding(project.clone(), binding.clone(), choice.clone(), busy.clone(), diagnostic.clone(), window, cx);
                    });
                    if delivered.is_err() { busy.set(false); }
                }));
            }
            dialog.title(if binding.pinned_owner().is_some() { "Reconnect local T3 project" } else { "Connect an existing local T3 project" })
                .width(px(640.)).on_ok(|_, _, _| false).child(content.child(rows))
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn save_local_project_binding(
        &mut self,
        project: Project,
        binding: Binding,
        choice: LocalOwnerChoice,
        busy: std::rc::Rc<std::cell::Cell<bool>>,
        diagnostic: Entity<Option<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !reader_features::enabled(reader_features::Feature::Agents, cx)
            || !self.local_binding_target_current(&project)
        {
            busy.set(false);
            return;
        }
        let vault = self.vault_root.clone();
        let request = self.project_request;
        let alias = choice.environment.alias.clone();
        let gateway = choice.gateway.clone();
        let access = reader_features::access(reader_features::Feature::Agents, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                let _access = access.enter();
                binding.save(&choice.environment, &choice.selection, &*choice.gateway)
            }).await;
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                use gpui_component::WindowExt as _;
                busy.set(false);
                if this.vault_root != vault || this.project_request != request
                    || !this.local_binding_target_current(&project)
                    || !reader_features::enabled(reader_features::Feature::Agents, cx) { return; }
                match result {
                    Ok(linked) => {
                        // Only a freshly revalidated owner may replace a cached
                        // authenticated transport used by project navigation.
                        this.project_navigation.t3_gateways.insert(alias, gateway);
                        window.close_dialog(cx);
                        this.show_new_project(linked, cx);
                        reader_toast::transient(format!("{} is connected to the selected local T3 project", project.name), window, cx);
                    }
                    Err(error) => diagnostic.update(cx, |message, cx| {
                        *message = Some(format!("Owner unresolved: {error:#}. Project and drafts retained; reopen setup after refreshing the source."));
                        cx.notify();
                    }),
                }
            });
        }).detach();
    }
    /// «Create the project in T3» with the paired connection; the result
    /// is a toast either way.
    fn create_t3_project(
        &mut self,
        project: Project,
        draft: Draft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.session_directory.clone();
        let known = self.project_navigation.t3_gateways.clone();
        let vault = self.vault_root.clone();
        let projects = self.project_navigation.discovery.projects.clone();
        cx.spawn_in(window, async move |this, cx| {
            let gateways: Gateways = if known.is_empty() {
                t3_gateways(&this, state, cx).await.unwrap_or_default()
            } else {
                known
            };
            let chosen = create::pick_environment(gateways.keys().map(String::as_str), &projects)
                .and_then(|alias| {
                    gateways
                        .get(&alias)
                        .cloned()
                        .map(|gateway| (alias, gateway))
                });
            let folder = project.folder.clone();
            let outcome: T3Outcome = match chosen {
                None => Err(anyhow::anyhow!(
                    "this device has no T3 connection: open the Manager panel and sign in to T3"
                )),
                Some((alias, gateway)) => {
                    let (vault, folder, draft) = (vault.clone(), folder.clone(), draft.clone());
                    cx.background_executor()
                        .spawn(async move {
                            let created = create::create_in_t3(&*gateway, &draft, &vault, &folder)?;
                            let link = T3Link {
                                environment: alias.clone(),
                                project_id: created.project.id.clone(),
                            };
                            let linked = create::link_t3(&folder, &link);
                            Ok::<_, anyhow::Error>((alias, created, linked))
                        })
                        .await
                }
            };
            let delivered = this.update_in_own_window(cx, |this, window, cx| {
                if this.vault_root == vault {
                    this.finish_t3_project(project, outcome, window, cx);
                }
            });
            if let Err(error) = delivered {
                eprintln!("New project: the T3 result did not reach the window: {error:#}");
            }
        })
        .detach();
    }

    fn finish_t3_project(
        &mut self,
        project: Project,
        outcome: T3Outcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (alias, created, linked) = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                eprintln!(
                    "New project: T3 did not create {:?}: {error:#}",
                    project.name
                );
                reader_toast::error(
                    format!(
                        "{} is a project, but T3 did not create it: {error:#}",
                        project.name
                    ),
                    window,
                    cx,
                );
                return;
            }
        };
        eprintln!(
            "New project: t3={alias} project={} root={} reused={}",
            created.project.id, created.project.workspace_root, created.reused
        );
        let folder = project.folder.clone();
        match linked {
            Ok(read_back) => self.show_new_project(read_back, cx),
            Err(error) => {
                reader_toast::error(
                    format!(
                        "T3 has the project {}, but the project manifest was not updated: {error:#}",
                        created.project.id
                    ),
                    window,
                    cx,
                );
                return;
            }
        }
        // The icon is T3's from now on, as for every project T3 has.
        let link = Counterpart {
            environment: alias,
            project_id: created.project.id.clone(),
            icon: created.project.project_icon.clone(),
            logo: None,
        };
        self.merge_icon_links(
            vec![folder.clone()],
            std::collections::BTreeMap::from([(folder, link)]),
            cx,
        );
        match &created.icon_error {
            Some(error) => reader_toast::error(
                format!(
                    "T3 has {}, but did not keep its icon: {error}",
                    project.name
                ),
                window,
                cx,
            ),
            None if created.reused => reader_toast::transient(
                format!(
                    "{} uses the T3 project «{}»",
                    project.name, created.project.title
                ),
                window,
                cx,
            ),
            None => reader_toast::transient(format!("{} is in T3", project.name), window, cx),
        }
    }
}

#[cfg(test)]
mod tests;
