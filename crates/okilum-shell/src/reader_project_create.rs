//! «Make this a project…» on a folder and «+ New project…» under the
//! Projects list (#1475): the Reader's side of the New project dialog.
//!
//! Create writes `ops/project.yaml` (and `_index.md` when missing) off the
//! UI thread, puts the project into the Projects list at once and closes
//! the dialog. «Create the project in T3» then runs on the paired T3
//! connection, the one the Manager panel and icon writes use (#1627): T3's
//! `project.create`, the chosen icon, and the `t3:` binding in the
//! manifest. A T3 failure is a toast; the written manifest stays.
use super::*;
use crate::reader_project_navigation::{t3_gateways, Gateways};
use okilum_shell::new_project_dialog::{NewProjectDialog, NewProjectEvent, TITLE};
use okilum_t3::projects::{
    create::{self, Created, Draft, T3Created, T3Link},
    icon::Counterpart,
    Project,
};

/// The tree's folder menu item.
pub(super) const MAKE_PROJECT: &str = "Make this a project…";

/// What the T3 step did: the connection, T3's project, and the manifest
/// read back with its new `t3:` block (or why it could not be added).
type T3Outcome = anyhow::Result<(String, T3Created, anyhow::Result<Project>)>;

impl Reader {
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
        use gpui_component::WindowExt as _;
        let fixed = folder.is_some();
        let relative = folder.unwrap_or_else(|| self.default_new_project_folder());
        self.close_project_switcher(window, cx);
        self.file_menu = None;
        let vault = self.vault_root.clone();
        let dialog = cx.new(|cx| NewProjectDialog::new(&vault, &relative, fixed, window, cx));
        let reader = cx.entity().downgrade();
        let handle = dialog.downgrade();
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
                            this.create_project(handle, folder, draft, window, cx)
                        })
                    }
                };
                if delivered.is_err() {
                    eprintln!("New project: the window that asked is gone");
                }
            })
        });
        let content = dialog.clone();
        window.open_dialog(cx, move |d, _, _| {
            d.title(TITLE)
                .width(px(480.))
                .on_ok(|_, _, _| false)
                .child(content.clone())
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
                    this.finish_create_project(dialog, written, draft, window, cx);
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
        if draft.create_in_t3() {
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
