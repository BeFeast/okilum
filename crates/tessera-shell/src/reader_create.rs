//! Inline, create-only Reader actions (#466).
use super::*;

pub(super) struct Creation {
    root: PathBuf,
    pub folder: String,
    pub directory: bool,
    pub input: Entity<InputState>,
    pub error: Option<String>,
    pub templates: Option<tessera_core::note_templates::Catalog>,
    pub selected_template: Option<String>,
    _subscription: Subscription,
}

impl Reader {
    pub(super) fn create_note_with_source(
        &mut self,
        path: &Path,
        source: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = (|| -> anyhow::Result<String> {
            if self.loading.as_ref().is_some_and(|l| l.active) {
                anyhow::bail!("Wait for the folder to finish opening and try again");
            }
            if self.session_directory.is_none() {
                anyhow::bail!("No draft recovery storage is available");
            }
            let mut path = path.to_path_buf();
            if path.extension().is_none() {
                path.set_extension("md");
            }
            let relative = path
                .strip_prefix(&self.vault_root)
                .map_err(|_| anyhow::anyhow!("Choose a location inside the open folder"))?;
            let rel = relative
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Use a UTF-8 filename"))?
                .to_owned();
            // Resolve/save the current draft before creating any new file.
            if !self.save_source(cx) {
                anyhow::bail!("Resolve the current note's save before creating another note");
            }
            tessera_core::note_files::create_with_source(
                &self.vault_root,
                relative,
                source.as_bytes(),
            )?;
            Ok(rel)
        })();
        match result {
            Ok(rel) => {
                self.editing = None;
                self.document_preparation_generation =
                    self.document_preparation_generation.wrapping_add(1);
                let document =
                    tessera_core::render::reader_document_from_source(&self.vault, &rel, source);
                self.accept_prepared_document(
                    prepared_links::DocumentRequest {
                        rel,
                        jump: None,
                        heading: None,
                        history_index: None,
                        restore_position: None,
                    },
                    Ok(prepared_links::PreparedDocument {
                        source: document.rendered,
                        original: Some(document.original_body),
                        identities: document.links,
                        frontmatter: document.frontmatter,
                    }),
                    window,
                    cx,
                );
                self.toggle_source(window, cx);
            }
            Err(error) => {
                self.link_notice = Some(format!(
                    "Could not create note: {error:#}. The destination was not replaced."
                ))
            }
        }
        cx.notify();
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
        if self.loading.as_ref().is_some_and(|l| l.active) || self.session_directory.is_none() {
            self.link_notice =
                Some("Wait for the vault and draft recovery storage before creating files.".into());
            cx.notify();
            return;
        }
        let mut folder = folder.map(str::to_owned).unwrap_or_else(|| {
            Path::new(&self.current_rel)
                .parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .into_owned()
        });
        let templates = tessera_core::note_templates::Catalog::load(&self.vault_root);
        // A template opened for inspection must not turn Cmd-N into a write
        // inside the templates collection.
        if templates
            .as_ref()
            .is_ok_and(|catalog| catalog.contains_target(Path::new(&folder)))
        {
            folder.clear();
        }
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
        let folder = create.folder.clone();
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
                tessera_core::note_files::typed_path(Path::new(&folder), &name, directory)?;
            let rel = relative
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Use a UTF-8 filename"))?
                .to_owned();
            let source = if directory {
                tessera_core::note_files::create_folders(&root, &relative, true)?;
                None
            } else {
                anyhow::ensure!(
                    self.save_source(cx),
                    "Resolve the current note's save before creating another"
                );
                let state = self
                    .session_directory
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("No draft recovery storage"))?;
                let now = time::OffsetDateTime::now_local()
                    .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
                let catalog = templates.as_ref().ok_or_else(|| anyhow::anyhow!("Templates could not be loaded. Cancel and retry after fixing templates.json"))?;
                Some(tessera_core::note_files::create_from_template(
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
                self.creation = None;
                self.editing = None;
                self.document_preparation_generation =
                    self.document_preparation_generation.wrapping_add(1);
                let document =
                    tessera_core::render::reader_document_from_source(&self.vault, &rel, source);
                self.accept_prepared_document(
                    prepared_links::DocumentRequest {
                        rel,
                        jump: None,
                        heading: None,
                        history_index: None,
                        restore_position: None,
                    },
                    Ok(prepared_links::PreparedDocument {
                        source: source.clone(),
                        original: Some(source),
                        identities: Vec::new(),
                        frontmatter: None,
                    }),
                    window,
                    cx,
                );
                self.toggle_source(window, cx);
            }
            Ok((rel, None)) => {
                self.creation = None;
                // Watcher refreshes the inventory; reveal an empty new folder now.
                let mut entries = self.vault.entries.clone();
                entries.push(tessera_core::vault::VaultEntry {
                    path: rel.clone(),
                    kind: tessera_core::vault::EntryKind::Directory,
                });
                self.tree.refresh(&root, &entries);
                self.reveal_in_tree(&rel, window, cx);
            }
            Err(error) => {
                if let Some(create) = self.creation.as_mut() {
                    create.error = Some(format!("{error:#}"));
                }
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[gpui::test]
    fn inline_create_templates_nested_paths_collision_and_cancel(cx: &mut TestAppContext) {
        cx.update(|cx| {
            // This test verifies file creation, not dialog animation. Keep the
            // hit target stable between selector lookup and dispatched input.
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let dir = std::env::temp_dir().join(format!("tessera-create-{}", uuid::Uuid::new_v4()));
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
            assert!(reader.editing.is_some());
            assert_eq!(
                reader.history.last().map(String::as_str),
                Some("Selected/Target/a/Новая 🧠.md")
            );
            assert!(reader.history.iter().any(|path| path == "start.md"));
        });
        let created = std::fs::read_to_string(root.join("Selected/Target/a/Новая 🧠.md")).unwrap();
        assert!(created.starts_with("---\ntype: Note\ncreated: "));
        assert!(created.ends_with("# Новая 🧠\n"));
        reader.update_in(visual, |reader, window, cx| {
            reader.new_note(None, window, cx);
            assert_eq!(
                reader.creation.as_ref().unwrap().folder,
                "Selected/Target/a"
            );
        });
        visual.simulate_keystrokes("escape");
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
            reader.new_from_template(window, cx)
        });
        visual.run_until_parked();
        let choice = visual
            .debug_bounds("template-choice-0")
            .expect("template choice shown");
        // Input can arrive on a later frame than the selector measurement.
        // Advance virtual time, not wall-clock time, to exercise that boundary.
        visual
            .executor()
            .advance_clock(std::time::Duration::from_millis(125));
        visual.update(|window, _| window.refresh());
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("template-choice-0"), Some(choice));
        visual.simulate_click(choice.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.did_prompt_for_new_path());
        visual.simulate_new_path_selection(|_| Some(root.join("Selected/Meeting.md")));
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(root.join("Selected/Meeting.md")).unwrap(),
            "# Meeting\n\nTemplate body"
        );
        assert_eq!(
            std::fs::read_to_string(templates.join("Meeting.md")).unwrap(),
            "# {{title}}\n\nTemplate body"
        );
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "Selected/Meeting.md");
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
        std::fs::remove_dir_all(dir).unwrap();
    }
}
