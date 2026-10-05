//! Explicit system Trash with confirmation and a non-overwriting Undo action.
use super::*;
use gpui_component::{notification::Notification, WindowExt};
use tessera_core::file_editor::FileEditor;

fn under(path: &str, parent: &str) -> bool {
    Path::new(path).starts_with(parent)
}

impl Reader {
    pub(super) fn delete_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = if self.tree_focus.contains_focused(window, cx) {
            self.tree.cursor.clone().unwrap_or_default()
        } else {
            self.selected_file().to_owned()
        };
        self.delete_path(path, window, cx);
    }

    pub(super) fn delete_path(
        &mut self,
        relative: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if relative.is_empty()
            || self.trash_pending
            || self.note_move_pending
            || self.loading.as_ref().is_some_and(|l| l.active)
        {
            return;
        }
        let Some(state) = self.session_directory.clone() else {
            return;
        };
        if !self.save_source(cx) {
            return;
        }
        let root = self.vault_root.clone();
        let directory = root.join(&relative).is_dir();
        let targets: Vec<_> = self
            .vault
            .entries
            .iter()
            .filter(|e| under(&e.path, &relative))
            .map(|e| e.path.clone())
            .collect();
        let incoming: usize = targets
            .iter()
            .map(|path| {
                self.vault
                    .backlinks(path)
                    .iter()
                    .filter(|link| !under(&link.path, &relative))
                    .count()
            })
            .sum();
        // Release only this window's saved editor; other writers and orphaned
        // drafts remain protected by the same locks as ordinary source saves.
        if under(self.selected_file(), &relative) {
            self.editing = None;
        }
        self.trash_pending = true;
        window.push_notification("Preparing to move to Trash…", cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let scan_root = root.clone(); let scan_relative = relative.clone();
            let prepared = cx.background_executor().spawn(async move {
                let mut pending = vec![scan_root.join(&scan_relative)];
                let mut count = 0usize; let mut locks = Vec::new();
                while let Some(path) = pending.pop() {
                    let meta = std::fs::symlink_metadata(&path)?;
                    if meta.is_dir() {
                        for entry in std::fs::read_dir(path)? { pending.push(entry?.path()); }
                    } else {
                        count += 1;
                        if meta.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")) {
                            locks.push(FileEditor::reserve_destination(&path, &state.join("editor-drafts"))?);
                        }
                    }
                }
                Ok::<_, anyhow::Error>((count, locks))
            }).await;
            let (count, _locks) = match prepared {
                Ok(value) => value,
                Err(error) => { let _ = this.update_in(cx, |this, window, cx| {
                    this.trash_pending = false;
                    window.push_notification(format!("Cannot move to Trash: {error:#}"), cx); cx.notify();
                }); return; }
            };
            if directory || incoming > 0 {
                let (send, receive) = async_channel::bounded(1);
                let shown = this.update_in(cx, |this, window, cx| {
                    if this.vault_root != root { this.trash_pending = false; return false; }
                    let message = format!("Move {relative} to system Trash? {count} files; {incoming} incoming links. Links will remain unchanged. You can Undo from the notification.");
                    window.open_dialog(cx, move |dialog, _, _| {
                        let yes = send.clone(); let no = send.clone(); let close = send.clone();
                        dialog.title("Move to Trash").child(message.clone())
                            .on_ok(move |_, _, _| { let _ = yes.try_send(true); true })
                            .on_cancel(move |_, _, _| { let _ = no.try_send(false); true })
                            .child({
                                let yes = send.clone(); let no = send.clone();
                                h_flex().gap_2()
                                    .child(Button::new("confirm-trash").debug_selector(|| "confirm-trash".into()).label("Move to Trash").on_click(move |_, window, cx| { let _ = yes.try_send(true); window.close_dialog(cx); }))
                                    .child(Button::new("cancel-trash").debug_selector(|| "cancel-trash".into()).label("Cancel").on_click(move |_, window, cx| { let _ = no.try_send(false); window.close_dialog(cx); }))
                            })
                            .on_close(move |_, _, _| { let _ = close.try_send(false); })
                    }); true
                }).unwrap_or(false);
                if !shown || receive.recv().await != Ok(true) {
                    let _ = this.update(cx, |this, cx| {this.trash_pending = false; cx.notify();}); return;
                }
            }
            let allowed = this.update_in(cx, |this, window, cx| {
                if this.vault_root != root { this.trash_pending = false; return false; }
                if under(this.selected_file(), &relative) { this.show_empty_vault(window, cx); }
                true
            }).unwrap_or(false);
            if !allowed { return; }
            let result = cx.background_executor().spawn(async move {
                reader_trash_fs::move_to_trash(&root, Path::new(&relative))
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.trash_pending = false;
                match result {
                    Ok(trashed) => {
                        let reader = cx.entity().downgrade();
                        window.push_notification(Notification::new().message("Moved to Trash").action(move |_, _, _| {
                            let trashed = trashed.clone(); let reader = reader.clone();
                            Button::new("undo-trash").label("Undo").on_click(move |_, window, cx| {
                                let _ = reader.update(cx, |this, cx| this.undo_trash(trashed.clone(), window, cx));
                            })
                        }), cx);
                    }
                    Err(error) => window.push_notification(format!("Cannot move to Trash: {error:#}"), cx),
                }
                cx.notify();
            });
        }).detach();
    }

    fn undo_trash(
        &mut self,
        trashed: reader_trash_fs::Trashed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.trash_pending {
            return;
        }
        let Some(state) = self.session_directory.clone() else {
            return;
        };
        self.trash_pending = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let _guard = if trashed
                        .relative
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
                    {
                        Some(FileEditor::reserve_destination(
                            &trashed.root.join(&trashed.relative),
                            &state.join("editor-drafts"),
                        )?)
                    } else {
                        None
                    };
                    trashed.restore()
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.trash_pending = false;
                window.push_notification(
                    match result {
                        Ok(()) => "Restored from Trash".into(),
                        Err(error) => format!("Cannot Undo: {error:#}"),
                    },
                    cx,
                );
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[gpui::test]
    fn incoming_links_require_confirmation_and_cancel_preserves_source(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("target.md"), "# Target\n").unwrap();
        std::fs::write(root.join("source.md"), "[[target]]\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("target.md")),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
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
            reader.delete_path("target.md".into(), window, cx)
        });
        visual.run_until_parked();
        let cancel = visual
            .debug_bounds("cancel-trash")
            .expect("incoming links require confirmation");
        assert!(root.join("target.md").exists());
        visual.simulate_click(cancel.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| assert!(!reader.trash_pending));
        assert_eq!(
            std::fs::read_to_string(root.join("target.md")).unwrap(),
            "# Target\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("source.md")).unwrap(),
            "[[target]]\n"
        );
    }
}
