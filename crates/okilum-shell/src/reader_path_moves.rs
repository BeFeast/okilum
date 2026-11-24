//! Apply completed path reconciliation to the active Reader surfaces.
use super::*;
pub(super) use reader_sidebar::cutover::Cutover;
pub(super) use reader_sidebar::moves::{resolve, save_sidebar, Plan};

pub(super) fn known_roots(directory: Option<&Path>) -> Vec<PathBuf> {
    let Some(directory) = directory else {
        return Vec::new();
    };
    let mut roots =
        reader_history::ReadingHistory::known_vault_roots(directory).unwrap_or_default();
    roots.extend(
        reader_open_windows::load_session_windows(directory)
            .into_iter()
            .filter(|(_, single)| !single)
            .map(|((root, _), _)| root),
    );
    roots
}

pub(super) fn live_plan(
    vault: &Vault,
    directory: Option<&Path>,
    paths: Vec<String>,
    archive_cutover: bool,
) -> Plan {
    let mut plan = resolve(&vault.root, directory, paths, Some(vault));
    if archive_cutover {
        plan.handoff = Cutover::from_archive(&vault.root, &known_roots(directory)).map(Arc::new);
    }
    plan
}

impl Reader {
    pub(super) fn navigation_paths(&self) -> Vec<String> {
        self.surfaces()
            .flat_map(|pane| {
                std::iter::once(pane.current_rel.clone())
                    .chain(pane.file_preview.iter().map(|preview| preview.rel.clone()))
                    .chain(pane.navigation.history.iter().cloned())
            })
            .chain(self.sidebar.recent.iter().map(|(path, _)| path.clone()))
            .chain(self.sidebar.pinned.iter().cloned())
            .collect()
    }

    pub(super) fn apply_startup_moves(&mut self, plan: &Plan, cx: &mut Context<Self>) {
        self.load_sidebar_state();
        let imported = plan
            .import
            .as_deref()
            .is_some_and(|(cutover, archive_state)| {
                self.import_archive_state(cutover, archive_state, cx)
            });
        let changed = self.remap_shortcuts(plan, cx);
        if !imported
            && !changed
            && (!self.sidebar.recent.is_empty() || !self.sidebar.pinned.is_empty())
        {
            self.save_sidebar(cx);
        }
        if let Some((mut saved, true)) = reader_ui_state::layout(&self.vault_root, cx) {
            plan.layout(&mut saved);
            reader_ui_state::record(&self.vault_root, saved, false, cx);
        }
    }

    fn import_archive_state(
        &mut self,
        cutover: &Cutover,
        archive: &reader_sidebar::State,
        cx: &mut Context<Self>,
    ) -> bool {
        if !cutover.import_sidebar(archive, &mut self.sidebar) {
            return false;
        }
        if !reader_ui_state::layout(&self.vault_root, cx).is_some_and(|(_, known)| known) {
            if let Some((saved, true)) = reader_ui_state::layout(&cutover.archive, cx) {
                reader_ui_state::record(&self.vault_root, cutover.import_layout(saved), false, cx);
            }
        }
        self.save_sidebar(cx);
        cx.notify();
        true
    }

    fn remap_shortcuts(&mut self, plan: &Plan, cx: &mut Context<Self>) -> bool {
        let previous = (
            &self.sidebar.recent,
            &self.sidebar.pinned,
            &self.sidebar.moved_uncertain,
        );
        let mut next = self.sidebar.clone();
        plan.sidebar(&mut next);
        if previous != (&next.recent, &next.pinned, &next.moved_uncertain) {
            self.sidebar = next;
            self.save_sidebar(cx);
            cx.notify();
            return true;
        }
        false
    }

    pub(super) fn apply_live_moves(
        &mut self,
        plan: &Plan,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(cutover) = plan.handoff.clone() {
            self.follow_archive_cutover(cutover, cx);
        }
        self.remap_shortcuts(plan, cx);
        for path in &mut self.quick_open.recent {
            plan.remap(path);
        }
        let root = self.vault_root.clone();
        let active = self.pane.id();
        let mut followed = false;
        let targets = self.for_each_pane(cx, |reader, _| {
            for path in &mut reader.pane.navigation.history {
                plan.remap(path);
            }
            let from = reader.selected_file();
            let to = plan.target(from)?;
            // A deleted note the person already left (Save As / Don't Save)
            // goes where they asked: a Save As copy matches its hash but is
            // not a move to follow.
            (reader.pane.editing.is_none() && !reader.left_deleted_note())
                .then(|| (reader.pane.id(), from.to_owned(), to.to_owned()))
        });
        for (pane, from, to) in targets.into_iter().flatten() {
            let root = root.clone();
            followed |= pane == active;
            self.run_for_pane(pane, window, cx, move |reader, window, cx| {
                if reader.vault_root == root
                    && reader.selected_file() == from
                    && reader.pane.editing.is_none()
                    && !reader.left_deleted_note()
                {
                    if reader.pane.current_rel == from {
                        // A verified move of an open Reader note is a
                        // continuation of the same selection. The ordinary
                        // navigation guard would otherwise re-detect deletion.
                        reader.pane.deleted_note = None;
                        reader.continue_deleted_navigation(
                            Box::new(move |reader, window, cx| {
                                reader.open_note(&to, None, window, cx)
                            }),
                            window,
                            cx,
                        );
                    } else {
                        reader.open_note(&to, None, window, cx);
                    }
                }
            });
        }
        if plan.uncertain(self.selected_file()) {
            self.link_notice = Some(format!("{} (moved?)", self.selected_file()).into());
        }
        followed
    }

    fn follow_archive_cutover(&self, cutover: Arc<Cutover>, cx: &mut Context<Self>) {
        let from = self.selected_file().to_owned();
        let Some(target) = cutover
            .target(&from)
            .filter(|_| self.pane.editing.is_none())
        else {
            return;
        };
        let opts = Opts {
            vault: Some(cutover.grove.clone()),
            note: Some(target.to_owned()),
            session_directory: self.session_directory.clone(),
            restore_workspace: true,
            ..Default::default()
        };
        let source = cx.entity().downgrade();
        // Both registry lookups and opening a window read the existing
        // Readers. Run after release of the archive Reader's update lease.
        cx.defer(move |cx| {
            let Some(source) = source.upgrade() else {
                return;
            };
            let reader = source.read(cx);
            if reader.vault_root != cutover.archive
                || reader.selected_file() != from
                || reader.pane.editing.is_some()
            {
                return;
            }
            if let Some(grove) =
                reader_open::reader_of(&cutover.grove, cx).and_then(|weak| weak.upgrade())
            {
                let archive_state = reader.sidebar.clone();
                grove.update(cx, |reader, cx| {
                    reader.import_archive_state(&cutover, &archive_state, cx);
                });
            }
            reader_open::finish_open(Ok(opts), cx);
        });
    }

    pub(super) fn open_archive_shortcut(&self, path: &str, cx: &mut Context<Self>) -> bool {
        let Some(archive) = &self.sidebar.archive_shortcuts else {
            return false;
        };
        let Ok(relative) = Path::new(path).strip_prefix(&archive.root) else {
            return false;
        };
        let note = okilum_core::vault::note_path(relative);
        let root = archive.root.clone();
        let id = archive.vault_id.clone();
        let directory = self.session_directory.clone();
        let resolving = cx.background_executor().spawn(async move {
            anyhow::ensure!(
                okilum_core::vault_link_identity::read(&root)?.as_deref() == Some(&id),
                "The archive vault's identity changed"
            );
            reader_files::checked_path(&root, &note)?;
            Ok(Opts {
                vault: Some(root),
                note: Some(note),
                session_directory: directory,
                ..Default::default()
            })
        });
        cx.spawn(async move |_, cx| {
            let resolved = resolving.await;
            cx.update(|cx| reader_open::finish_open(resolved, cx));
        })
        .detach();
        true
    }

    pub(super) fn shortcut_label(&self, path: &str) -> String {
        let label = self.note_label(path);
        if self.sidebar.moved_uncertain.contains(path) {
            format!("(moved?) {label}")
        } else {
            label
        }
    }
}
