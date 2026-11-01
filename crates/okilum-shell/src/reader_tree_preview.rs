//! Tree-owned file preview. Ordinary activation retains its separate navigation behavior.
use super::*;

#[path = "reader_quick_look.rs"]
mod readonly;

#[derive(Default)]
pub(super) struct Session {
    preview: Option<(String, Entity<readonly::Preview>)>,
}
impl Session {
    pub(super) fn is_open(&self) -> bool {
        self.preview.is_some()
    }
    pub(super) fn close(&mut self) {
        self.preview = None;
    }
}
impl Reader {
    /// Render in the center before the ordinary document/editing surface. The
    /// underlying document, draft, history and file preview remain untouched.
    pub(super) fn render_tree_quick_look(&self) -> Option<AnyElement> {
        self.tree_preview.preview.as_ref().map(|(_, preview)| {
            div()
                .size_full()
                .min_h_0()
                .child(preview.clone())
                .into_any_element()
        })
    }

    pub(super) fn toggle_tree_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let attachment = self
            .tree
            .cursor_index()
            .is_some_and(|i| self.tree.rows[i].kind == okilum_core::vault::EntryKind::Attachment);
        if !attachment {
            self.tree_preview.close();
            self.tree_key(TreeKey::Open, window, cx);
        } else if self.tree_preview.is_open() {
            self.tree_preview.close();
            cx.notify();
        } else {
            self.preview_tree_selection(window, cx);
        }
    }
    pub(super) fn follow_tree_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tree_preview.is_open() {
            self.preview_tree_selection(window, cx);
        } else {
            self.tree_preview.close();
        }
    }
    fn preview_tree_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.tree.cursor_index().map(|i| self.tree.rows[i].clone()) else {
            self.tree_preview.close();
            return;
        };
        if row.kind != okilum_core::vault::EntryKind::Attachment {
            self.tree_preview.close();
            return;
        }
        let result = reader_files::checked_path(&self.vault_root, &row.path).and_then(|path| {
            anyhow::ensure!(path.is_file(), "Choose a file to preview");
            Ok(path)
        });
        match result {
            Ok(path) => {
                let root = self.vault_root.clone();
                let rel = row.path.clone();
                let preview = cx.new(|cx| readonly::Preview::new(root, rel, path, cx));
                self.tree_preview.preview = Some((row.path, preview));
            }
            Err(error) => {
                self.tree_preview.close();
                reader_toast::error(reader_files::preview_error_text(&error), window, cx);
                cx.notify();
                return;
            }
        }
        // In compact windows this is a browsing session, not a document activation.
        self.tree_focus.focus(window, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[gpui::test]
    fn space_previews_selected_files_and_arrows_keep_compact_tree_focus(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("Folder")).unwrap();
        std::fs::write(root.join("Alpha.bin"), [1, 2]).unwrap();
        std::fs::write(root.join("Beta.bin"), [3, 4]).unwrap();
        std::fs::write(root.join("Start.md"), "# Start").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let r = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Start.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(r.clone());
            Root::new(r, window, cx)
        });
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(800.), px(700.)));
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.reveal_in_tree("Alpha.bin", window, cx);
            r.tree.cursor = Some("Alpha.bin".into());
            r.tree_focus.focus(window, cx);
        });
        visual.run_until_parked();
        let history = reader.read_with(visual, |r, _| r.navigation.history.clone());
        visual.simulate_keystrokes("space");
        visual.run_until_parked();
        reader.update_in(visual, |r, window, _| {
            assert_eq!(r.navigation.history, history);
            assert_eq!(r.current_rel, "Start.md");
            assert!(r.file_preview.is_none());
            assert!(r.tree_preview.is_open());
            assert_eq!(r.tree_preview.preview.as_ref().unwrap().0, "Alpha.bin");
            assert!(r.tree_focus.is_focused(window));
            assert!(r.panels.visible(
                reader_layout::Panel::Notes,
                f32::from(r.body_bounds.size.width)
            ));
        });
        visual.simulate_keystrokes("down");
        visual.run_until_parked();
        reader.update_in(visual, |r, window, _| {
            assert_eq!(r.tree.cursor.as_deref(), Some("Beta.bin"));
            assert_eq!(r.tree_preview.preview.as_ref().unwrap().0, "Beta.bin");
            assert!(r.tree_focus.is_focused(window));
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(r.tree_preview.preview.as_ref().unwrap().0, "Alpha.bin")
        });
        visual.simulate_keystrokes("space down");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(!r.tree_preview.is_open());
            assert_eq!(r.tree.cursor.as_deref(), Some("Beta.bin"));
            assert!(r.tree_preview.preview.is_none());
            assert!(r.file_preview.is_none(), "preview must not navigate");
        });
        visual.simulate_keystrokes("space escape");
        visual.run_until_parked();
        reader.update_in(visual, |r, window, _| {
            assert!(!r.tree_preview.is_open());
            assert!(r.tree_focus.is_focused(window));
        });
        reader.update_in(visual, |r, window, cx| {
            r.tree.cursor = Some("Folder".into());
            r.tree_focus.focus(window, cx);
        });
        visual.simulate_keystrokes("space");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(
                r.tree
                    .rows
                    .iter()
                    .find(|row| row.path == "Folder")
                    .unwrap()
                    .expanded
            );
            assert!(!r.tree_preview.is_open());
        });
        reader.update_in(visual, |r, window, cx| {
            r.tree.cursor = Some("Start.md".into());
            r.tree_focus.focus(window, cx);
        });
        visual.simulate_keystrokes("space");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "Start.md");
            assert!(r.file_preview.is_none());
            assert!(!r.tree_preview.is_open());
        });
    }
}
