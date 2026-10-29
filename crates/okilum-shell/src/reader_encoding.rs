//! Encoding Repair is explicit; merely displaying a damaged note never writes it.
use super::*;
use okilum_core::file_editor::{FileEditor, Save};

impl Reader {
    pub(super) fn repair_note_encoding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.note_encoding_damaged
            || self.current_rel.is_empty()
            || self.encoding_repair_pending.is_some()
            || self.move_applying
            || self.editing.is_some()
        {
            return;
        }
        let Some(state) = self.session_directory.clone() else {
            self.link_notice = Some(
                "Repair needs Note history storage. The original file has not changed.".into(),
            );
            cx.notify();
            return;
        };
        let root = self.vault_root.clone();
        let rel = self.current_rel.clone();
        let path = root.join(&rel);
        self.encoding_repair_pending = Some(path.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let source = path.clone();
            let result = cx.background_executor().spawn(async move {
                FileEditor::repair_encoding(&source, &state.join("editor-drafts"))
            }).await;
            let _ = this.update_in_own_window(cx, |reader, window, cx| {
                if reader.encoding_repair_pending.as_ref() == Some(&path) {
                    reader.encoding_repair_pending = None;
                }
                if reader.vault_root != root || reader.current_rel != rel { cx.notify(); return; }
                match result {
                    Ok(Save::Saved) => {
                        reader.link_notice = None;
                        reader.prepare_document(&rel, None, None, window, cx);
                        reader_toast::transient("Encoding repaired. Original bytes are in Note history.", window, cx);
                    }
                    Ok(Save::Conflict) => {
                        reader.link_notice = Some("The note changed during Repair. Review its current contents before trying again.".into());
                    }
                    Err(error) => {
                        eprintln!("Encoding Repair failed: {error:#}");
                        reader.link_notice = Some("Encoding could not be repaired. The note and its recovery have been preserved.".into());
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn restore_source_bytes(
        &mut self,
        reviewed: &[u8],
        proposed: &[u8],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.source_is_dirty(cx),
            "Save or discard unsaved edits before restoring a version"
        );
        self.check_move_editors(std::slice::from_ref(&self.current_rel), cx)?;
        if self.editing.is_some() {
            anyhow::ensure!(
                self.leave_source(cx),
                "The editor is still saving; open History again after it finishes"
            );
        }
        let state = self
            .session_directory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No recovery storage"))?;
        let result = FileEditor::replace_bytes(
            &self.vault_root.join(&self.current_rel),
            &state.join("editor-drafts"),
            reviewed,
            proposed,
        )?;
        anyhow::ensure!(
            result == Save::Saved,
            "The note changed after preview; open History again"
        );
        let rel = self.current_rel.clone();
        self.prepare_document(&rel, None, None, window, cx);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn repair_button_unlocks_edit_and_history_restores_the_original_bytes(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let state = fixture.path().join("state");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("Synthetic.md");
        let original = b"# Synthetic note\n\nBefore \xff After\n";
        std::fs::write(&path, original).unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Synthetic.md".into()),
                        session_directory: Some(state.clone()),
                        index_dir: Some(fixture.path().join("index")),
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
        visual.simulate_resize(size(px(1400.), px(960.)));
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert!(r.note_encoding_damaged);
            r.toggle_source(window, cx);
            assert!(r.editing.is_none());
        });
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "no click never changes a byte"
        );
        let repair = visual.debug_bounds("note-encoding-repair").unwrap();
        visual.simulate_click(repair.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            String::from_utf8_lossy(original)
        );
        reader.update_in(visual, |r, window, cx| {
            assert!(!r.note_encoding_damaged);
            assert!(r.encoding_repair_pending.is_none());
            r.toggle_source(window, cx);
            assert!(r.editing.is_some(), "Repair unlocks the real editor");
            r.toggle_source(window, cx);
        });
        visual.run_until_parked();
        let history =
            okilum_core::source_history::list(&state.join("editor-drafts"), &root).unwrap();
        assert_eq!(history.versions.len(), 1);
        assert_eq!(history.versions[0].bytes(), original);
        reader.update_in(visual, |r, window, cx| r.open_timeline(window, cx));
        visual.run_until_parked();
        let version = visual.debug_bounds("history-version-0").unwrap();
        visual.simulate_click(version.center(), Modifiers::default());
        visual.run_until_parked();
        let restore = visual.debug_bounds("history-restore").unwrap();
        visual.simulate_click(restore.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "History restores the same file byte for byte"
        );
        reader.update_in(visual, |r, window, cx| {
            assert!(r.note_encoding_damaged);
            r.toggle_source(window, cx);
            assert!(r.editing.is_none());
        });
        assert!(visual.debug_bounds("note-encoding-notice").is_some());
        assert!(std::fs::read_dir(&root).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".okilum-save-")));
    }
}
