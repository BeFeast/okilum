//! An open note deleted outside Okilum (#1986).
//!
//! A file manager, `git checkout`, sync or a script can delete the note on
//! screen. Its contents are then only in this window: the Reader's rendered
//! source, or the editor's buffer. Okilum says so without a modal: a
//! persistent notice over the note offers Restore (write it back at its own
//! place) and Save as…, and the header strikes the title through. Leaving it
//! in Read asks first, because nothing could bring the contents back; the
//! editor keeps its buffer as a recovery draft instead.
//!
//! When the file returns at the same place (a branch switch back), the notice
//! goes away. If it returns with different contents, the window keeps showing
//! what it had and offers to reload the version on disk; it never silently
//! replaces the only copy of the old contents. In the editor the existing
//! conflict view already offers Reload / Keep my version for that case.
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
        let on_disk = match sources.get(&self.current_rel) {
            Some(raw) => Some(raw.clone()),
            None => match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                // Unreadable is not deleted; the existing notices apply.
                Err(_) => return false,
                // Present and nothing to compare with: no read on this thread.
                Ok(_) if self.deleted_note.is_none() => return false,
                Ok(_) => match std::fs::read_to_string(&path) {
                    Ok(text) => Some(text),
                    Err(_) => return false,
                },
            },
        };
        let editing = self.editing.is_some();
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
            (Some(_), Some(_)) if editing => {
                // The editor's own disk refresh decided: the same text clears,
                // a different one is a conflict with Reload / Keep my version.
                self.deleted_note = None;
                self.source_file_returned(cx);
                false
            }
            (Some(text), Some(deleted)) => {
                if text == deleted.contents {
                    self.deleted_note = None;
                    false
                } else {
                    deleted.disk = Disk::Different;
                    true
                }
            }
            (Some(_), None) => false,
        };
        if held && editing {
            self.clear_moved_or_deleted_notice();
        }
        cx.notify();
        held
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
                        .placement(Anchor::BottomRight)
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
        let result = (|| -> std::io::Result<()> {
            use std::io::Write as _;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()
        })();
        match result {
            Ok(()) => {
                self.deleted_note = None;
                if self.editing.is_some() && !self.source_restored(window, cx) {
                    // The editor's folder handle is stale (the folder was
                    // recreated): reopen the restored note.
                    self.editing = None;
                    self.prepare_document_at_place(&rel, None, window, cx);
                }
                reader_toast::transient("Note restored", window, cx);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
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
        let path = self.vault_root.join(&self.current_rel);
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                self.deleted_note = None;
                self.reconcile_document_source(Some(raw), window, cx);
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
        let picker = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut path))) = picker.await else {
                return;
            };
            if path.extension().is_none() {
                path.set_extension("md");
            }
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                let written = (|| -> std::io::Result<()> {
                    use std::io::Write as _;
                    let mut file = std::fs::File::create(&path)?;
                    file.write_all(contents.as_bytes())?;
                    file.sync_all()
                })();
                if let Err(error) = written {
                    eprintln!("Could not save the deleted note: {error:#}");
                    reader_toast::error(
                        "The note couldn’t be saved there. Choose another place.",
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
                    Some(then) => then(this, window, cx),
                    None => {
                        if let Some(saved) = inside.filter(|_| same && this.vault_root == root) {
                            this.open_note(&saved, None, window, cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Navigation away from the note on screen (`target`, or another vault
    /// with `None`). True while the note is deleted and shown in Read: the
    /// caller stops and hands its navigation to `ask_leaving_deleted_note`.
    /// The editor does not ask: leaving keeps its buffer as a durable draft
    /// (More → Recover notes), and navigation must not depend on saving a
    /// deleted path.
    pub(super) fn leaving_deleted_note(&mut self, target: Option<&str>) -> bool {
        if self.deleted_note.is_some() && self.deleted_current().is_none() {
            self.deleted_note = None;
        }
        self.deleted_note.is_some()
            && self.editing.is_none()
            && target != Some(self.current_rel.as_str())
    }

    /// Ask before a deleted note's contents are left behind; `retry` runs the
    /// navigation once the person has decided to save them or not.
    pub(super) fn ask_leaving_deleted_note(
        &mut self,
        retry: Continue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.deleted_note_asking {
            return;
        }
        self.deleted_note_asking = true;
        let disk = self
            .deleted_current()
            .map_or(Disk::Missing, |deleted| deleted.disk);
        let answer = ask_leave(window, cx, self.selected_title(), disk);
        let root = self.vault_root.clone();
        let rel = self.current_rel.clone();
        cx.spawn_in(window, async move |this, cx| {
            let choice = answer.recv().await.unwrap_or(Leave::Cancel);
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                this.deleted_note_asking = false;
                let current = this.vault_root == root && this.current_rel == rel;
                match choice {
                    Leave::Cancel => {}
                    Leave::DontSave => {
                        if current {
                            this.deleted_note = None;
                        }
                        retry(this, window, cx);
                    }
                    Leave::SaveAs if current && this.deleted_note.is_some() => {
                        this.save_deleted_note_as(Some(retry), window, cx);
                    }
                    Leave::SaveAs => retry(this, window, cx),
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

    const START: &str = "# Start\n\nonly copy of these words\n";

    /// Real time for the OS watcher, test time for the Reader's timers,
    /// until `done` holds or ~15 s are up.
    fn settle(
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
                        vault: Some(root.to_path_buf()),
                        note: Some("Start.md".into()),
                        index_dir: Some(state.join("index")),
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
        reader.read_with(visual, |r, _| {
            assert!(r.watcher.is_some(), "positive control: a watcher");
            assert_eq!(r.current_rel, "Start.md", "positive control: opened");
            assert!(
                r.note_source.contains("only copy"),
                "positive control: shown"
            );
        });
        (reader, visual)
    }

    fn vault() -> (tempfile::TempDir, PathBuf) {
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

    fn click(visual: &mut VisualTestContext, selector: &'static str) {
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

    #[gpui::test]
    fn the_editor_buffer_is_what_restore_writes(cx: &mut TestAppContext) {
        let (temp, root) = vault();
        let (reader, visual) = open(cx, &root, temp.path());
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        let edited = "# Start\n\nonly copy of these words, edited\n";
        reader.update_in(visual, |r, window, cx| {
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
}
