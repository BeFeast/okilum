//! An open note deleted outside Okilum (#1986).
//!
//! A file manager, `git checkout`, sync or a script can delete the note on
//! screen. Its contents are then only in this window: the Reader's rendered
//! source, or the editor's buffer. Okilum says so without a modal: a
//! persistent notice over the note offers Restore (write it back at its own
//! place) and Save as…, and the header strikes the title through. Leaving it
//! asks first in Read, Source and Live Preview. Recovery drafts remain a
//! fallback, not a substitute for the choice to save, discard or stay.
//!
//! When the file returns at the same place (a branch switch back), the notice
//! goes away. If it returns with different contents, the window keeps showing
//! what it had and offers to reload the version on disk; it never silently
//! replaces the only copy of the old contents. In the editor the existing
//! conflict view already offers Reload / Keep my version for that case.
#[cfg(test)]
mod close_tests;
mod navigation;

use super::*;
use gpui_component::{notification::Notification, WindowExt};

struct DeletedNotice;

/// What is on disk at the note's own place now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Disk {
    /// Nothing: the contents are only in this window.
    Missing,
    /// A file is back, with other contents than the window shows (Read).
    Different,
}

pub(super) struct DeletedNote {
    root: PathBuf,
    rel: String,
    /// The source the Reader showed when the file went away. While editing,
    /// the editor's buffer is used instead: it may have changed since.
    contents: String,
    pub(super) disk: Disk,
}

/// The answer to «leave a deleted note?».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Leave {
    SaveAs,
    DontSave,
    Cancel,
}

pub(super) type Continue = Box<dyn FnOnce(&mut Reader, &mut Window, &mut Context<Reader>)>;

/// The question asked before a deleted note's contents are left behind.
/// Escape, the close button and Cancel answer Cancel; Enter answers nothing.
fn ask_leave(
    window: &mut Window,
    cx: &mut App,
    title: String,
    disk: Disk,
) -> async_channel::Receiver<Leave> {
    let text = match disk {
        Disk::Missing => format!(
            "«{title}» was deleted from disk. Its contents are only in Okilum now and will be lost if you leave without saving."
        ),
        Disk::Different => format!(
            "«{title}» came back on disk with different contents. The version on screen is only in Okilum now and will be lost if you leave without saving."
        ),
    };
    let (send, receive) = async_channel::bounded(1);
    window.open_dialog(cx, move |dialog, _, _| {
        let cancel = send.clone();
        let close = send.clone();
        let footer_cancel = send.clone();
        let dont_save = send.clone();
        let save_as = send.clone();
        dialog
            .title("Save this note’s contents?")
            .width(px(480.))
            .child(v_flex().gap_2().child(div().text_sm().child(text.clone())))
            .on_ok(|_, _, _| false)
            .on_cancel(move |_, _, _| {
                let _ = cancel.try_send(Leave::Cancel);
                true
            })
            .on_close(move |_, _, _| {
                let _ = close.try_send(Leave::Cancel);
            })
            .footer(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("deleted-note-cancel")
                            .debug_selector(|| "deleted-note-cancel".into())
                            .ghost()
                            .label("Cancel")
                            .on_click(move |_, window, cx| {
                                let _ = footer_cancel.try_send(Leave::Cancel);
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("deleted-note-dont-save")
                            .debug_selector(|| "deleted-note-dont-save".into())
                            .ghost()
                            .label("Don’t Save")
                            .on_click(move |_, window, cx| {
                                let _ = dont_save.try_send(Leave::DontSave);
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("deleted-note-save-as")
                            .debug_selector(|| "deleted-note-save-as".into())
                            .primary()
                            .label("Save As…")
                            .on_click(move |_, window, cx| {
                                let _ = save_as.try_send(Leave::SaveAs);
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
    receive
}

fn save_icon() -> Icon {
    Icon::default().path(SharedString::from(okilum_shell::lucide::asset_path("save")))
}

/// Reuse the guarded, native create-only writer: complete synced bytes, real
/// directories and no destination replacement, including a raced symlink.
fn save_new_copy(path: &Path, contents: &str) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Missing destination folder"))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Missing destination filename"))?;
    okilum_core::note_files::create_attachment_with_source(
        parent,
        Path::new(name),
        contents.as_bytes(),
    )
}

impl Reader {
    /// The deleted-note state for the note on screen, if any.
    pub(super) fn deleted_current(&self) -> Option<&DeletedNote> {
        self.deleted_note.as_ref().filter(|deleted| {
            deleted.root == self.vault_root
                && deleted.rel == self.current_rel
                && self.file_preview.is_none()
        })
    }

    /// The contents that exist only in this window: the editor's buffer, or
    /// the source the Reader showed.
    fn deleted_contents(&self, cx: &App) -> Option<String> {
        let deleted = self.deleted_current()?;
        Some(
            self.source_text(cx)
                .unwrap_or_else(|| deleted.contents.clone()),
        )
    }

    /// Inventory reconciliation of the note on screen. Looks at the note's own
    /// place and records whether it went away or came back. Returns true when
    /// the displayed document must be kept as it is: its file is gone, or came
    /// back different and the person has not chosen to reload it.
    pub(super) fn track_deleted_note(
        &mut self,
        sources: &std::collections::HashMap<String, String>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.deleted_note.is_some() && self.deleted_current().is_none() {
            self.deleted_note = None;
        }
        if self.current_rel.is_empty() || self.file_preview.is_some() || !self.document_ready() {
            return false;
        }
        let path = self.vault_root.join(&self.current_rel);
        let on_disk = match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            // An unreadable or redirected return cannot authorize replacing
            // the retained buffer or dropping its navigation guard.
            Err(_) => return self.deleted_current().is_some(),
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return self.deleted_current().is_some();
            }
            Ok(_) => match self.deleted_current() {
                // A worker snapshot can predate the deletion/return. It cannot
                // prove that the retained contents are back on disk now.
                Some(_) => match std::fs::read_to_string(&path) {
                    Ok(text) => Some(text),
                    Err(_) => return true,
                },
                None => match sources.get(&self.current_rel) {
                    Some(raw) => Some(raw.clone()),
                    None => return false,
                },
            },
        };
        self.track_deleted_observation(on_disk, cx)
    }

    /// A worker's checked observation; publication never reads source on the UI.
    pub(super) fn track_deleted_observation(
        &mut self,
        on_disk: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let editing = self.editing.is_some();
        let retained = self.source_text(cx);
        let mut returned = false;
        let held = match (on_disk, self.deleted_note.as_mut()) {
            (None, Some(deleted)) => {
                deleted.disk = Disk::Missing;
                true
            }
            (None, None) => {
                let contents = self.source_text(cx).unwrap_or_else(|| {
                    self.note_canonical_source
                        .as_deref()
                        .map(str::to_owned)
                        .unwrap_or_else(|| self.note_source.clone())
                });
                self.deleted_note = Some(DeletedNote {
                    root: self.vault_root.clone(),
                    rel: self.current_rel.clone(),
                    contents,
                    disk: Disk::Missing,
                });
                true
            }
            (Some(text), Some(deleted)) => {
                if text == retained.as_deref().unwrap_or(&deleted.contents) {
                    self.deleted_note = None;
                    returned = true;
                    false
                } else {
                    deleted.disk = Disk::Different;
                    true
                }
            }
            (Some(_), None) => false,
        };
        if returned && editing {
            self.source_file_returned(cx);
        }
        if held && editing {
            self.clear_moved_or_deleted_notice();
        }
        if held {
            // A previously prepared refresh must not replace the retained copy.
            self.navigation.reconciliation_generation =
                self.navigation.reconciliation_generation.wrapping_add(1);
        }
        cx.notify();
        held
    }

    /// Recheck a retained read-only note's return on a worker. Even this less
    /// common watcher route must not hydrate/read the source on the UI thread.
    pub(super) fn reconcile_deleted_source_in_background(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.vault_root.clone();
        let rel = self.current_rel.clone();
        self.navigation.reconciliation_generation =
            self.navigation.reconciliation_generation.wrapping_add(1);
        let generation = self.navigation.reconciliation_generation;
        let vault = self.vault.clone();
        let selected = rel.clone();
        let task = cx.background_executor().spawn(async move {
            let read = || -> anyhow::Result<String> {
                anyhow::ensure!(
                    vault.root.canonicalize()? == vault.root,
                    "The retained vault was redirected"
                );
                let metadata = std::fs::symlink_metadata(vault.root.join(&selected))?;
                let path = reader_files::checked_path(&vault.root, &selected)?;
                anyhow::ensure!(
                    metadata.is_file() && !metadata.file_type().is_symlink(),
                    "Returned note is not a regular file"
                );
                Ok(okilum_core::vault::read_display_source(&path)?.text)
            };
            read()
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                if this.vault_root != root
                    || this.current_rel != rel
                    || this.editing.is_some()
                    || this.navigation.reconciliation_generation != generation
                {
                    return;
                }
                let source = match result {
                    Ok(raw) => Some(raw),
                    Err(error)
                        if error.chain().any(|cause| {
                            cause
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                        }) =>
                    {
                        None
                    }
                    Err(error) => {
                        eprintln!("Could not check returned note: {error:#}");
                        return;
                    }
                };
                if !this.track_deleted_observation(source.clone(), cx) {
                    this.reconcile_document_source(source.map(Arc::from), window, cx);
                }
            });
        })
        .detach();
    }

    /// The persistent notice. Overlays the document like every Reader notice,
    /// so it never takes a layout row.
    pub(super) fn sync_deleted_notice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self
            .deleted_current()
            .map(|deleted| (deleted.rel.clone(), deleted.disk));
        if shown.is_some() && self.editing.is_some() {
            // The editor's own «moved or deleted» notice says less than this one.
            self.clear_moved_or_deleted_notice();
        }
        if shown == self.displayed_deleted_notice {
            return;
        }
        self.displayed_deleted_notice = shown.clone();
        self.deleted_notice_generation = self.deleted_notice_generation.wrapping_add(1);
        let generation = self.deleted_notice_generation;
        let title = self.selected_title();
        let reader = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            let _ = reader.update(cx, |this, cx| {
                if this.deleted_notice_generation != generation {
                    return;
                }
                window.remove_notification::<DeletedNotice>(cx);
                let Some((_, disk)) = shown else {
                    return;
                };
                let message = match disk {
                    Disk::Missing => format!(
                        "«{title}» was deleted from disk. Its contents are only in Okilum for now."
                    ),
                    Disk::Different => format!(
                        "«{title}» is back on disk with different contents. This window still shows the earlier version."
                    ),
                };
                let actions = cx.weak_entity();
                window.push_notification(
                    Notification::new()
                        .id::<DeletedNotice>()
                        .message(message)
                        .content(move |_, _, cx| {
                            let restore = actions.clone();
                            let save = actions.clone();
                            h_flex()
                                .debug_selector(|| "deleted-note-notice".into())
                                .gap_1()
                                .map(|row| match disk {
                                    Disk::Missing => row.child(
                                        reader_icon_button(
                                            "deleted-note-restore",
                                            IconName::Undo2,
                                            "Restore",
                                            cx,
                                        )
                                        .debug_selector(|| "deleted-note-restore".into())
                                        .on_click(move |_, window, cx| {
                                            let _ = restore.update(cx, |this, cx| {
                                                this.restore_deleted_note(window, cx)
                                            });
                                        }),
                                    ),
                                    Disk::Different => row.child(
                                        reader_icon_button(
                                            "deleted-note-reload",
                                            IconName::RotateCw,
                                            "Reload from disk",
                                            cx,
                                        )
                                        .debug_selector(|| "deleted-note-reload".into())
                                        .on_click(move |_, window, cx| {
                                            let _ = restore.update(cx, |this, cx| {
                                                this.reload_returned_note(window, cx)
                                            });
                                        }),
                                    ),
                                })
                                .child(
                                    reader_icon_button(
                                        "deleted-note-save-as",
                                        save_icon(),
                                        "Save As…",
                                        cx,
                                    )
                                    .debug_selector(|| "deleted-note-notice-save-as".into())
                                    .on_click(move |_, window, cx| {
                                        let _ = save.update(cx, |this, cx| {
                                            this.save_deleted_note_as(None, window, cx)
                                        });
                                    }),
                                )
                                .into_any_element()
                        })
                        .placement(Anchor::TopRight)
                        .py_2()
                        .autohide(false),
                    cx,
                );
            });
        });
    }

    /// Show the notice again after it was closed (the header's «Deleted»).
    pub(super) fn reshow_deleted_notice(&mut self, cx: &mut Context<Self>) {
        self.displayed_deleted_notice = None;
        cx.notify();
    }

    /// Write the contents back at the note's own place. Never replaces a file
    /// that appeared there meanwhile.
    pub(super) fn restore_deleted_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(contents) = self.deleted_contents(cx) else {
            return;
        };
        if self.deleted_current().map(|deleted| deleted.disk) != Some(Disk::Missing) {
            return;
        }
        let rel = self.current_rel.clone();
        let path = self.vault_root.join(&rel);
        let result = (|| -> anyhow::Result<()> {
            if let Some(parent) = Path::new(&rel)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
            {
                okilum_core::note_files::create_folders(&self.vault_root, parent, false)?;
            }
            okilum_core::note_files::create_attachment_with_source(
                &self.vault_root,
                Path::new(&rel),
                contents.as_bytes(),
            )
        })();
        match result {
            Ok(()) => {
                if self.editing.is_some() && !self.source_restored(window, cx) {
                    // A recreated folder or a racing writer must not cause us
                    // to drop the retained editor and open unrelated bytes.
                    self.track_deleted_note(&Default::default(), cx);
                    reader_toast::error("The copy was restored, but the editor could not reconnect. Your buffer is still open; use Save As… to keep another copy.", window, cx);
                    return;
                }
                self.deleted_note = None;
                reader_toast::transient("Note restored", window, cx);
            }
            Err(_) if std::fs::symlink_metadata(&path).is_ok() => {
                reader_toast::error(
                    "A file is already back at this place. Nothing was overwritten.",
                    window,
                    cx,
                );
            }
            Err(error) => {
                eprintln!("Could not restore the deleted note: {error:#}");
                reader_toast::error(
                    "The note couldn’t be restored. Use Save As… to keep its contents elsewhere.",
                    window,
                    cx,
                );
            }
        }
        #[cfg(any(unix, windows))]
        self.queue_vault_mutation(
            okilum_core::Changes {
                changed: std::collections::BTreeSet::from([rel]),
                ..Default::default()
            },
            cx,
        );
        cx.notify();
    }

    /// The file came back different: show the version on disk.
    pub(super) fn reload_returned_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.deleted_current().map(|deleted| deleted.disk) != Some(Disk::Different) {
            return;
        }
        if self.editing.is_some() {
            self.resolve_source(true, window, cx);
            return;
        }
        let path = self.vault_root.join(&self.current_rel);
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                self.deleted_note = None;
                self.reconcile_document_source(Some(raw.into()), window, cx);
            }
            Err(error) => {
                eprintln!("Could not reload the returned note: {error:#}");
                reader_toast::error("The note on disk couldn’t be read.", window, cx);
            }
        }
        cx.notify();
    }

    /// Save the contents to a chosen place. With `then`, a navigation the
    /// person asked for continues once the contents are saved; without it, a
    /// copy saved inside the vault opens in place of the deleted note.
    pub(super) fn save_deleted_note_as(
        &mut self,
        then: Option<Continue>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(contents) = self.deleted_contents(cx) else {
            if let Some(then) = then {
                then(self, window, cx);
            }
            return;
        };
        let root = self.vault_root.clone();
        let rel = self.current_rel.clone();
        let original = root.join(&rel);
        let directory = original
            .parent()
            .filter(|parent| parent.is_dir())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.clone());
        let name = original
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Note.md".into());
        let generation = self.navigation.preparation_generation;
        let picker = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut path))) = picker.await else {
                return;
            };
            if path.extension().is_none() {
                path.set_extension("md");
            }
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                // A picker can outlive the document or edits made behind it.
                // Never mark newer contents saved or replay stale navigation.
                if this.vault_root != root
                    || this.current_rel != rel
                    || this.navigation.preparation_generation != generation
                {
                    return;
                }
                if this.deleted_contents(cx).as_deref() != Some(contents.as_str()) {
                    reader_toast::error(
                        "The note changed while Save As was open. Nothing was saved. Use Save As again to keep the latest contents.",
                        window,
                        cx,
                    );
                    return;
                }
                let written = save_new_copy(&path, &contents);
                if let Err(error) = written {
                    eprintln!("Could not save the deleted note: {error:#}");
                    reader_toast::error(
                        "The note couldn’t be saved there. Choose a new filename; existing files are never overwritten.",
                        window,
                        cx,
                    );
                    return;
                }
                let same = this.vault_root == root && this.current_rel == rel;
                if same {
                    this.deleted_note = None;
                    // Its contents are saved: the editor of the deleted file
                    // has nothing left to protect.
                    this.source_saved_elsewhere(cx);
                    this.remember_source_mode();
                    this.editing = None;
                }
                let inside = root.canonicalize().ok().and_then(|root| {
                    let parent = path.parent()?.canonicalize().ok()?;
                    let full = parent.join(path.file_name()?);
                    full.strip_prefix(&root)
                        .ok()
                        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                });
                if let Some(saved) = inside.as_ref().filter(|_| this.vault_root == root) {
                    #[cfg(any(unix, windows))]
                    this.queue_vault_mutation(
                        okilum_core::Changes {
                            changed: std::collections::BTreeSet::from([saved.clone()]),
                            ..Default::default()
                        },
                        cx,
                    );
                }
                let label = path
                    .file_stem()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                reader_toast::transient(format!("Saved as «{label}»"), window, cx);
                match then {
                    Some(then) => this.continue_deleted_navigation(then, window, cx),
                    None => {
                        if let Some(saved) = inside.filter(|_| same && this.vault_root == root) {
                            this.continue_deleted_navigation(Box::new(move |this, window, cx| {
                                this.open_note(&saved, None, window, cx);
                            }), window, cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::Disk;
    use ::core::prelude::v1::test;
    use gpui_component::WindowExt as _;
    use std::path::Path;

    pub(super) const START: &str = "# Start\n\nonly copy of these words\n";

    /// Real time for the OS watcher, test time for the Reader's timers,
    /// until `done` holds or ~15 s are up.
    pub(super) fn settle(
        visual: &mut VisualTestContext,
        reader: &Entity<Reader>,
        what: &str,
        done: impl Fn(&Reader) -> bool,
    ) {
        for _ in 0..150 {
            std::thread::sleep(Duration::from_millis(100));
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
            if reader.read_with(visual, |reader, _| done(reader)) {
                return;
            }
        }
        panic!("not reached after 15 s: {what}");
    }

    fn open<'a>(
        cx: &'a mut TestAppContext,
        root: &Path,
        state: &Path,
    ) -> (Entity<Reader>, &'a mut VisualTestContext) {
        open_kind(cx, root, state, false)
    }

    fn open_kind<'a>(
        cx: &'a mut TestAppContext,
        root: &Path,
        state: &Path,
        folder: bool,
    ) -> (Entity<Reader>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: (!folder).then(|| root.to_path_buf()),
                        open_path: folder.then(|| root.join("Start.md")),
                        // #2066 makes a folder open a vault. Keep testing the
                        // explicit quick viewer's lazy folder/recovery path.
                        single_file: folder,
                        note: Some("Start.md".into()),
                        index_dir: (!folder).then(|| state.join("index")),
                        session_directory: Some(state.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        visual.run_until_parked();
        let reader = entity.unwrap();
        if folder {
            reader.update_in(visual, |r, window, cx| {
                r.open_note("Start.md", None, window, cx)
            });
            visual.run_until_parked();
        }
        reader.read_with(visual, |r, _| {
            assert!(r.watcher.is_some(), "positive control: a watcher");
            assert_eq!(r.single_file, folder);
            assert_eq!(r.current_rel, "Start.md", "positive control: opened");
            assert!(
                r.note_source.contains("only copy"),
                "positive control: shown"
            );
        });
        (reader, visual)
    }

    pub(super) fn vault() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Start.md"), START).unwrap();
        std::fs::write(root.join("Other.md"), "# Other\n\notherword\n").unwrap();
        let root = root.canonicalize().unwrap();
        (temp, root)
    }

    fn disk(reader: &Reader) -> Option<Disk> {
        reader.deleted_current().map(|deleted| deleted.disk)
    }

    fn draw(visual: &mut VisualTestContext) {
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
    }

    pub(super) fn click(visual: &mut VisualTestContext, selector: &'static str) {
        draw(visual);
        let bounds = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is on screen"));
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
    }

    #[gpui::test]
    fn deletion_is_shown_and_restore_writes_the_contents_back(cx: &mut TestAppContext) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());
        draw(visual);
        assert!(visual.debug_bounds("deleted-note-notice").is_none());
        assert!(visual.debug_bounds("reader-document-deleted").is_none());

        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "deletion seen", |r| {
            disk(r) == Some(Disk::Missing)
        });
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "Start.md");
            assert!(r.note_source.contains("only copy"), "still shown");
            assert!(
                !r.link_notice
                    .as_deref()
                    .is_some_and(|n| n.contains("no longer available")),
                "the deleted notice replaces the generic one: {:?}",
                r.link_notice
            );
        });
        draw(visual);
        assert!(visual.debug_bounds("deleted-note-notice").is_some());
        assert!(visual.debug_bounds("reader-document-deleted").is_some());

        click(visual, "deleted-note-restore");
        assert_eq!(
            std::fs::read_to_string(root.join("Start.md")).unwrap(),
            START,
            "restored byte for byte at its own place"
        );
        reader.read_with(visual, |r, _| assert!(r.deleted_current().is_none()));
        // The watcher sees the restored file as the same note; nothing returns.
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(100));
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
        }
        reader.read_with(visual, |r, _| {
            assert!(r.deleted_current().is_none());
            assert!(r.note_source.contains("only copy"));
        });
        draw(visual);
        assert!(visual.debug_bounds("deleted-note-notice").is_none());
        assert!(visual.debug_bounds("reader-document-deleted").is_none());
    }

    #[gpui::test]
    fn a_file_back_unchanged_clears_and_a_changed_one_offers_reload(cx: &mut TestAppContext) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());

        // Back with the same contents (a branch switch there and back).
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "deletion seen", |r| {
            disk(r) == Some(Disk::Missing)
        });
        std::fs::write(root.join("Start.md"), START).unwrap();
        settle(visual, &reader, "same file back", |r| {
            r.deleted_current().is_none()
        });

        // Back with other contents: kept on screen until Reload.
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "second deletion seen", |r| {
            disk(r) == Some(Disk::Missing)
        });
        std::fs::write(root.join("Start.md"), "# Start\n\nthe branch version\n").unwrap();
        settle(visual, &reader, "different file back", |r| {
            disk(r) == Some(Disk::Different)
        });
        reader.read_with(visual, |r, _| {
            assert!(r.note_source.contains("only copy"), "not replaced silently");
        });
        draw(visual);
        assert!(visual.debug_bounds("deleted-note-notice").is_some());
        assert!(
            visual.debug_bounds("reader-document-deleted").is_none(),
            "a file is there again: not marked deleted"
        );
        click(visual, "deleted-note-reload");
        settle(visual, &reader, "reloaded", |r| {
            r.note_source.contains("branch version") && r.deleted_current().is_none()
        });
    }

    #[gpui::test]
    fn leaving_a_deleted_note_asks_and_save_as_keeps_its_contents(cx: &mut TestAppContext) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "deletion seen", |r| {
            disk(r) == Some(Disk::Missing)
        });

        // Cancel stays on the deleted note.
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        assert!(
            visual.update(|window, cx| window.has_active_dialog(cx)),
            "asks"
        );
        click(visual, "deleted-note-cancel");
        assert!(!visual.update(|window, cx| window.has_active_dialog(cx)));
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "Start.md");
            assert!(r.note_source.contains("only copy"));
            assert_eq!(disk(r), Some(Disk::Missing));
        });

        // Save As… writes the contents, then the navigation goes on.
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        click(visual, "deleted-note-save-as");
        assert!(visual.did_prompt_for_new_path(), "asks where to save");
        let saved = root.join("Kept.md");
        visual.simulate_new_path_selection({
            let saved = saved.clone();
            move |_| Some(saved)
        });
        visual.run_until_parked();
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), START);
        settle(visual, &reader, "navigation continued", |r| {
            r.current_rel == "Other.md" && r.note_source.contains("otherword")
        });
        reader.read_with(visual, |r, _| assert!(r.deleted_note.is_none()));
    }

    #[gpui::test]
    fn dont_save_leaves_without_writing(cx: &mut TestAppContext) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());
        // Positive control: an ordinary note opens without a question.
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        settle(visual, &reader, "plain navigation", |r| {
            r.current_rel == "Other.md"
        });
        assert!(!visual.update(|window, cx| window.has_active_dialog(cx)));
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Start.md", None, window, cx)
        });
        settle(visual, &reader, "back", |r| {
            r.current_rel == "Start.md" && r.note_source.contains("only copy")
        });

        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "deletion seen", |r| {
            disk(r) == Some(Disk::Missing)
        });
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        click(visual, "deleted-note-dont-save");
        settle(visual, &reader, "left", |r| r.current_rel == "Other.md");
        assert!(!root.join("Start.md").exists(), "nothing written");
        reader.read_with(visual, |r, _| assert!(r.deleted_note.is_none()));
    }

    fn restore_editor_buffer(cx: &mut TestAppContext, live: bool) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        let edited = "# Start\n\nonly copy of these words, edited\n";
        reader.update_in(visual, |r, window, cx| {
            r.set_live_preview(live, window, cx);
            r.editing
                .as_ref()
                .expect("editing")
                .set_value(edited, window, cx);
        });
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "deletion seen while editing", |r| {
            disk(r) == Some(Disk::Missing)
        });
        reader.read_with(visual, |r, _| {
            assert!(r.editing.is_some(), "the editor stays open");
            assert!(
                !r.link_notice
                    .as_deref()
                    .is_some_and(|n| n == super::super::reader_editor::MOVED_OR_DELETED_NOTICE),
                "one notice, not two: {:?}",
                r.link_notice
            );
        });
        click(visual, "deleted-note-restore");
        assert_eq!(
            std::fs::read_to_string(root.join("Start.md")).unwrap(),
            edited
        );
        reader.read_with(visual, |r, _| {
            assert!(r.deleted_current().is_none());
            assert!(r.editing.is_some(), "still editing the restored note");
        });
        // The restored file is the editor's base: a later save just works.
        let more = "# Start\n\nonly copy of these words, edited twice\n";
        reader.update_in(visual, |r, window, cx| {
            r.editing.as_ref().unwrap().set_value(more, window, cx);
            assert!(r.save_source(cx), "{:?}", r.link_notice);
        });
        assert_eq!(
            std::fs::read_to_string(root.join("Start.md")).unwrap(),
            more
        );
    }

    #[gpui::test]
    fn the_editor_buffer_is_what_restore_writes(cx: &mut TestAppContext) {
        restore_editor_buffer(cx, false);
    }

    #[gpui::test]
    fn live_restore_preserves_current_buffer_and_later_save(cx: &mut TestAppContext) {
        restore_editor_buffer(cx, true);
    }

    fn editor_leave_confirmation(cx: &mut TestAppContext, live: bool, folder: bool) {
        let (temp, root) = vault();
        let (reader, visual) = open_kind(cx, &root, temp.path(), folder);
        let edited = "# Start\n\nlatest unsaved editor contents\n";
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.set_live_preview(live, window, cx);
            r.editing.as_ref().unwrap().set_value(edited, window, cx);
        });
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "editor deletion", |r| {
            disk(r) == Some(Disk::Missing)
        });
        reader.read_with(visual, |r, cx| {
            assert_eq!(r.source_live_preview(), live, "positive control: mode");
            assert_eq!(r.source_text(cx).as_deref(), Some(edited));
        });

        // Cancel leaves the editor and latest bytes intact.
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        assert!(visual.update(|window, cx| window.has_active_dialog(cx)));
        click(visual, "deleted-note-cancel");
        reader.read_with(visual, |r, cx| {
            assert_eq!(r.current_rel, "Start.md");
            assert_eq!(r.source_text(cx).as_deref(), Some(edited));
            assert!(r.editing.is_some());
        });

        // Cancelling the path picker is also Cancel, not permission to leave.
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        click(visual, "deleted-note-save-as");
        assert!(visual.did_prompt_for_new_path());
        visual.simulate_new_path_selection(|_| None);
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert_eq!(r.current_rel, "Start.md");
            assert_eq!(r.source_text(cx).as_deref(), Some(edited));
            assert_eq!(disk(r), Some(Disk::Missing));
        });

        // A failed write cannot authorize navigation or consume the draft.
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        click(visual, "deleted-note-save-as");
        let invalid = root.join("absent/Kept.md");
        visual.simulate_new_path_selection(move |_| Some(invalid));
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert_eq!(r.current_rel, "Start.md");
            assert_eq!(r.source_text(cx).as_deref(), Some(edited));
            assert!(r.deleted_current().is_some());
        });

        // A destination appearing while the picker is open is never clobbered.
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        click(visual, "deleted-note-save-as");
        let existing = root.join("Existing.md");
        std::fs::write(&existing, "external bytes").unwrap();
        visual.simulate_new_path_selection(move |_| Some(existing));
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(root.join("Existing.md")).unwrap(),
            "external bytes"
        );
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "Start.md"));

        // Successful Save As writes the editor buffer, then continues once.
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        click(visual, "deleted-note-save-as");
        let saved = root.join("Kept.md");
        visual.simulate_new_path_selection(move |_| Some(saved));
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(root.join("Kept.md")).unwrap(),
            edited
        );
        settle(visual, &reader, "saved editor navigation", |r| {
            r.current_rel == "Other.md"
        });
        assert!(!root.join("Start.md").exists());
    }

    #[gpui::test]
    fn source_mode_confirms_and_save_as_preserves_latest_buffer(cx: &mut TestAppContext) {
        editor_leave_confirmation(cx, false, false);
    }

    #[gpui::test]
    fn live_mode_confirms_and_save_as_preserves_latest_buffer(cx: &mut TestAppContext) {
        editor_leave_confirmation(cx, true, false);
    }

    fn editor_dont_save_keeps_recovery(cx: &mut TestAppContext, live: bool) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());
        let edited = "# Start\n\nrecover this unsaved text\n";
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.set_live_preview(live, window, cx);
            r.editing.as_ref().unwrap().set_value(edited, window, cx);
        });
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "deleted", |r| {
            disk(r) == Some(Disk::Missing)
        });
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        click(visual, "deleted-note-dont-save");
        settle(visual, &reader, "discard navigation", |r| {
            r.current_rel == "Other.md"
        });
        assert!(!root.join("Start.md").exists());
        assert!(okilum_core::file_editor::FileEditor::has_unsaved_draft(
            &root.join("Start.md"),
            &temp.path().join("state/editor-drafts")
        )
        .unwrap());
    }

    #[gpui::test]
    fn source_dont_save_keeps_recovery_without_recreating_deleted_source(cx: &mut TestAppContext) {
        editor_dont_save_keeps_recovery(cx, false);
    }

    #[gpui::test]
    fn live_dont_save_keeps_recovery_without_recreating_deleted_source(cx: &mut TestAppContext) {
        editor_dont_save_keeps_recovery(cx, true);
    }

    #[gpui::test]
    fn save_as_rejects_changed_buffer_and_retry_keeps_latest_contents(cx: &mut TestAppContext) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "deleted", |r| {
            disk(r) == Some(Disk::Missing)
        });
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        click(visual, "deleted-note-save-as");
        assert!(
            visual.did_prompt_for_new_path(),
            "positive control: picker opened"
        );
        let latest = "# Start\n\nchanged behind the path picker\n";
        reader.update_in(visual, |r, window, cx| {
            r.editing.as_ref().unwrap().set_value(latest, window, cx);
        });
        let stale = root.join("OldBuffer.md");
        visual.simulate_new_path_selection(move |_| Some(stale));
        visual.run_until_parked();
        assert!(
            !root.join("OldBuffer.md").exists(),
            "no stale copy published"
        );
        reader.read_with(visual, |r, cx| {
            assert_eq!(r.current_rel, "Start.md", "no pending navigation replay");
            assert_eq!(r.source_text(cx).as_deref(), Some(latest));
            assert_eq!(disk(r), Some(Disk::Missing));
        });
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        click(visual, "deleted-note-save-as");
        let latest_path = root.join("Latest.md");
        visual.simulate_new_path_selection(move |_| Some(latest_path));
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(root.join("Latest.md")).unwrap(),
            latest
        );
        settle(
            visual,
            &reader,
            "latest copy saved and navigation resumed",
            |r| r.current_rel == "Other.md",
        );
        assert!(!root.join("Start.md").exists());
    }

    #[gpui::test]
    fn old_dialog_and_picker_cannot_apply_after_same_path_is_reopened(cx: &mut TestAppContext) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "deleted", |r| {
            disk(r) == Some(Disk::Missing)
        });
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        assert!(visual.update(|window, cx| window.has_active_dialog(cx)));
        // Same path/root after a new document request is a different document.
        reader.update_in(visual, |r, _, _| {
            r.navigation.preparation_generation += 1;
        });
        click(visual, "deleted-note-dont-save");
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "Start.md");
            assert_eq!(disk(r), Some(Disk::Missing));
        });
        reader.update_in(visual, |r, window, cx| {
            r.save_deleted_note_as(None, window, cx)
        });
        visual.run_until_parked();
        assert!(visual.did_prompt_for_new_path());
        reader.update_in(visual, |r, _, _| {
            r.navigation.preparation_generation += 1;
        });
        let path = root.join("StaleGeneration.md");
        visual.simulate_new_path_selection(move |_| Some(path));
        visual.run_until_parked();
        assert!(!root.join("StaleGeneration.md").exists());
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "Start.md");
            assert_eq!(disk(r), Some(Disk::Missing));
        });
    }

    #[gpui::test]
    fn stale_dialog_and_picker_cannot_navigate_or_write_for_another_document(
        cx: &mut TestAppContext,
    ) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "deleted", |r| {
            disk(r) == Some(Disk::Missing)
        });
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, _, _| r.current_rel = "NewIdentity.md".into());
        click(visual, "deleted-note-dont-save");
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "NewIdentity.md"));

        reader.update_in(visual, |r, window, cx| {
            r.current_rel = "Start.md".into();
            r.save_deleted_note_as(None, window, cx);
        });
        visual.run_until_parked();
        assert!(visual.did_prompt_for_new_path());
        reader.update_in(visual, |r, _, _| r.current_rel = "NewIdentity.md".into());
        let saved = root.join("Stale.md");
        visual.simulate_new_path_selection(move |_| Some(saved));
        visual.run_until_parked();
        assert!(!root.join("Stale.md").exists());
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "NewIdentity.md"));
    }

    #[gpui::test]
    fn folder_source_confirms_and_save_as_preserves_dirty_contents(cx: &mut TestAppContext) {
        editor_leave_confirmation(cx, false, true);
    }

    #[gpui::test]
    fn folder_live_confirms_and_save_as_preserves_dirty_contents(cx: &mut TestAppContext) {
        editor_leave_confirmation(cx, true, true);
    }

    fn returned_editor(cx: &mut TestAppContext, folder: bool, live: bool, dirty: bool) {
        let (temp, root) = vault();
        let (reader, visual) = open_kind(cx, &root, temp.path(), folder);
        let retained = if dirty { "# Unsaved contents\n" } else { START };
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.set_live_preview(live, window, cx);
            if dirty {
                r.editing.as_ref().unwrap().set_value(retained, window, cx);
            }
        });
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "editor deletion", |r| {
            disk(r) == Some(Disk::Missing)
        });
        assert!(
            !visual.update(|window, cx| window.has_active_dialog(cx)),
            "deletion is nonmodal"
        );
        reader.read_with(visual, |r, cx| {
            assert_eq!(r.source_text(cx).as_deref(), Some(retained))
        });
        std::fs::write(root.join("Start.md"), "# Returned version\n").unwrap();
        settle(visual, &reader, "returned editor version", |r| {
            disk(r) == Some(Disk::Different)
        });
        // Exercise the shared-publication path as well as the direct refresh,
        // after the watcher has positively detected both filesystem changes.
        reader.update_in(visual, |r, window, cx| {
            // Stale inventory still containing the old bytes is not a proof
            // that the returned file matches the retained buffer.
            let stale = std::collections::HashMap::from([("Start.md".into(), retained.into())]);
            assert!(r.track_deleted_note(&stale, cx));
            assert_eq!(disk(r), Some(Disk::Different));
            r.refresh_source_from_disk(window, cx);
            r.reconcile_published_document(window, cx);
            assert_eq!(r.source_text(cx).as_deref(), Some(retained));
        });
        visual.run_until_parked();
        draw(visual);
        assert!(
            visual.debug_bounds("reader-document-deleted").is_none(),
            "returned file is no longer marked deleted"
        );
        assert!(visual.debug_bounds("deleted-note-reload").is_some());
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        click(visual, "deleted-note-cancel");
        reader.read_with(visual, |r, cx| {
            assert_eq!(r.source_text(cx).as_deref(), Some(retained))
        });
        click(visual, "deleted-note-reload");
        reader.read_with(visual, |r, cx| {
            assert_eq!(r.source_text(cx).as_deref(), Some("# Returned version\n"));
            assert!(r.deleted_current().is_none());
        });
        assert_eq!(
            std::fs::read_to_string(root.join("Start.md")).unwrap(),
            "# Returned version\n"
        );
    }

    #[gpui::test]
    fn clean_source_survives_deletion_and_different_return(cx: &mut TestAppContext) {
        returned_editor(cx, false, false, false);
    }

    #[gpui::test]
    fn dirty_live_survives_deletion_and_different_return(cx: &mut TestAppContext) {
        returned_editor(cx, false, true, true);
    }

    #[gpui::test]
    fn folder_clean_live_survives_deletion_and_different_return(cx: &mut TestAppContext) {
        returned_editor(cx, true, true, false);
    }

    #[gpui::test]
    fn folder_dirty_source_survives_deletion_and_different_return(cx: &mut TestAppContext) {
        returned_editor(cx, true, false, true);
    }

    #[gpui::test]
    fn navigation_before_watcher_and_close_both_ask(cx: &mut TestAppContext) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());
        std::fs::remove_file(root.join("Start.md")).unwrap();
        // No watcher tick: the navigation itself must discover the absence.
        reader.update_in(visual, |r, window, cx| {
            r.preview_file("asset.txt", window, cx)
        });
        visual.run_until_parked();
        assert!(visual.update(|window, cx| window.has_active_dialog(cx)));
        click(visual, "deleted-note-cancel");
        reader.update_in(visual, |r, window, cx| r.close_note(window, cx));
        visual.run_until_parked();
        click(visual, "deleted-note-cancel");
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "Start.md"));
        reader.update_in(visual, |r, window, cx| r.close_note(window, cx));
        visual.run_until_parked();
        click(visual, "deleted-note-dont-save");
        reader.read_with(visual, |r, _| assert!(r.current_rel.is_empty()));
    }

    #[test]
    fn copy_publication_never_overwrites_a_returned_file_or_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("copy.md");
        super::save_new_copy(&path, START).unwrap();
        assert!(super::save_new_copy(&path, "replacement").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), START);
        #[cfg(unix)]
        {
            let link = temp.path().join("link.md");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(super::save_new_copy(&link, "replacement").is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), START);
        }
    }

    #[gpui::test]
    fn folder_read_return_restore_and_attachment_navigation(cx: &mut TestAppContext) {
        let (temp, root) = vault();
        std::fs::write(root.join("asset.txt"), "attachment").unwrap();
        let (reader, visual) = open_kind(cx, &root, temp.path(), true);
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "folder deletion", |r| {
            disk(r) == Some(Disk::Missing)
        });
        std::fs::write(root.join("Start.md"), START).unwrap();
        settle(visual, &reader, "unchanged return", |r| disk(r).is_none());
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "second deletion", |r| {
            disk(r) == Some(Disk::Missing)
        });
        std::fs::write(root.join("Start.md"), "# Returned version\n").unwrap();
        settle(visual, &reader, "changed return", |r| {
            disk(r) == Some(Disk::Different)
        });
        reader.read_with(visual, |r, _| assert!(r.note_source.contains("only copy")));
        click(visual, "deleted-note-reload");
        settle(visual, &reader, "explicit reload", |r| {
            r.note_source.contains("Returned version") && disk(r).is_none()
        });
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "third deletion", |r| {
            disk(r) == Some(Disk::Missing)
        });
        click(visual, "deleted-note-restore");
        assert_eq!(
            std::fs::read_to_string(root.join("Start.md")).unwrap(),
            "# Returned version\n"
        );
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "fourth deletion", |r| {
            disk(r) == Some(Disk::Missing)
        });
        reader.update_in(visual, |r, window, cx| {
            r.preview_file("asset.txt", window, cx)
        });
        visual.run_until_parked();
        click(visual, "deleted-note-cancel");
        reader.read_with(visual, |r, _| assert!(r.file_preview.is_none()));
        reader.update_in(visual, |r, window, cx| {
            r.preview_file("asset.txt", window, cx)
        });
        visual.run_until_parked();
        click(visual, "deleted-note-dont-save");
        reader.read_with(visual, |r, _| {
            assert_eq!(r.file_preview.as_ref().unwrap().rel, "asset.txt")
        });
        assert!(!root.join("Start.md").exists());
    }

    #[gpui::test]
    fn window_mode_switch_keeps_deleted_editor_on_cancel_and_failed_recovery(
        cx: &mut TestAppContext,
    ) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());
        let latest = "# Latest unsaved mode-switch draft\n";
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.editing.as_ref().unwrap().set_value(latest, window, cx);
        });
        std::fs::remove_file(root.join("Start.md")).unwrap();
        // Do not deliver the watcher: the transition itself must detect deletion.
        reader.update_in(visual, |r, window, cx| r.switch_window_mode(window, cx));
        visual.run_until_parked();
        assert!(visual.update(|window, cx| window.has_active_dialog(cx)));
        click(visual, "deleted-note-cancel");
        reader.read_with(visual, |r, cx| {
            assert!(!r.single_file);
            assert_eq!(r.source_text(cx).as_deref(), Some(latest));
        });
        let journal = temp.path().join("state/editor-drafts");
        std::fs::rename(&journal, temp.path().join("saved-drafts")).unwrap();
        std::fs::write(&journal, "blocked").unwrap();
        reader.update_in(visual, |r, window, cx| r.switch_window_mode(window, cx));
        visual.run_until_parked();
        click(visual, "deleted-note-dont-save");
        reader.read_with(visual, |r, cx| {
            assert!(!r.single_file);
            assert_eq!(r.current_rel, "Start.md");
            assert_eq!(r.source_text(cx).as_deref(), Some(latest));
            assert!(r.editing.is_some());
        });
        assert!(!root.join("Start.md").exists());
    }

    #[gpui::test]
    fn dont_save_cannot_leave_when_latest_recovery_write_fails(cx: &mut TestAppContext) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.editing
                .as_ref()
                .unwrap()
                .set_value("# Unsaved draft\n", window, cx);
        });
        std::fs::remove_file(root.join("Start.md")).unwrap();
        settle(visual, &reader, "deleted", |r| {
            disk(r) == Some(Disk::Missing)
        });
        // A file in place of the recovery directory reliably fails even when
        // the test runner has elevated filesystem permissions.
        let journal = temp.path().join("state/editor-drafts");
        std::fs::rename(&journal, temp.path().join("saved-drafts")).unwrap();
        std::fs::write(&journal, "blocked").unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        click(visual, "deleted-note-dont-save");
        reader.read_with(visual, |r, cx| {
            assert_eq!(r.current_rel, "Start.md");
            assert_eq!(r.source_text(cx).as_deref(), Some("# Unsaved draft\n"));
            assert!(r.deleted_current().is_some());
        });
        assert!(!root.join("Start.md").exists());
    }

    #[gpui::test]
    fn deletion_during_preparation_and_same_note_reopen_keep_the_retained_copy(
        cx: &mut TestAppContext,
    ) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());
        let prepared = reader.read_with(visual, |r, _| {
            okilum_core::render::reader_document(&r.vault, "Other.md").unwrap()
        });
        std::fs::remove_file(root.join("Start.md")).unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.accept_prepared_document(
                prepared_links::DocumentRequest {
                    rel: "Other.md".into(),
                    jump: None,
                    heading: None,
                    history_index: None,
                    restore_position: None,
                },
                Ok(prepared.into()),
                window,
                cx,
            );
        });
        visual.run_until_parked();
        click(visual, "deleted-note-cancel");
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "Start.md");
            assert!(r.note_source.contains("only copy"));
        });
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Start.md", None, window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(disk(r), Some(Disk::Missing));
            assert!(r.note_source.contains("only copy"));
        });
        // Choosing another target while the dialog is open cannot bypass it.
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Other.md", None, window, cx)
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| r.show_empty_vault(window, cx));
        click(visual, "deleted-note-cancel");
        reader.read_with(visual, |r, _| assert_eq!(r.current_rel, "Start.md"));
    }
}
