//! Explicit system Trash with confirmation and a non-overwriting Undo action.
use super::*;
use gpui_component::{notification::Notification, WindowExt};
use std::os::unix::fs::MetadataExt;
use tessera_core::file_editor::FileEditor;

#[derive(Default)]
pub(super) struct UndoHistory {
    items: Vec<reader_trash_fs::Trashed>,
    generation: u64,
    visible: std::rc::Rc<std::cell::Cell<bool>>,
}
struct TrashToast;

const TRASH_TOAST_LIFETIME: std::time::Duration = std::time::Duration::from_secs(8);

type Inventory = Vec<(PathBuf, u64, u64, u64, i64, i64)>;
fn inventory(path: &Path) -> anyhow::Result<Inventory> {
    let mut pending = vec![path.to_owned()];
    let mut entries = Vec::new();
    while let Some(path) = pending.pop() {
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            for entry in std::fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        }
        entries.push((
            path,
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
        ));
    }
    entries.sort();
    Ok(entries)
}

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
        let own_path = self
            .editing
            .as_ref()
            .filter(|_| under(self.selected_file(), &relative))
            .map(|_| root.join(self.selected_file()));
        self.trash_pending = true;
        window.push_notification("Preparing to move to Trash…", cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let scan_root = root.clone(); let scan_relative = relative.clone();
            let scan_own = own_path.clone(); let scan_state = state.clone();
            let prepared = cx.background_executor().spawn(async move {
                let before = inventory(&scan_root.join(&scan_relative))?;
                let mut pending = vec![scan_root.join(&scan_relative)];
                let mut count = 0usize; let mut locks = Vec::new();
                while let Some(path) = pending.pop() {
                    let meta = std::fs::symlink_metadata(&path)?;
                    if meta.is_dir() {
                        for entry in std::fs::read_dir(path)? { pending.push(entry?.path()); }
                    } else {
                        count += 1;
                        if scan_own.as_ref() != Some(&path) && meta.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")) {
                            locks.push(FileEditor::reserve_destination(&path, &scan_state.join("editor-drafts"))?);
                        }
                    }
                }
                anyhow::ensure!(inventory(&scan_root.join(&scan_relative))? == before, "The item changed while preparing Trash. Try again");
                Ok::<_, anyhow::Error>((count, locks, before))
            }).await;
            let (count, _locks, before) = match prepared {
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
                    let message = format!("{relative}\n{count} files · {incoming} incoming links. Links stay unchanged.");
                    window.open_dialog(cx, move |dialog, _, cx| {
                        let yes = send.clone(); let no = send.clone(); let close = send.clone();
                        dialog.title("Move to Trash").child(message.clone())
                            .on_ok(move |_, _, _| { let _ = yes.try_send(true); true })
                            .on_cancel(move |_, _, _| { let _ = no.try_send(false); true })
                            .footer({
                                let yes = send.clone(); let no = send.clone();
                                h_flex().justify_end().gap_2()
                                    .child(Button::new("cancel-trash").debug_selector(|| "cancel-trash".into()).ghost().label("Cancel").on_click(move |_, window, cx| { let _ = no.try_send(false); window.close_dialog(cx); }))
                                    .child(reader_icon_button("confirm-trash", Icon::default().path("icons/trash.svg"), "Move to Trash", cx).danger().debug_selector(|| "confirm-trash".into()).on_click(move |_, window, cx| { let _ = yes.try_send(true); window.close_dialog(cx); }))
                            })
                            .on_close(move |_, _, _| { let _ = close.try_send(false); })
                    }); true
                }).unwrap_or(false);
                if !shown || receive.recv().await != Ok(true) {
                    let _ = this.update(cx, |this, cx| {this.trash_pending = false; cx.notify();}); return;
                }
            }
            let held_editor = match this.update_in(cx, |this, _, cx| {
                if this.vault_root != root || !this.save_source(cx) {
                    this.trash_pending = false; cx.notify(); return None;
                }
                let editor = if own_path.as_ref() == Some(&root.join(this.selected_file())) {
                    this.editing.take()
                } else { None };
                Some(editor)
            }) { Ok(Some(editor)) => editor, _ => return };
            let held_path = own_path.clone();
            let has_editor = held_editor.is_some();
            let move_root = root.clone(); let move_relative = relative.clone();
            let result = cx.background_executor().spawn(async move {
                let _late_guard = match own_path.filter(|_| !has_editor) {
                    Some(path) => Some(FileEditor::reserve_destination(&path, &state.join("editor-drafts"))?),
                    None => None,
                };
                anyhow::ensure!(inventory(&move_root.join(&move_relative))? == before, "The item changed since confirmation. Nothing was moved; try again");
                reader_trash_fs::move_to_trash(&move_root, Path::new(&move_relative))
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.trash_pending = false;
                match result {
                    Ok(trashed) => {
                        if this.vault_root == root {
                            let mut changes = tessera_core::Changes::default();
                            if directory { changes.directories.insert(relative.clone()); } else { changes.removed.insert(relative.clone()); }
                            this.queue_vault_mutation(changes, cx);
                        }
                        if this.vault_root == root && under(this.selected_file(), &relative) {
                            this.editing = None;
                            this.show_empty_vault(window, cx);
                        }
                        this.trash_undo.items.push(trashed);
                        this.show_trash_toast(window, cx);
                    }
                    Err(error) => {
                        if this.vault_root == root && held_path.as_ref() == Some(&root.join(this.selected_file())) && this.editing.is_none() {
                            this.editing = held_editor;
                        }
                        window.push_notification(format!("Cannot move to Trash: {error:#}"), cx);
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    fn show_trash_toast(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_trash_toast(window, cx);
        self.trash_undo.generation = self.trash_undo.generation.wrapping_add(1);
        let generation = self.trash_undo.generation;
        let visible = std::rc::Rc::new(std::cell::Cell::new(true));
        self.trash_undo.visible = visible.clone();
        let reader = cx.entity().downgrade();
        window.push_notification(
            Notification::new()
                .id::<TrashToast>()
                .message("Moved to Trash")
                .py_2()
                .placement(Anchor::BottomRight)
                .on_close(move |_, _| visible.set(false))
                .action(move |_, _, cx| {
                    let reader = reader.clone();
                    reader_icon_button(
                        "undo-trash",
                        IconName::Undo2,
                        if cfg!(target_os = "macos") {
                            "Undo (⌘Z)"
                        } else {
                            "Undo (Ctrl+Z)"
                        },
                        cx,
                    )
                    .debug_selector(|| "undo-trash".into())
                    .on_click(move |_, window, cx| {
                        let _ = reader.update(cx, |this, cx| this.undo_last_trash(window, cx));
                    })
                }),
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(TRASH_TOAST_LIFETIME).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.trash_undo.generation == generation {
                    this.dismiss_trash_toast(window, cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn dismiss_trash_toast(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.trash_undo.visible.replace(false) {
            return false;
        }
        window.remove_notification::<TrashToast>(cx);
        true
    }

    pub(super) fn undo_last_trash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(trashed) = self.trash_undo.items.last().cloned() else {
            return;
        };
        if self.trash_pending {
            return;
        }
        let Some(state) = self.session_directory.clone() else {
            return;
        };
        let restored_root = trashed.root.clone();
        let restored_path = trashed.relative.clone();
        self.dismiss_trash_toast(window, cx);
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
                    trashed
                        .restore()
                        .map(|()| trashed.root.join(&trashed.relative).is_dir())
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.trash_pending = false;
                if let Ok(directory) = &result {
                    if this.vault_root == restored_root {
                        let mut changes = tessera_core::Changes::default();
                        if *directory {
                            changes
                                .directories
                                .insert(tessera_core::vault::note_path(&restored_path));
                        } else {
                            changes
                                .changed
                                .insert(tessera_core::vault::note_path(&restored_path));
                        }
                        this.queue_vault_mutation(changes, cx);
                    }
                }
                window.push_notification(
                    match result {
                        Ok(_) => {
                            this.trash_undo.items.pop();
                            "Restored from Trash".into()
                        }
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
    #[cfg(target_os = "linux")]
    #[gpui::test]
    fn delete_and_undo_publish_inventory_search_and_referrers_without_prepare(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("New.md"), "uniquetrashword").unwrap();
        std::fs::write(root.join("Ref.md"), "[[New]]").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("New.md".into()),
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
        let generation = reader.read_with(visual, |v, _| {
            assert_eq!(v.vault.backlinks("New.md").len(), 1);
            v.loading.as_ref().unwrap().generation
        });
        reader.update_in(visual, |v, window, cx| {
            v.delete_path("New.md".into(), window, cx)
        });
        visual.run_until_parked();
        let bounds = visual
            .debug_bounds("confirm-trash")
            .expect("incoming link confirmation");
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert_eq!(v.loading.as_ref().unwrap().generation, generation);
            assert!(!v.vault.notes.iter().any(|note| note.path == "New.md"));
            assert!(v.vault.backlinks("New.md").is_empty());
            assert!(v
                .searcher
                .as_ref()
                .unwrap()
                .search("uniquetrashword", 10)
                .unwrap()
                .is_empty());
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let undo = visual
            .debug_bounds("undo-trash")
            .expect("actual Trash Undo action");
        visual.simulate_click(undo.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert_eq!(v.loading.as_ref().unwrap().generation, generation);
            assert!(v.vault.notes.iter().any(|note| note.path == "New.md"));
            assert_eq!(v.vault.backlinks("New.md").len(), 1);
            assert_eq!(
                v.searcher
                    .as_ref()
                    .unwrap()
                    .search("uniquetrashword", 10)
                    .unwrap()
                    .len(),
                1
            );
        });
        assert_eq!(
            std::fs::read_to_string(root.join("New.md")).unwrap(),
            "uniquetrashword"
        );
    }

    #[gpui::test]
    fn trash_toast_expires_at_bottom_and_keyboard_undo_survives_dismissal(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("start.md"), "Keep open").unwrap();
        std::fs::write(root.join("gone.md"), "Exact bytes\r\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
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
        for dismissal in ["timer", "escape", "close"] {
            let trashed = reader_trash_fs::Trashed::test_move(
                &root,
                Path::new("gone.md"),
                &temp.path().join("Trash"),
            );
            reader.update_in(visual, |r, window, cx| {
                r.trash_undo.items.push(trashed);
                r.show_trash_toast(window, cx);
                r.reveal_in_tree("start.md", window, cx);
            });
            visual.run_until_parked();
            let bounds = visual
                .debug_bounds("undo-trash")
                .expect("visible Undo action");
            visual.update(|window, _| {
                assert!(bounds.center().y > window.viewport_size().height / 2.)
            });
            match dismissal {
                "timer" => {
                    visual
                        .executor()
                        .advance_clock(std::time::Duration::from_secs(7));
                    visual.run_until_parked();
                    reader.read_with(visual, |r, _| assert!(r.trash_undo.visible.get()));
                    visual
                        .executor()
                        .advance_clock(std::time::Duration::from_secs(1));
                    visual.run_until_parked();
                }
                "escape" => visual.simulate_keystrokes("escape"),
                _ => {
                    // The stock notification close button invokes this same dismiss method.
                    visual.update(|window, cx| {
                        for note in window.notifications(cx).iter() {
                            note.update(cx, |n, cx| n.dismiss(window, cx));
                        }
                    });
                    visual.run_until_parked();
                    // Notification removal completes after its exit transition.
                    visual
                        .executor()
                        .advance_clock(std::time::Duration::from_secs(1));
                    visual.run_until_parked();
                }
            }
            reader.read_with(visual, |r, _| {
                assert!(!r.trash_undo.visible.get(), "{dismissal}");
                assert_eq!(r.trash_undo.items.len(), 1);
            });
            #[cfg(target_os = "macos")]
            visual.simulate_keystrokes("cmd-z");
            #[cfg(not(target_os = "macos"))]
            visual.simulate_keystrokes("ctrl-z");
            visual.run_until_parked();
            assert_eq!(
                std::fs::read_to_string(root.join("gone.md")).unwrap(),
                "Exact bytes\r\n"
            );
            reader.read_with(visual, |r, _| assert!(r.trash_undo.items.is_empty()));
        }
    }

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
            reader.toggle_source(window, cx);
            assert!(reader.editing.is_some());
            reader.delete_path("target.md".into(), window, cx)
        });
        visual.run_until_parked();
        let cancel = visual
            .debug_bounds("cancel-trash")
            .expect("incoming links require confirmation");
        assert!(root.join("target.md").exists());
        visual.simulate_click(cancel.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(!reader.trash_pending);
            assert!(reader.editing.is_some());
        });
        assert_eq!(
            std::fs::read_to_string(root.join("target.md")).unwrap(),
            "# Target\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("source.md")).unwrap(),
            "[[target]]\n"
        );
        std::fs::create_dir(root.join("Folder")).unwrap();
        std::fs::write(root.join("Folder/first.md"), "first").unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.delete_path("Folder".into(), window, cx)
        });
        visual.run_until_parked();
        let confirm = visual
            .debug_bounds("confirm-trash")
            .expect("folder confirmation");
        // A new writer arrived after the displayed count and lock set were built.
        std::fs::write(root.join("Folder/new.md"), "new arrival").unwrap();
        visual.simulate_click(confirm.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(!reader.trash_pending);
            assert!(reader.editing.is_some());
        });
        assert_eq!(
            std::fs::read_to_string(root.join("Folder/first.md")).unwrap(),
            "first"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("Folder/new.md")).unwrap(),
            "new arrival"
        );
    }
}
