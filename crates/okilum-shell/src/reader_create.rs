//! Inline, create-only Reader actions (#466).
use super::*;
use gpui_component::{notification::Notification, WindowExt};
use std::io::Read as _;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;

pub(super) struct CreatedUndo {
    pub root: PathBuf,
    pub relative: String,
    pub(super) source: Option<String>,
    pub(super) identity: (u64, u64),
}

impl CreatedUndo {
    fn capture(root: &Path, relative: &str, source: Option<String>) -> anyhow::Result<Self> {
        let root = root.canonicalize()?;
        #[cfg(unix)]
        let metadata = std::fs::symlink_metadata(root.join(relative))?;
        #[cfg(unix)]
        let identity = (metadata.dev(), metadata.ino());
        #[cfg(windows)]
        let identity = okilum_core::windows_files::checked_identity(&root.join(relative))?;
        let item = Self {
            root,
            relative: relative.into(),
            source,
            identity,
        };
        item.verify()?;
        Ok(item)
    }

    pub(super) fn verify(&self) -> anyhow::Result<()> {
        let path = self.root.join(&self.relative);
        let metadata = std::fs::symlink_metadata(&path)?;
        #[cfg(unix)]
        let identity = (metadata.dev(), metadata.ino());
        #[cfg(windows)]
        let identity = okilum_core::windows_files::checked_identity(&path)?;
        anyhow::ensure!(
            identity == self.identity && !metadata.file_type().is_symlink(),
            "This item was replaced; it was kept"
        );
        if let Some(source) = &self.source {
            anyhow::ensure!(metadata.is_file(), "This item changed; it was kept");
            let mut bytes = Vec::new();
            std::fs::File::open(path)?
                .take(source.len() as u64 + 1)
                .read_to_end(&mut bytes)?;
            anyhow::ensure!(
                bytes == source.as_bytes(),
                "This note changed after creation; it was kept"
            );
        } else {
            anyhow::ensure!(metadata.is_dir(), "This item changed; it was kept");
            anyhow::ensure!(
                std::fs::read_dir(path)?.next().is_none(),
                "This folder is no longer empty; it was kept"
            );
        }
        Ok(())
    }
}
/// How long the «Created …» toast stays; its Undo keeps it from hiding.
const CREATION_TOAST_LIFETIME: std::time::Duration = std::time::Duration::from_secs(6);

struct CreationToast;

pub(super) struct Creation {
    root: PathBuf,
    pub folder: String,
    pub directory: bool,
    pub input: Entity<InputState>,
    pub error: Option<String>,
    pub templates: Option<okilum_core::note_templates::Catalog>,
    pub selected_template: Option<String>,
    _subscription: Subscription,
}

impl Reader {
    pub(super) fn invalidate_creation_undo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.creation_undo = None;
        window.remove_notification::<CreationToast>(cx);
    }

    fn announce_creation(
        &mut self,
        relative: &str,
        source: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = if source.is_some() {
            reader_move::display_name(relative)
        } else {
            Path::new(relative)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        };
        let Ok(item) = CreatedUndo::capture(&self.vault_root, relative, source) else {
            self.invalidate_creation_undo(window, cx);
            reader_toast::transient(format!("Created {title} — Undo unavailable"), window, cx);
            return;
        };
        let item = Arc::new(item);
        self.creation_undo = Some(item.clone());
        let reader = cx.weak_entity();
        let announced = item.clone();
        // The Undo button keeps the toast from hiding on its own; it goes
        // after a while, and Ctrl+Z still undoes (#1354).
        let expiring = Arc::downgrade(&item);
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(CREATION_TOAST_LIFETIME)
                .await;
            let _ = handle.update(cx, |_, window, cx| {
                let current = this
                    .upgrade()
                    .and_then(|this| this.read(cx).creation_undo.clone());
                if current.is_some_and(|c| expiring.upgrade().is_some_and(|e| Arc::ptr_eq(&c, &e)))
                {
                    window.remove_notification::<CreationToast>(cx);
                }
            });
        })
        .detach();
        window.push_notification(
            Notification::new()
                .id::<CreationToast>()
                .message(format!("Created {title}"))
                .placement(Anchor::BottomRight)
                .py_2()
                .autohide(false)
                .action(move |_, _, cx| {
                    let reader = reader.clone();
                    let announced = announced.clone();
                    reader_icon_button(
                        "undo-create",
                        IconName::Undo2,
                        if cfg!(target_os = "macos") {
                            "Undo (⌘Z)"
                        } else {
                            "Undo (Ctrl+Z)"
                        },
                        cx,
                    )
                    .debug_selector(|| "undo-create".into())
                    .on_click(move |_, window, cx| {
                        let _ = reader.update(cx, |this, cx| {
                            this.undo_creation(Some(&announced), window, cx)
                        });
                    })
                }),
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(8)).await;
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                if this
                    .creation_undo
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &item))
                {
                    window.remove_notification::<CreationToast>(cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn undo_creation(
        &mut self,
        announced: Option<&Arc<CreatedUndo>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.creation_undo.clone() else {
            return;
        };
        if announced.is_some_and(|old| !Arc::ptr_eq(old, &item)) {
            return;
        }
        if self.creation.is_some()
            || self.renaming.is_some()
            || self.trash_pending
            || self.note_move_pending
        {
            return;
        }
        let error = if self.vault_root != item.root {
            Some("Open the original vault to undo creation".to_owned())
        } else if self.source_is_dirty(cx) {
            Some("Finish editing before undoing creation; your note was kept".to_owned())
        } else {
            item.verify()
                .err()
                .map(|error| format!("Cannot undo creation: {error}"))
        };
        window.remove_notification::<CreationToast>(cx);
        if let Some(error) = error {
            reader_toast::transient(error, window, cx);
            return;
        }
        self.delete_created_path(item, window, cx);
    }
    pub(super) fn creation_templates(
        &self,
    ) -> anyhow::Result<okilum_core::note_templates::Catalog> {
        let folder = self
            .session_directory
            .as_deref()
            .map(|state| reader_templates::configured_folder(&self.vault_root, state))
            .transpose()?
            .flatten();
        okilum_core::note_templates::Catalog::load_with_folder(&self.vault_root, folder.as_deref())
    }

    #[cfg(any(unix, windows))]
    pub(super) fn create_missing_note(
        &mut self,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.missing_note_path(url) else {
            return;
        };
        if self.creation.is_some() || self.trash_pending || self.note_move_pending {
            self.link_notice = Some("Finish or cancel the current file operation first.".into());
            cx.notify();
            return;
        }
        self.clear_hover(cx);
        // Place the input under the nearest existing real parent. Any not-yet-
        // created subdirectories remain editable as a relative suffix.
        let folder = self.tree.creation_parent(Path::new(&path));
        self.new_note(Some(&folder), window, cx);
        if let Some(create) = &self.creation {
            // begin_create may redirect away from the templates directory. Keep
            // the authored destination intact so create-only validation rejects
            // protected targets instead of silently creating a different note.
            let name = Path::new(&path)
                .strip_prefix(&create.folder)
                .unwrap_or(Path::new(&path))
                .to_string_lossy()
                .into_owned();
            create.input.update(cx, |input, cx| {
                input.set_value(name, window, cx);
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
    }

    pub(super) fn new_note(
        &mut self,
        folder: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_create(folder, false, window, cx);
    }
    pub(super) fn new_folder(
        &mut self,
        folder: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_create(folder, true, window, cx);
    }
    fn begin_create(
        &mut self,
        folder: Option<&str>,
        directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.trash_pending || self.note_move_pending {
            return;
        }
        if self.loading.as_ref().is_some_and(|l| l.active) || self.session_directory.is_none() {
            self.link_notice =
                Some("Wait for the vault and draft recovery storage before creating files.".into());
            cx.notify();
            return;
        }
        self.renaming = None;
        let mut folder = folder
            .map(str::to_owned)
            .or_else(|| self.tree.selected_creation_folder())
            .unwrap_or_else(|| {
                Path::new(&self.current_rel)
                    .parent()
                    .unwrap_or(Path::new(""))
                    .to_string_lossy()
                    .into_owned()
            });
        let templates = self.creation_templates();
        // A template opened for inspection must not turn Cmd-N into a write
        // inside the templates collection.
        if templates
            .as_ref()
            .is_ok_and(|catalog| catalog.contains_target(Path::new(&folder)))
        {
            folder.clear();
        }
        folder = existing_creation_folder(&self.vault_root, &folder);
        let error = templates.as_ref().err().map(|e| format!("{e:#}"));
        let templates = templates.ok();
        let selected_template = templates.as_ref().and_then(|c| c.default_file());
        self.reveal_in_tree(&folder, window, cx);
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(if directory {
                "Folder name"
            } else {
                "Note name"
            })
        });
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.commit_creation(window, cx),
                InputEvent::Change => {
                    if let Some(create) = this.creation.as_mut() {
                        create.error = None;
                    }
                    cx.notify();
                }
                _ => {}
            });
        input.update(cx, |input, cx| input.focus(window, cx));
        let insertion = if folder.is_empty() {
            0
        } else {
            self.tree
                .rows
                .iter()
                .position(|r| r.path == folder)
                .map_or(0, |i| i + 1)
        };
        self.creation = Some(Creation {
            root: self.vault_root.clone(),
            folder,
            directory,
            input,
            error: if directory { None } else { error },
            templates,
            selected_template,
            _subscription: subscription,
        });
        if let Some(error) = self.creation.as_ref().and_then(|c| c.error.clone()) {
            reader_toast::error(error, window, cx);
        }
        self.scroll_tree_to(insertion);
        cx.notify();
    }
    pub(super) fn choose_creation_template(
        &mut self,
        selected: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(create) = self.creation.as_mut() {
            create.selected_template = selected;
            create.error = None;
            create.input.update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
        }
    }
    pub(super) fn cancel_creation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.creation = None;
        self.tree_focus.focus(window, cx);
        cx.notify();
    }
    fn commit_creation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(create) = self.creation.as_ref() else {
            return;
        };
        let root = create.root.clone();
        let directory = create.directory;
        let name = create.input.read(cx).value().to_string();
        // The destination may disappear while the inline row is open too.
        let folder = existing_creation_folder(&root, &create.folder);
        let templates = create.templates.clone();
        let selected_template = create.selected_template.clone();
        let result = (|| -> anyhow::Result<(String, Option<String>)> {
            anyhow::ensure!(
                root == self.vault_root,
                "The open vault changed. Cancel and try again"
            );
            anyhow::ensure!(
                !self.loading.as_ref().is_some_and(|l| l.active),
                "Wait for the vault to finish opening"
            );
            let relative =
                okilum_core::note_files::typed_path(Path::new(&folder), &name, directory)?;
            let rel = okilum_core::vault::note_path(&relative);
            let source = if directory {
                okilum_core::note_files::create_folders(&root, &relative, true)?;
                None
            } else {
                anyhow::ensure!(
                    self.save_source_for_navigation(cx),
                    "Resolve the current note's save before creating another"
                );
                let state = self
                    .session_directory
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("No draft recovery storage"))?;
                let now = time::OffsetDateTime::now_local()
                    .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
                let catalog = templates.as_ref().ok_or_else(|| anyhow::anyhow!("Templates could not be loaded. Cancel and retry after fixing templates.json"))?;
                Some(okilum_core::note_files::create_from_template(
                    &root,
                    &relative,
                    now,
                    &state.join("editor-drafts"),
                    catalog,
                    selected_template.as_deref(),
                )?)
            };
            Ok((rel, source))
        })();
        match result {
            Ok((rel, Some(source))) => {
                let created = rel.clone();
                self.creation = None;
                Arc::make_mut(&mut self.vault).register_created_note(&rel);
                // Publish the successful create before the watcher catches up.
                // Its ancestors must exist in the tree for an immediate Cmd-N
                // in this new folder to render a focused, actionable input row.
                self.tree
                    .entry_created(&rel, okilum_core::vault::EntryKind::Markdown);
                // New note opened the sidebar for its name row. In a narrow
                // window that sidebar covers the note, so it closes like
                // after any other selection (#1412). The editor takes focus
                // below.
                self.dismiss_sidebar_after_selection(window, cx);
                self.editing = None;
                self.navigation.preparation_generation =
                    self.navigation.preparation_generation.wrapping_add(1);
                let document =
                    okilum_core::render::reader_document_from_source(&self.vault, &rel, &source);
                self.accept_prepared_document(
                    prepared_links::DocumentRequest {
                        rel,
                        jump: None,
                        heading: None,
                        history_index: None,
                        restore_position: None,
                    },
                    Ok(prepared_links::PreparedDocument {
                        canonical_source: Some(document.canonical_source),
                        source: document.rendered,
                        original: Some(document.original_body),
                        identities: document.links,
                        frontmatter: document.frontmatter,
                    }),
                    window,
                    cx,
                );
                self.edit_created_note(window, cx);
                // Typing starts in the body after the heading, never before
                // the frontmatter (#1117).
                let body = okilum_core::note_files::body_start(&source);
                if let Some(editing) = &self.editing {
                    editing
                        .input()
                        .update(cx, |input, cx| input.set_selected_range(body..body, cx));
                }
                self.announce_creation(&created, Some(source), window, cx);
                if let Some(folder) = templates
                    .as_ref()
                    .filter(|catalog| catalog.missing())
                    .map(|catalog| catalog.folder.display().to_string())
                {
                    reader_toast::transient(
                        format!("Created without a template: the templates folder «{folder}» is not in this vault."),
                        window,
                        cx,
                    );
                }
                self.queue_vault_mutation(
                    okilum_core::Changes {
                        changed: std::collections::BTreeSet::from([created]),
                        ..Default::default()
                    },
                    cx,
                );
            }
            Ok((rel, None)) => {
                self.creation = None;
                // Watcher refreshes the inventory; reveal an empty new folder now.
                self.tree
                    .entry_created(&rel, okilum_core::vault::EntryKind::Directory);
                self.reveal_in_tree(&rel, window, cx);
                self.announce_creation(&rel, None, window, cx);
                self.queue_vault_mutation(
                    okilum_core::Changes {
                        directories: std::collections::BTreeSet::from([rel]),
                        ..Default::default()
                    },
                    cx,
                );
            }
            Err(error) => {
                reader_toast::error(format!("{error:#}"), window, cx);
                if let Some(create) = self.creation.as_mut() {
                    create.error = Some(format!("{error:#}"));
                }
            }
        }
        cx.notify();
    }
}

/// Fall back only for missing paths. Existing unsafe or inaccessible entries
/// stay subject to create-only validation and its visible error.
fn existing_creation_folder(root: &Path, folder: &str) -> String {
    let mut path = Path::new(folder);
    while !path.as_os_str().is_empty()
        && std::fs::symlink_metadata(root.join(path))
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        path = path.parent().unwrap_or(Path::new(""));
    }
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn deleted_folder_operation(cx: &mut TestAppContext, operation: &str, nested: bool) {
        use gpui_component::WindowExt;
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let state = temp.path().join("state");
        let folder = if nested { "Parent/mytpl" } else { "mytpl" };
        let note = format!("{folder}/Note.md");
        std::fs::create_dir_all(root.join(folder)).unwrap();
        std::fs::write(root.join(&note), "# Note\n").unwrap();
        std::fs::write(root.join("start.md"), "# Start\n").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some(note.clone()),
                        index_dir: Some(temp.path().join("cache")),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_eq!(r.selected_file(), note);
            r.toggle_source(window, cx);
            r.editing
                .as_ref()
                .unwrap()
                .set_value("# Unsaved שלום\r\n", window, cx);
            r.tree.cursor = Some(folder.into());
            assert!(r.tree.rows.iter().any(|row| row.path == folder));
        });
        visual.run_until_parked();
        std::fs::remove_dir_all(root.join(folder)).unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.refresh_source_from_disk(window, cx);
            assert!(r
                .link_notice
                .as_deref()
                .unwrap()
                .contains("moved or deleted"));
            assert_eq!(
                r.editing
                    .as_ref()
                    .unwrap()
                    .input()
                    .read(cx)
                    .value()
                    .as_ref(),
                "# Unsaved שלום\r\n"
            );
            r.apply_vault_changes(
                okilum_core::Changes {
                    directories: std::collections::BTreeSet::from([folder.into()]),
                    ..Default::default()
                },
                window,
                cx,
            );
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(!r.tree.rows.iter().any(|row| row.path == folder));
        });
        if operation == "recovery_failure" {
            let journal = std::fs::read_dir(state.join("editor-drafts"))
                .unwrap()
                .flatten()
                .map(|entry| entry.path())
                .find(|path| path.extension().is_some_and(|ext| ext == "json"))
                .unwrap();
            std::fs::remove_file(&journal).unwrap();
            std::fs::create_dir(&journal).unwrap();
            reader.update_in(visual, |r, window, cx| {
                r.open_note("start.md", None, window, cx);
                assert_eq!(r.selected_file(), note);
                assert_eq!(
                    r.editing
                        .as_ref()
                        .unwrap()
                        .input()
                        .read(cx)
                        .value()
                        .as_ref(),
                    "# Unsaved שלום\r\n"
                );
                assert!(r
                    .link_notice
                    .as_deref()
                    .unwrap()
                    .contains("recovery copy couldn’t be saved"));
                r.new_note(None, window, cx);
                r.creation
                    .as_ref()
                    .unwrap()
                    .input
                    .update(cx, |input, cx| input.set_value("Blocked", window, cx));
                r.commit_creation(window, cx);
                assert!(r.creation.as_ref().unwrap().error.is_some());
                assert!(r.editing.is_some());
            });
            visual.run_until_parked();
            assert!(!root.join("Blocked.md").exists());
            visual.update(|window, cx| assert!(!window.notifications(cx).is_empty()));
            return;
        }
        if operation == "quick_open" {
            reader.update_in(visual, |r, window, cx| r.focus_handle.focus(window, cx));
            visual.simulate_keystrokes("ctrl-k");
            visual.simulate_input("start");
            visual.run_until_parked();
            reader.read_with(visual, |r, _| {
                assert!(r.quick_open.open);
                assert!(r.quick_open.rows.iter().any(|row| row.path == "start.md"));
            });
            visual.simulate_keystrokes("enter");
        } else if operation == "sidebar" {
            reader.update_in(visual, |r, window, cx| {
                r.reveal_in_tree("start.md", window, cx);
            });
            visual.run_until_parked();
            let row = visual.debug_bounds("tree-row-start.md").unwrap();
            visual.simulate_click(row.center(), gpui::Modifiers::default());
        } else {
            if operation == "keyboard" {
                reader.update_in(visual, |r, window, cx| r.focus_handle.focus(window, cx));
                visual.simulate_keystrokes("secondary-n");
            } else {
                reader.update_in(visual, |r, _, cx| {
                    r.panels.open(reader_layout::Panel::Notes);
                    cx.notify();
                });
                visual.run_until_parked();
                let button = visual.debug_bounds("sidebar-new-note").unwrap();
                visual.simulate_click(button.center(), gpui::Modifiers::default());
            }
            visual.run_until_parked();
            reader.read_with(visual, |r, _| {
                assert_eq!(
                    r.creation.as_ref().unwrap().folder,
                    if nested { "Parent" } else { "" }
                );
            });
            visual.simulate_input("Created");
            visual.simulate_keystrokes("enter");
        }
        visual.run_until_parked();
        let destination = if operation == "quick_open" || operation == "sidebar" {
            "start.md"
        } else if nested {
            "Parent/Created.md"
        } else {
            "Created.md"
        };
        reader.update_in(visual, |r, window, cx| {
            assert_eq!(
                r.selected_file(),
                destination,
                "navigation must not depend on saving a deleted path"
            );
            assert!(
                !window.notifications(cx).is_empty(),
                "draft recovery must be announced"
            );
        });
        assert!(root.join(destination).is_file());
        assert!(
            !root.join(folder).exists(),
            "never recreate the deleted folder"
        );
        let drafts = state.join("editor-drafts");
        let listing = okilum_core::source_history::drafts(&drafts, &root).unwrap();
        let draft = listing
            .versions
            .iter()
            .find(|v| v.note.ends_with(&note))
            .expect("deleted source remains recoverable");
        assert_eq!(draft.text, "# Unsaved שלום\r\n");
        okilum_core::source_history::save_copy(
            &root,
            Path::new("Recovered.md"),
            &drafts,
            &draft.text,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("Recovered.md")).unwrap(),
            draft.text
        );
    }

    #[gpui::test]
    fn deleted_folder_keyboard_creation_preserves_draft(cx: &mut TestAppContext) {
        deleted_folder_operation(cx, "keyboard", false);
    }

    #[gpui::test]
    fn deleted_folder_header_creation_uses_surviving_ancestor(cx: &mut TestAppContext) {
        deleted_folder_operation(cx, "header", true);
    }

    #[gpui::test]
    fn deleted_folder_quick_open_preserves_draft(cx: &mut TestAppContext) {
        deleted_folder_operation(cx, "quick_open", false);
    }

    #[gpui::test]
    fn deleted_folder_sidebar_navigation_preserves_draft(cx: &mut TestAppContext) {
        deleted_folder_operation(cx, "sidebar", false);
    }

    #[gpui::test]
    fn deleted_folder_failed_recovery_blocks_navigation_and_creation(cx: &mut TestAppContext) {
        deleted_folder_operation(cx, "recovery_failure", false);
    }

    #[test]
    fn creation_undo_guard_preserves_changed_replaced_and_nonempty_items() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let path = root.join("Created.md");
        std::fs::write(&path, "# Created\n").unwrap();
        let note = CreatedUndo::capture(root, "Created.md", Some("# Created\n".into())).unwrap();
        assert!(note.verify().is_ok());
        std::fs::write(&path, "user content").unwrap();
        assert!(note.verify().is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "user content");
        std::fs::rename(&path, root.join("Original.md")).unwrap();
        std::fs::write(&path, "# Created\n").unwrap();
        assert!(
            note.verify().is_err(),
            "same bytes do not authorize a replacement inode"
        );
        std::fs::create_dir(root.join("Folder")).unwrap();
        let folder = CreatedUndo::capture(root, "Folder", None).unwrap();
        assert!(folder.verify().is_ok());
        std::fs::write(root.join("Folder/Keep.md"), "keep").unwrap();
        assert!(folder.verify().is_err());
        assert!(root.join("Folder/Keep.md").exists());
    }

    #[cfg(target_os = "linux")]
    #[gpui::test]
    fn creation_undo_keyboard_and_toast_keep_generated_content_recoverable(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            // Exercise hit testing at settled toast geometry, not animation timing.
            cx.set_reduce_motion(true);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Start.md"), "# Start").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Start.md".into()),
                        index_dir: Some(temp.path().join("cache")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.simulate_resize(size(px(1200.), px(860.)));
        visual.run_until_parked();
        for directory in [false, true] {
            let relative = if directory {
                "Parent/Empty"
            } else {
                "Created.md"
            };
            reader.update_in(visual, |r, window, cx| {
                r.begin_create(Some(""), directory, window, cx);
                r.creation
                    .as_ref()
                    .unwrap()
                    .input
                    .clone()
                    .update(cx, |input, cx| {
                        input.set_value(
                            if directory { "Parent/Empty" } else { "Created" },
                            window,
                            cx,
                        );
                    });
                r.commit_creation(window, cx);
            });
            visual.run_until_parked();
            assert!(
                root.join(relative).exists(),
                "positive control: creation succeeded"
            );
            let bytes = (!directory).then(|| std::fs::read(root.join(relative)).unwrap());
            if !directory {
                visual.simulate_input("temporary text");
                visual.run_until_parked();
                reader.read_with(visual, |r, cx| assert!(r.source_is_dirty(cx)));
                visual.simulate_keystrokes("ctrl-z");
                visual.run_until_parked();
                reader.read_with(visual, |r, cx| {
                    assert_eq!(
                        r.editing
                            .as_ref()
                            .unwrap()
                            .test_input()
                            .read(cx)
                            .value()
                            .as_ref(),
                        std::str::from_utf8(bytes.as_ref().unwrap()).unwrap()
                    );
                    assert!(
                        r.creation_undo.is_some(),
                        "Source Undo must not undo creation"
                    );
                    assert!(!r.trash_pending);
                });
                assert!(root.join(relative).exists());
            }
            // Toast stack geometry animates after the previous Undo feedback.
            visual.executor().advance_clock(Duration::from_secs(1));
            visual.run_until_parked();
            let undo = visual
                .debug_bounds("undo-create")
                .expect("creation toast offers Undo");
            if directory {
                visual.simulate_click(undo.center(), Modifiers::default());
            } else {
                reader.update_in(visual, |r, window, cx| {
                    let input = r.editing.as_ref().unwrap().test_input();
                    input.update(cx, |input, cx| input.set_value("Unsaved work", window, cx));
                    assert!(r.source_is_dirty(cx));
                    r.undo_creation(None, window, cx);
                    assert!(
                        root.join(relative).exists(),
                        "dirty Source must survive Undo"
                    );
                    assert!(r.creation_undo.is_some());
                    let original = std::str::from_utf8(bytes.as_ref().unwrap()).unwrap();
                    input.update(cx, |input, cx| input.set_value(original, window, cx));
                    r.tree_focus.focus(window, cx);
                });
                visual.run_until_parked();
                visual.simulate_keystrokes("secondary-z");
            }
            visual.run_until_parked();
            reader.read_with(visual, |r, cx| {
                assert!(r.creation_undo.is_none(),
                    "Undo dispatched for {relative}: pending={}, dirty={}, loading={}, bounds={undo:?}",
                    r.trash_pending, r.source_is_dirty(cx),
                    r.loading.as_ref().is_some_and(|l| l.active));
            });
            assert!(
                !root.join(relative).exists(),
                "creation Undo removes the target: {relative}"
            );
            if directory {
                assert!(root.join("Parent").is_dir(), "ancestors are not removed");
            }
            reader.update_in(visual, |r, window, cx| r.undo_last_trash(window, cx));
            visual.run_until_parked();
            assert!(
                root.join(relative).exists(),
                "system Trash remains recoverable"
            );
            if let Some(bytes) = bytes {
                assert_eq!(std::fs::read(root.join(relative)).unwrap(), bytes);
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[gpui::test]
    fn creation_undo_is_scoped_to_its_toast_vault_and_tree(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(true);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let other = temp.path().join("other");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(other.join("Second")).unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        index_dir: Some(temp.path().join("cache")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.simulate_resize(size(px(1200.), px(860.)));
        visual.run_until_parked();
        let old = reader.update_in(visual, |r, window, cx| {
            r.new_folder(Some(""), window, cx);
            r.creation
                .as_ref()
                .unwrap()
                .input
                .clone()
                .update(cx, |input, cx| {
                    input.set_value("First", window, cx);
                });
            r.commit_creation(window, cx);
            r.creation_undo.clone().unwrap()
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_secs(7));
        visual.run_until_parked();
        let latest = reader.update_in(visual, |r, window, cx| {
            r.new_folder(Some(""), window, cx);
            r.creation
                .as_ref()
                .unwrap()
                .input
                .clone()
                .update(cx, |input, cx| {
                    input.set_value("Second", window, cx);
                });
            r.commit_creation(window, cx);
            r.creation_undo.clone().unwrap()
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("undo-create").is_some(),
            "old timer cannot dismiss a new toast"
        );
        reader.update_in(visual, |r, window, cx| {
            r.undo_creation(Some(&old), window, cx);
            assert!(Arc::ptr_eq(r.creation_undo.as_ref().unwrap(), &latest));
            assert!(
                !r.trash_pending,
                "stale callback cannot act on latest creation"
            );
        });
        assert!(root.join("First").exists());
        assert!(root.join("Second").exists());
        visual.executor().advance_clock(Duration::from_secs(7));
        visual.run_until_parked();
        // The lifetime initiates dismissal; allow the notification exit phase
        // to unmount its action before checking rendered bounds.
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("undo-create").is_none(),
            "toast disappears after its eight-second lifetime and exit phase"
        );
        reader.update_in(visual, |r, window, cx| {
            assert!(
                r.creation_undo.is_some(),
                "keyboard Undo survives toast expiry"
            );
            r.new_note(Some(""), window, cx);
        });
        visual.run_until_parked();
        visual.simulate_input("Scratch");
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert_eq!(
                r.creation.as_ref().unwrap().input.read(cx).value().as_ref(),
                "Scratch"
            );
        });
        visual.simulate_keystrokes("ctrl-z");
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert!(
                r.creation
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .value()
                    .is_empty(),
                "inline field keeps text Undo"
            );
            assert!(r.creation_undo.is_some());
            assert!(!r.trash_pending);
            r.cancel_creation(window, cx);
            r.vault_root = other.clone();
            r.undo_creation(Some(&latest), window, cx);
            assert!(
                !r.trash_pending,
                "foreign vault callback must not begin deletion"
            );
            r.vault_root = root.clone();
            r.tree_focus.focus(window, cx);
        });
        assert!(other.join("Second").exists());
        assert!(root.join("Second").exists());
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert!(
                r.tree_focus.contains_focused(window, cx),
                "tree must own keyboard focus"
            );
            assert!(r.creation_undo.is_some());
            assert!(!r.source_is_dirty(cx));
            assert!(!r.loading.as_ref().is_some_and(|l| l.active));
        });
        visual.simulate_keystrokes("secondary-z");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(
                r.creation_undo.is_none(),
                "tree keyboard Undo must reach the handler"
            );
        });
        assert!(
            !root.join("Second").exists(),
            "expired toast still supports tree Undo"
        );
        assert!(root.join("First").exists());
        assert!(other.join("Second").exists());
        reader.update_in(visual, |r, window, cx| r.undo_last_trash(window, cx));
        visual.run_until_parked();
        assert!(
            root.join("Second").exists(),
            "clean up recoverable Trash fixture"
        );
    }

    #[gpui::test]
    fn creation_prefers_tree_selection_then_open_note(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("Open")).unwrap();
        std::fs::create_dir_all(root.join("Selected/Nested")).unwrap();
        std::fs::write(root.join("Open/Current.md"), "# Current").unwrap();
        std::fs::write(root.join("Selected/Other.md"), "# Other").unwrap();
        std::fs::write(root.join("Root.md"), "# Root").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Open/Current.md".into()),
                        index_dir: Some(temp.path().join("cache")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        for directory in [false, true] {
            for (selected, explicit, expected) in [
                (Some("Selected/Nested"), None, "Selected/Nested"),
                (Some("Selected/Other.md"), None, "Selected"),
                (Some("Root.md"), None, ""),
                (None, None, "Open"),
                (Some("No longer exists.md"), None, "Open"),
                (Some("Selected/Nested"), Some(""), ""),
            ] {
                reader.update_in(visual, |r, window, cx| {
                    r.tree.cursor = selected.map(str::to_owned);
                    if directory {
                        r.new_folder(explicit, window, cx);
                    } else {
                        r.new_note(explicit, window, cx);
                    }
                    assert_eq!(
                        r.creation.as_ref().unwrap().folder,
                        expected,
                        "directory={directory}, selected={selected:?}, explicit={explicit:?}"
                    );
                    r.cancel_creation(window, cx);
                });
            }
        }
    }

    #[gpui::test]
    fn empty_folder_persists_inventory_without_forking_or_copying_search(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Start.md"), "# Start").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Start.md".into()),
                        index_dir: Some(cache.clone()),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        let search = reader.read_with(visual, |v, _| v.searcher.clone().unwrap());
        let before = okilum_core::vault::warm::Snapshot::load_checked(&cache, &root).unwrap();
        assert!(before.search_generation.is_some());
        reader.update_in(visual, |v, window, cx| {
            v.new_folder(Some(""), window, cx);
            let input = v.creation.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("Empty", window, cx));
            v.commit_creation(window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert!(Arc::ptr_eq(v.searcher.as_ref().unwrap(), &search));
            assert!(v.vault.entries.iter().any(|entry| entry.path == "Empty"));
        });
        let after = okilum_core::vault::warm::Snapshot::load_checked(&cache, &root).unwrap();
        assert_eq!(before.search_generation, after.search_generation);
        assert!(after
            .vault()
            .entries
            .iter()
            .any(|entry| entry.path == "Empty"
                && entry.kind == okilum_core::vault::EntryKind::Directory));
    }

    /// #1117: typing right after creating a note writes into its body, after
    /// the heading. #1352: the new note has no front matter nobody wrote, opens
    /// in Live Preview, and leaving it keeps the Source choice made before.
    #[gpui::test]
    fn typing_right_after_create_lands_in_the_body(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let dir = std::env::temp_dir().join(format!("okilum-create-{}", uuid::Uuid::new_v4()));
        let root = dir.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("start.md"), "start\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(dir.join("index")),
                        session_directory: Some(dir.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        // As in the report: an existing note open in Source, then New note.
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx);
            reader.new_note(None, window, cx);
            let input = reader.creation.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| {
                input.set_value("Friend new note", window, cx)
            });
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(!r.ui_state.live_preview, "the existing note was in Source");
            assert!(
                r.source_live_preview(),
                "the new note opens in Live Preview"
            );
        });
        // Type at once, without clicking into the editor.
        visual.simulate_input("Body");
        visual.run_until_parked();
        reader.update_in(visual, |r, _, cx| {
            assert_eq!(r.current_rel, "Friend new note.md");
            let value = r.editing.as_ref().unwrap().test_input().read(cx).value();
            // Positive control: the keystrokes reached the new note's editor.
            assert_eq!(value.as_ref(), "# Friend new note\nBody");
            assert!(r.leave_source(cx));
            assert!(
                !r.ui_state.live_preview,
                "Source stays the choice for other notes"
            );
        });
        let saved = std::fs::read_to_string(root.join("Friend new note.md")).unwrap();
        assert_eq!(saved, "# Friend new note\nBody");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #1412: in a narrow window (half a screen, next to Settings) the
    /// sidebar opened for the name row lies over the document. After Enter
    /// it closes, so the new note is not drawn under it; a wide window keeps
    /// its docked sidebar.
    #[gpui::test]
    fn narrow_window_shows_the_new_note_after_create(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let long: String = (1..=400).map(|n| format!("Line {n}\n\n")).collect();
        std::fs::write(root.join("Long.md"), format!("# Long\n\n{long}")).unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Long.md".into()),
                        index_dir: Some(temp.path().join("cache")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        for (width, name, docked) in [(683., "Narrow", false), (1366., "Wide", true)] {
            visual.simulate_resize(size(px(width), px(742.)));
            visual.run_until_parked();
            reader.update_in(visual, |r, window, cx| {
                r.new_note(None, window, cx);
                let input = r.creation.as_ref().unwrap().input.clone();
                input.update(cx, |input, cx| input.set_value(name, window, cx));
            });
            visual.run_until_parked();
            let notes_visible = |r: &Reader| {
                let available = f32::from(r.body_bounds.size.width);
                assert_eq!(reader_layout::overlay(available), !docked, "{name}");
                r.panels.visible(reader_layout::Panel::Notes, available)
            };
            // Positive control: the name row shows the sidebar at both widths.
            reader.read_with(visual, |r, _| assert!(notes_visible(r), "{name}"));
            visual.simulate_keystrokes("enter");
            visual.run_until_parked();
            reader.read_with(visual, |r, _| {
                assert_eq!(r.current_rel, format!("{name}.md"));
                assert!(r.source_live_preview(), "{name} opens in Live Preview");
                assert_eq!(notes_visible(r), docked, "{name}: sidebar after create");
            });
            // The editor keeps focus: typing lands in the new note.
            visual.simulate_input("Body");
            visual.run_until_parked();
            reader.update_in(visual, |r, _, cx| {
                let value = r.editing.as_ref().unwrap().test_input().read(cx).value();
                assert_eq!(value.as_ref(), format!("# {name}\nBody"));
                assert!(r.leave_source(cx));
            });
        }
    }

    /// #1394: a templates folder chosen in Settings applies to new notes,
    /// like one from `.obsidian/templates.json`; a chosen folder that is not
    /// in the vault still lets the note be created, without a template, and
    /// says so.
    #[gpui::test]
    fn new_note_uses_the_templates_folder_chosen_in_settings(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let dir = std::env::temp_dir().join(format!("okilum-create-{}", uuid::Uuid::new_v4()));
        let root = dir.join("notes");
        std::fs::create_dir_all(root.join("templates")).unwrap();
        std::fs::create_dir_all(root.join("obsidian-templates")).unwrap();
        let root = root.canonicalize().unwrap();
        let state = dir.join("state");
        std::fs::write(root.join("start.md"), "start").unwrap();
        std::fs::write(
            root.join("templates/Note.md"),
            "# {{title}}\n\nFrom Settings\n",
        )
        .unwrap();
        std::fs::write(
            root.join("obsidian-templates/Note.md"),
            "# {{title}}\n\nFrom Obsidian\n",
        )
        .unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(dir.join("index")),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let mut create = |name: &str| {
            reader.update_in(visual, |reader, window, cx| {
                reader.new_note(None, window, cx);
                let input = reader.creation.as_ref().unwrap().input.clone();
                input.update(cx, |input, cx| input.set_value(name, window, cx));
            });
            visual.run_until_parked();
            visual.simulate_keystrokes("enter");
            visual.run_until_parked();
            reader.update_in(visual, |reader, window, cx| {
                assert!(reader.leave_source(cx));
                reader.cancel_creation(window, cx);
            });
            std::fs::read_to_string(root.join(format!("{name}.md"))).ok()
        };
        // Positive control: without a choice the note is just its title.
        assert_eq!(create("Plain").as_deref(), Some("# Plain\n"));
        // The folder chosen in Settings.
        reader_templates::save(&root, &state, &root.join("templates")).unwrap();
        // Rejecting the vault itself keeps the chosen folder usable (#1411).
        assert_eq!(
            reader_templates::save(&root, &state, &root)
                .unwrap_err()
                .to_string(),
            "Choose a folder inside the vault, not the vault itself"
        );
        assert_eq!(
            create("Chosen").as_deref(),
            Some("# Chosen\n\nFrom Settings\n"),
            "created, with the Settings template"
        );
        // The folder from Obsidian's own settings, with no Settings choice.
        std::fs::remove_file(reader_templates::preferences_path(&root, &state).unwrap()).unwrap();
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        std::fs::write(
            root.join(".obsidian/templates.json"),
            r#"{"folder":"obsidian-templates"}"#,
        )
        .unwrap();
        assert_eq!(
            create("Obsidian").as_deref(),
            Some("# Obsidian\n\nFrom Obsidian\n")
        );
        // A chosen folder that is not in the vault: created, no template.
        reader_templates::save(&root, &state, &root.join("templates")).unwrap();
        std::fs::remove_dir_all(root.join("templates")).unwrap();
        assert_eq!(create("Missing").as_deref(), Some("# Missing\n"));
        reader.read_with(visual, |reader, _| {
            assert!(reader
                .creation_templates()
                .is_ok_and(|catalog| catalog.missing()));
        });
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[gpui::test]
    fn inline_create_templates_nested_paths_collision_and_cancel(cx: &mut TestAppContext) {
        cx.update(|cx| {
            // This test verifies file creation, not dialog animation. Keep the
            // hit target stable between selector lookup and dispatched input.
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let dir = std::env::temp_dir().join(format!("okilum-create-{}", uuid::Uuid::new_v4()));
        let root = dir.join("notes");
        std::fs::create_dir_all(root.join("Selected/Target")).unwrap();
        // macOS temp_dir uses /var, while Reader canonicalizes it to /private/var.
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("start.md"), "start").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(dir.join("index")),
                        session_directory: Some(dir.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let generation = reader.read_with(visual, |reader, _| {
            reader.loading.as_ref().unwrap().generation
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.new_note(Some("Selected/Target"), window, cx);
            assert!(reader.sidebar_items().windows(2).any(
                |rows| matches!(&rows[0], SideItem::Tree(row) if row.path == "Selected/Target")
                    && matches!(&rows[1], SideItem::Create(..))
            ))
        });
        assert!(!visual.did_prompt_for_new_path());
        reader.update_in(visual, |reader, window, cx| {
            let input = reader.creation.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("a/Новая 🧠", window, cx));
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "Selected/Target/a/Новая 🧠.md");
            assert_eq!(
                reader.loading.as_ref().unwrap().generation,
                generation,
                "create must not enter full prepare"
            );
            assert!(reader
                .vault
                .notes
                .iter()
                .any(|note| note.path == reader.current_rel));
            assert_eq!(
                reader
                    .searcher
                    .as_ref()
                    .unwrap()
                    .search("Новая", 10)
                    .unwrap()
                    .len(),
                1
            );
            assert!(reader
                .vault
                .entries
                .iter()
                .any(|entry| entry.path == "Selected/Target/a"
                    && entry.kind == okilum_core::vault::EntryKind::Directory));
            assert!(reader.editing.is_some());
            assert_eq!(
                reader.navigation.history.last().map(String::as_str),
                Some("Selected/Target/a/Новая 🧠.md")
            );
            assert!(reader
                .navigation
                .history
                .iter()
                .any(|path| path == "start.md"));
        });
        let created = std::fs::read_to_string(root.join("Selected/Target/a/Новая 🧠.md")).unwrap();
        assert_eq!(
            created, "# Новая 🧠\n",
            "no front matter by default (#1352)"
        );
        reader.update_in(visual, |reader, window, cx| {
            reader.new_note(None, window, cx);
            assert_eq!(
                reader.creation.as_ref().unwrap().folder,
                "Selected/Target/a"
            );
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("inline-create-row").is_some());
        reader.update_in(visual, |reader, window, cx| {
            assert!(reader
                .creation
                .as_ref()
                .unwrap()
                .input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window));
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.creation.is_none());
            assert!(reader.editing.is_some());
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.new_note(Some(""), window, cx);
            let input = reader.creation.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("../outside", window, cx));
            reader.commit_creation(window, cx);
            assert!(reader.creation.as_ref().unwrap().error.is_some());
            input.update(cx, |input, cx| input.set_value("start", window, cx));
            reader.commit_creation(window, cx);
            assert!(reader.creation.as_ref().unwrap().error.is_some());
            assert_eq!(reader.current_rel, "Selected/Target/a/Новая 🧠.md");
            reader.cancel_creation(window, cx);
            reader.new_folder(Some("Selected"), window, cx);
            let input = reader.creation.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("empty/nested", window, cx));
            reader.commit_creation(window, cx);
            assert!(reader.creation.is_none());
            assert!(reader
                .tree
                .rows
                .iter()
                .any(|row| row.path == "Selected/empty/nested"));
        });
        assert!(root.join("Selected/empty/nested").is_dir());
        assert!(!dir.join("outside.md").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("start.md")).unwrap(),
            "start"
        );
        let templates = root.join("_Assets/Templates");
        std::fs::create_dir_all(&templates).unwrap();
        std::fs::write(templates.join("Meeting.md"), "# {{title}}\n\nTemplate body").unwrap();
        reader.update_in(visual, |reader, window, cx| {
            // No tree selection: template creation follows the open note.
            reader.tree.cursor = None;
            reader.new_from_template(window, cx)
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            assert!(
                reader.file_menu.is_some(),
                "template choices are a popup menu"
            );
            assert!(
                !window.has_active_dialog(cx),
                "template choice is not a modal"
            );
            assert!(
                reader.creation.is_none(),
                "a template must be chosen explicitly"
            );
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            assert!(reader.file_menu.is_none());
            assert!(reader.creation.is_none());
            reader.new_from_template(window, cx);
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("down enter");
        visual.run_until_parked();
        assert!(!visual.did_prompt_for_new_path());
        reader.update_in(visual, |reader, window, cx| {
            let input = reader.creation.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("Meeting", window, cx));
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(root.join("Selected/Target/a/Meeting.md")).unwrap(),
            "# Meeting\n\nTemplate body"
        );
        assert_eq!(
            std::fs::read_to_string(templates.join("Meeting.md")).unwrap(),
            "# {{title}}\n\nTemplate body"
        );
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "Selected/Target/a/Meeting.md");
            assert!(reader.editing.is_some());
        });
        std::fs::create_dir_all(root.join("_Assets/Templates")).unwrap();
        std::fs::write(root.join("_Assets/Templates/Note.md"), "Default {{title}}").unwrap();
        std::fs::write(
            root.join("_Assets/Templates/Meeting.md"),
            "Meeting {{title}} {{date:YYYY-MM-DD}}",
        )
        .unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.new_note(Some(""), window, cx);
            assert_eq!(
                reader
                    .creation
                    .as_ref()
                    .unwrap()
                    .selected_template
                    .as_deref(),
                Some("Note.md")
            );
            reader.choose_creation_template(Some("Meeting.md".into()), window, cx);
            let input = reader.creation.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("Planning", window, cx));
            reader.commit_creation(window, cx);
            assert_eq!(reader.current_rel, "Planning.md");
            reader.new_note(None, window, cx);
            assert_eq!(
                reader
                    .creation
                    .as_ref()
                    .unwrap()
                    .selected_template
                    .as_deref(),
                Some("Note.md")
            );
            reader.cancel_creation(window, cx);
            reader.new_note(Some("_Assets/Templates"), window, cx);
            assert_eq!(reader.creation.as_ref().unwrap().folder, "");
            reader.cancel_creation(window, cx);
        });
        assert!(std::fs::read_to_string(root.join("Planning.md"))
            .unwrap()
            .starts_with("Meeting Planning "));
        assert_eq!(
            std::fs::read_to_string(root.join("_Assets/Templates/Meeting.md")).unwrap(),
            "Meeting {{title}} {{date:YYYY-MM-DD}}"
        );
        std::fs::remove_dir_all(root.join("_Assets/Templates")).unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.new_note(Some("_Assets/Templates"), window, cx);
            assert_eq!(reader.creation.as_ref().unwrap().folder, "");
            reader.cancel_creation(window, cx);
        });
        std::fs::remove_dir_all(dir).unwrap();
    }
}
