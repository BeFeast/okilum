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

/// `Trashed.root` is canonical; the open vault root normally is too, but a
/// non-canonical spelling of the same folder must still match.
fn same_vault(trashed_root: &Path, vault_root: &Path) -> bool {
    trashed_root == vault_root
        || vault_root
            .canonicalize()
            .is_ok_and(|root| root == trashed_root)
}

/// Undo never restores into a vault other than the open one (#561). The item
/// stays in system Trash and the user is told how to get it back.
fn foreign_vault_notice(trashed: &reader_trash_fs::Trashed) -> String {
    let vault = trashed
        .root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| trashed.root.display().to_string());
    let trash = if cfg!(target_os = "macos") {
        "the Trash in Finder"
    } else {
        "the Trash in your file manager"
    };
    format!(
        "Nothing to Undo in this vault. \u{201c}{}\u{201d} from vault \u{201c}{vault}\u{201d} \
         remains in system Trash. Reopen that vault to Undo, or restore it from {trash}.",
        trashed.relative.display()
    )
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
        self.trash_undo.visible.set(false);
        self.trash_undo.generation = self.trash_undo.generation.wrapping_add(1);
        window.push_notification(
            Notification::new()
                .id::<TrashToast>()
                .message("Preparing to move to Trash…")
                .placement(Anchor::BottomRight)
                .py_2()
                .autohide(false),
            cx,
        );
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
                    window.push_notification(Notification::new().id::<TrashToast>()
                        .message(format!("Cannot move to Trash: {error:#}"))
                        .placement(Anchor::BottomRight).py_2().autohide(false), cx); cx.notify();
                }); return; }
            };
            if directory || incoming > 0 {
                let (send, receive) = async_channel::bounded(1);
                let shown = this.update_in(cx, |this, window, cx| {
                    window.remove_notification::<TrashToast>(cx);
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
            let held_editor = match this.update_in(cx, |this, window, cx| {
                if this.vault_root != root || !this.save_source(cx) {
                    window.remove_notification::<TrashToast>(cx);
                    this.trash_pending = false; cx.notify(); return None;
                }
                window.push_notification(Notification::new().id::<TrashToast>()
                    .message("Moving to Trash…").placement(Anchor::BottomRight)
                    .py_2().autohide(false), cx);
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
                        window.push_notification(Notification::new().id::<TrashToast>()
                            .message(format!("Cannot move to Trash: {error:#}"))
                            .placement(Anchor::BottomRight).py_2().autohide(false), cx);
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    fn show_trash_toast(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.trash_undo.visible.set(false);
        self.trash_undo.generation = self.trash_undo.generation.wrapping_add(1);
        let generation = self.trash_undo.generation;
        let visible = std::rc::Rc::new(std::cell::Cell::new(true));
        self.trash_undo.visible = visible.clone();
        let reader = cx.entity().downgrade();
        let announced = self.trash_undo.items.last().map(|t| t.root.clone());
        window.push_notification(
            Notification::new()
                .id::<TrashToast>()
                .message("Moved to Trash")
                .autohide(false)
                .py_2()
                .placement(Anchor::BottomRight)
                .on_close(move |_, _| visible.set(false))
                .action(move |_, _, cx| {
                    let reader = reader.clone();
                    let announced = announced.clone();
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
                        let _ = reader.update(cx, |this, cx| {
                            // The toast announced one vault's item; after a
                            // vault switch it must not undo another vault's.
                            match announced.as_deref() {
                                Some(root) if !same_vault(root, &this.vault_root) => {
                                    this.explain_foreign_trash(window, cx)
                                }
                                _ => this.undo_last_trash(window, cx),
                            }
                        });
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

    /// Undo history is scoped to the open vault: items trashed from another
    /// vault stay in system Trash until that vault is open again.
    pub(super) fn undo_last_trash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.trash_undo.items.is_empty() || self.trash_pending {
            return;
        }
        let Some(index) = self
            .trash_undo
            .items
            .iter()
            .rposition(|t| same_vault(&t.root, &self.vault_root))
        else {
            self.explain_foreign_trash(window, cx);
            return;
        };
        let trashed = self.trash_undo.items[index].clone();
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
                match result {
                    Ok(_) => {
                        // `trash_pending` blocked every history change meanwhile.
                        this.trash_undo.items.remove(index);
                        reader_toast::transient("Restored from Trash", window, cx);
                    }
                    Err(error) => {
                        reader_toast::error(format!("Cannot Undo: {error:#}"), window, cx)
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn explain_foreign_trash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(trashed) = self.trash_undo.items.last() else {
            return;
        };
        let message = foreign_vault_notice(trashed);
        self.dismiss_trash_toast(window, cx);
        reader_toast::error(message, window, cx);
        cx.notify();
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
                window.push_notification(
                    Notification::new()
                        .id::<TrashToast>()
                        .message("Preparing to move to Trash…")
                        .autohide(false),
                    cx,
                );
                let with_progress = window.notifications(cx).len();
                r.trash_undo.items.push(trashed);
                r.show_trash_toast(window, cx);
                assert_eq!(
                    window.notifications(cx).len(),
                    with_progress,
                    "result replaces progress without touching unrelated toasts"
                );
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

    #[test]
    fn foreign_vault_notice_names_the_item_vault_and_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Vault A");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("gone.md"), "x").unwrap();
        let trashed = reader_trash_fs::Trashed::test_move(
            &root,
            Path::new("gone.md"),
            &temp.path().join("Trash"),
        );
        let notice = foreign_vault_notice(&trashed);
        for part in [
            "gone.md",
            "Vault A",
            "remains in system Trash",
            "Reopen that vault",
        ] {
            assert!(notice.contains(part), "{part}: {notice}");
        }
    }

    #[gpui::test]
    fn undo_after_vault_switch_keeps_the_other_vault_item_in_trash(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let trash = temp.path().join("Trash");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for root in [&a, &b] {
            std::fs::create_dir(root).unwrap();
            std::fs::write(root.join("start.md"), "# Start").unwrap();
        }
        let a = a.canonicalize().unwrap();
        let b = b.canonicalize().unwrap();
        std::fs::write(a.join("gone.md"), "A bytes\r\n").unwrap();
        std::fs::write(b.join("own.md"), "B bytes").unwrap();
        let opts_for = |root: &Path| Opts {
            vault: Some(root.to_owned()),
            open_path: Some(root.join("start.md")),
            cache_base_override: Some(temp.path().join("os-cache")),
            session_directory: Some(temp.path().join("state")),
            ..Default::default()
        };
        let in_trash = |name: &str| -> Vec<String> {
            std::fs::read_dir(trash.join("files"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.to_string_lossy().ends_with(name))
                .map(|p| std::fs::read_to_string(p).unwrap())
                .collect()
        };
        let notices = |visual: &mut VisualTestContext| {
            visual.update(|window, cx| window.notifications(cx).len())
        };
        let undo = |visual: &mut VisualTestContext, keyboard: bool| {
            if keyboard {
                #[cfg(target_os = "macos")]
                visual.simulate_keystrokes("cmd-z");
                #[cfg(not(target_os = "macos"))]
                visual.simulate_keystrokes("ctrl-z");
            } else {
                visual.update(|window, cx| window.draw(cx).clear(cx));
                let button = visual
                    .debug_bounds("undo-trash")
                    .expect("visible Undo action");
                visual.simulate_click(button.center(), Modifiers::default());
            }
            visual.run_until_parked();
        };
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Reader::new(opts_for(&a), window, cx));
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let trashed = reader_trash_fs::Trashed::test_move(&a, Path::new("gone.md"), &trash);
        reader.update_in(visual, |r, window, cx| {
            r.trash_undo.items.push(trashed);
            r.show_trash_toast(window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.start_loading(opts_for(&b), window, cx)
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_eq!(r.vault_root, b, "positive control: the switch published B");
            r.reveal_in_tree("start.md", window, cx);
        });
        visual.run_until_parked();
        // Both the still-visible toast button and the keyboard refuse to cross vaults.
        for keyboard in [false, true] {
            let before = notices(visual);
            undo(visual, keyboard);
            assert!(!a.join("gone.md").exists(), "keyboard={keyboard}");
            assert!(!b.join("gone.md").exists(), "keyboard={keyboard}");
            assert_eq!(in_trash("gone.md"), ["A bytes\r\n"], "keyboard={keyboard}");
            assert!(notices(visual) > before, "keyboard={keyboard}: explained");
            reader.read_with(visual, |r, _| {
                assert!(!r.trash_pending);
                assert!(!r.trash_undo.visible.get());
                assert_eq!(r.trash_undo.items.len(), 1);
            });
        }
        // Positive control: B's own item is undone by the same button in B.
        let own = reader_trash_fs::Trashed::test_move(&b, Path::new("own.md"), &trash);
        reader.update_in(visual, |r, window, cx| {
            r.trash_undo.items.push(own);
            r.show_trash_toast(window, cx);
        });
        visual.run_until_parked();
        undo(visual, false);
        assert_eq!(
            std::fs::read_to_string(b.join("own.md")).unwrap(),
            "B bytes"
        );
        assert!(!a.join("gone.md").exists());
        reader.read_with(visual, |r, _| assert_eq!(r.trash_undo.items.len(), 1));
        // Back in A, keyboard Undo restores the scoped item into A.
        reader.update_in(visual, |r, window, cx| {
            r.start_loading(opts_for(&a), window, cx)
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_eq!(r.vault_root, a);
            r.reveal_in_tree("start.md", window, cx);
        });
        visual.run_until_parked();
        undo(visual, true);
        assert_eq!(
            std::fs::read_to_string(a.join("gone.md")).unwrap(),
            "A bytes\r\n"
        );
        assert!(!b.join("gone.md").exists());
        assert!(in_trash("gone.md").is_empty());
        reader.read_with(visual, |r, _| assert!(r.trash_undo.items.is_empty()));
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
