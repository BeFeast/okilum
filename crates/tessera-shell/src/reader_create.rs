//! Explicit create-only Reader action, with the native folder/name picker.
use super::*;

impl Reader {
    pub(super) fn new_note(
        &mut self,
        folder: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading.as_ref().is_some_and(|l| l.active) {
            self.link_notice =
                Some("Wait for the folder to finish opening before creating a note.".into());
            cx.notify();
            return;
        }
        if self.session_directory.is_none() {
            self.link_notice = Some("Cannot create a note without draft recovery storage.".into());
            cx.notify();
            return;
        }
        let root = self.vault_root.clone();
        let parent = folder.map(Path::new).unwrap_or_else(|| {
            Path::new(&self.current_rel)
                .parent()
                .unwrap_or(Path::new(""))
        });
        let picker = cx.prompt_for_new_path(&root.join(parent), Some("Untitled.md"));
        cx.spawn_in(window, async move |this, cx| {
            let result = picker.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Ok(Some(path))) if this.vault_root == root => {
                        this.create_note_at(&path, window, cx)
                    }
                    Ok(Ok(None)) => {}
                    Ok(Ok(Some(_))) => {
                        this.link_notice =
                            Some("The open folder changed. Choose New note again.".into())
                    }
                    _ => {
                        this.link_notice =
                            Some("Could not open the new note dialog. Please try again.".into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn create_note_at(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        self.create_note_with_source(path, "", window, cx);
    }

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[gpui::test]
    fn picker_creates_in_selected_folder_and_enters_source(cx: &mut TestAppContext) {
        cx.update(|cx| {
            // This test verifies file creation, not dialog animation. Keep the
            // hit target stable between selector lookup and dispatched input.
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let dir = std::env::temp_dir().join(format!("tessera-create-{}", uuid::Uuid::new_v4()));
        let root = dir.join("notes");
        std::fs::create_dir_all(root.join("Selected")).unwrap();
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
            reader.new_note(Some("Selected"), window, cx)
        });
        assert!(visual.did_prompt_for_new_path());
        visual.simulate_new_path_selection(|directory| {
            assert_eq!(directory, root.join("Selected"));
            Some(directory.join("Новая 🧠.md"))
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "Selected/Новая 🧠.md");
            assert!(reader.editing.is_some());
            assert_eq!(
                reader.history.last().map(String::as_str),
                Some("Selected/Новая 🧠.md")
            );
            assert!(reader.history.iter().any(|path| path == "start.md"));
        });
        assert_eq!(
            std::fs::read(root.join("Selected/Новая 🧠.md")).unwrap(),
            b""
        );
        reader.update_in(visual, |reader, window, cx| {
            reader.new_note(None, window, cx)
        });
        visual.simulate_new_path_selection(|_| None);
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| assert!(reader.editing.is_some()));
        reader.update_in(visual, |reader, window, cx| {
            reader.create_note_at(&dir.join("outside.md"), window, cx)
        });
        assert!(!dir.join("outside.md").exists());
        reader.read_with(visual, |reader, _| {
            assert!(reader
                .link_notice
                .as_ref()
                .unwrap()
                .contains("inside the open folder"))
        });
        // An existing destination is never replaced, even if a native picker accepts it.
        reader.update_in(visual, |reader, window, cx| {
            reader.create_note_at(&root.join("start.md"), window, cx)
        });
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "Selected/Новая 🧠.md");
            assert!(reader
                .link_notice
                .as_ref()
                .unwrap()
                .contains("Could not create"));
        });
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
        std::fs::remove_dir_all(dir).unwrap();
    }
}
