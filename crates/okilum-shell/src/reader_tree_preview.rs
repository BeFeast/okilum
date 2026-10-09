//! Tree-owned file preview. Ordinary activation retains its separate navigation behavior.
use super::*;

#[derive(Default)]
pub(super) struct Session {
    active: bool,
    #[cfg(all(target_os = "macos", not(test)))]
    panel: Option<native::Panel>,
}
impl Session {
    fn show(&mut self, path: &Path) -> anyhow::Result<()> {
        #[cfg(all(target_os = "macos", not(test)))]
        {
            if self.panel.is_none() {
                self.panel = Some(native::Panel::new()?);
            }
            self.panel.as_ref().unwrap().show(path)?;
        }
        #[cfg(any(not(target_os = "macos"), test))]
        let _ = path;
        self.active = true;
        Ok(())
    }
    pub(super) fn is_open(&self) -> bool {
        #[cfg(all(target_os = "macos", not(test)))]
        return self.active && self.panel.as_ref().is_some_and(native::Panel::is_open);
        #[cfg(any(not(target_os = "macos"), test))]
        self.active
    }
    pub(super) fn close(&mut self) {
        self.active = false;
        #[cfg(all(target_os = "macos", not(test)))]
        {
            self.panel = None;
        }
    }
}
impl Reader {
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
        let result = reader_files::checked_path(&self.vault_root, &row.path)
            .and_then(|path| self.tree_preview.show(&path));
        if let Err(error) = result {
            self.tree_preview.close();
            reader_toast::error(format!("Cannot preview file: {error}"), window, cx);
            return;
        }
        #[cfg(any(not(target_os = "macos"), test))]
        {
            self.preview_file(&row.path, window, cx);
            if self.file_preview.as_ref().is_none_or(|p| p.rel != row.path) {
                self.tree_preview.close();
            }
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
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[gpui::test]
    fn space_previews_selected_files_and_arrows_keep_compact_tree_focus(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
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
        visual.simulate_keystrokes("space");
        visual.run_until_parked();
        reader.update_in(visual, |r, window, _| {
            assert!(r.tree_preview.is_open());
            assert_eq!(r.file_preview.as_ref().unwrap().rel, "Alpha.bin");
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
            assert_eq!(r.file_preview.as_ref().unwrap().rel, "Beta.bin");
            assert!(r.tree_focus.is_focused(window));
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(r.file_preview.as_ref().unwrap().rel, "Alpha.bin")
        });
        visual.simulate_keystrokes("space down");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(!r.tree_preview.is_open());
            assert_eq!(r.tree.cursor.as_deref(), Some("Beta.bin"));
            assert_eq!(
                r.file_preview.as_ref().unwrap().rel,
                "Alpha.bin",
                "closed preview must not follow"
            );
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
