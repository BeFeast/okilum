//! Tree-owned file preview. Ordinary activation retains its separate navigation behavior.
use super::*;

#[path = "reader_quick_look.rs"]
mod readonly;

#[cfg(test)]
type NativePresenter = Rc<dyn Fn(&Path) -> anyhow::Result<bool>>;

#[derive(Default)]
pub(super) struct Session {
    active: bool,
    preview: Option<(String, Entity<readonly::Preview>)>,
    #[cfg(test)]
    native_presenter: Option<NativePresenter>,
    #[cfg(all(target_os = "macos", not(test)))]
    panel: Option<native::Panel>,
}
impl Session {
    /// A failed system provider must not prevent keyboard-only browsing. Keep
    /// the bounded reader preview for the rest of that browsing session.
    fn show_native(&mut self, path: &Path) -> anyhow::Result<bool> {
        if self.preview.is_some() {
            return Ok(false);
        }
        #[cfg(test)]
        if let Some(present) = &self.native_presenter {
            return present(path);
        }
        #[cfg(all(target_os = "macos", not(test)))]
        {
            if self.panel.is_none() {
                self.panel = Some(native::Panel::new()?);
            }
            self.panel.as_ref().unwrap().show(path)?;
            Ok(true)
        }
        #[cfg(any(not(target_os = "macos"), test))]
        {
            let _ = path;
            Ok(false)
        }
    }
    pub(super) fn is_open(&self) -> bool {
        #[cfg(all(target_os = "macos", not(test)))]
        return self.active
            && (self.preview.is_some() || self.panel.as_ref().is_some_and(native::Panel::is_open));
        #[cfg(any(not(target_os = "macos"), test))]
        self.active
    }
    pub(super) fn close(&mut self) {
        self.active = false;
        self.preview = None;
        #[cfg(all(target_os = "macos", not(test)))]
        {
            self.panel = None;
        }
    }
}
impl Reader {
    /// Read-only center surface; the document, editor and navigation stay owned
    /// by their existing controllers. macOS prefers its native Quick Look panel
    /// and uses this surface if the system cannot supply a controller.
    /// `render_tree_quick_look` draws a preview in place of the document.
    pub(super) fn tree_quick_look_shown(&self) -> bool {
        self.tree_preview.preview.is_some()
    }

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
            let native = match self.tree_preview.show_native(&path) {
                Ok(shown) => shown,
                Err(error) => {
                    diagnostic_error!(
                        "System Quick Look unavailable; using reader preview: {error:#}"
                    );
                    #[cfg(all(target_os = "macos", not(test)))]
                    {
                        self.tree_preview.panel = None;
                    }
                    false
                }
            };
            if !native {
                let root = self.vault_root.clone();
                let rel = row.path.clone();
                let preview = cx.new(|cx| readonly::Preview::new(root, rel, path, window, cx));
                self.tree_preview.preview = Some((row.path.clone(), preview));
            }
            self.tree_preview.active = true;
            Ok(())
        });
        if let Err(error) = result {
            self.tree_preview.close();
            diagnostic_error!("Cannot preview file {}: {error:#}", row.path);
            reader_toast::error(reader_files::preview_error_text(&error), window, cx);
            return;
        }
        // In compact windows this is a browsing session, not a document activation.
        self.tree_focus.focus(window, cx);
        cx.notify();
    }
}

#[cfg(all(target_os = "macos", not(test)))]
mod native {
    use super::*;
    use std::{
        ffi::{c_char, c_void, CString},
        os::unix::ffi::OsStrExt,
        ptr::NonNull,
    };
    unsafe extern "C" {
        fn okilum_quicklook_new() -> *mut c_void;
        fn okilum_quicklook_show(panel: *mut c_void, path: *const c_char) -> bool;
        fn okilum_quicklook_visible(panel: *mut c_void) -> bool;
        fn okilum_quicklook_release(panel: *mut c_void);
    }
    // Owned by Reader on the AppKit/GPUI foreground thread. Not Send or Sync.
    pub(super) struct Panel(NonNull<c_void>);
    impl Panel {
        pub(super) fn new() -> anyhow::Result<Self> {
            NonNull::new(unsafe { okilum_quicklook_new() })
                .map(Self)
                .ok_or_else(|| anyhow::anyhow!("Quick Look could not open"))
        }
        pub(super) fn show(&self, path: &Path) -> anyhow::Result<()> {
            let path = CString::new(path.as_os_str().as_bytes())?;
            anyhow::ensure!(
                unsafe { okilum_quicklook_show(self.0.as_ptr(), path.as_ptr()) },
                "Quick Look could not load this file"
            );
            Ok(())
        }
        pub(super) fn is_open(&self) -> bool {
            unsafe { okilum_quicklook_visible(self.0.as_ptr()) }
        }
    }
    impl Drop for Panel {
        fn drop(&mut self) {
            unsafe { okilum_quicklook_release(self.0.as_ptr()) };
        }
    }
}

#[cfg(test)]
#[path = "reader_tree_preview/keyboard_tests.rs"]
mod keyboard_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    /// #3182 review: Today's note pressed while Quick Look shows an attachment
    /// closes the preview, so the newly created note's editor is on screen.
    #[cfg(unix)]
    #[gpui::test]
    fn todays_note_closes_quick_look_and_shows_the_new_note(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Alpha.bin"), [1, 2]).unwrap();
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
        visual.simulate_resize(size(px(1200.), px(700.)));
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.reveal_in_tree("Alpha.bin", window, cx);
            r.tree.cursor = Some("Alpha.bin".into());
            r.tree_focus.focus(window, cx);
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("space");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(r.tree_preview.is_open(), "control: Quick Look is open");
            assert!(r.tree_quick_look_shown(), "control: it draws over the note");
        });
        reader.read_with(visual, |r, _| {
            let state = r.session_directory.clone().unwrap();
            crate::reader_daily_notes::save(
                &state,
                &r.vault_root,
                &okilum_core::daily_notes::Settings::default(),
            )
            .unwrap();
        });
        let rel = okilum_core::vault::note_path(
            &okilum_core::daily_notes::Settings::default()
                .relative_path(crate::reader_daily_notes::now())
                .unwrap(),
        );
        assert!(!root.join(&rel).exists());
        reader.update_in(visual, |r, window, cx| r.open_daily_note(window, cx));
        visual.run_until_parked();
        assert!(root.join(&rel).is_file(), "today's note was created");
        reader.read_with(visual, |r, _| {
            assert!(!r.tree_preview.is_open());
            assert!(!r.tree_quick_look_shown());
            assert_eq!(r.pane.current_rel, rel);
            assert!(r.editing.is_some());
        });
        assert!(
            visual.debug_bounds("reader-source").is_some(),
            "the new note's editor is drawn, not Quick Look"
        );
    }

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
            assert_eq!(r.pane.current_rel, "Start.md");
            assert!(r.pane.file_preview.is_none());
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
            assert!(r.pane.file_preview.is_none(), "preview must not navigate");
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
            assert_eq!(r.pane.current_rel, "Start.md");
            assert!(r.pane.file_preview.is_none());
            assert!(!r.tree_preview.is_open());
        });
    }
}
