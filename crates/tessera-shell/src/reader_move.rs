//! Explicit rename/move with lossless link previews and revision-bound application.
use super::*;
use gpui_component::{notification::Notification, WindowExt};
use tessera_core::{
    file_editor::{EditorLock, FileEditor},
    link_rewrite::{Operation, Preview},
    note_move::MovePlan,
};

pub(super) fn display_name(path: &str) -> String {
    let path = Path::new(path);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    name.strip_suffix(".md").unwrap_or(&name).to_owned()
}

fn move_message(from: &str, to: &str, links: Option<&Preview>) -> String {
    let mut message = if Path::new(from).parent() == Path::new(to).parent() {
        format!(
            "Renamed to {}",
            if links.is_some_and(|p| p.directory.is_some()) {
                Path::new(to)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            } else {
                display_name(to)
            }
        )
    } else {
        let parent = Path::new(to).parent().unwrap_or(Path::new(""));
        let folder = if parent.as_os_str().is_empty() {
            "Home".into()
        } else {
            parent
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        };
        format!("Moved to {folder}")
    };
    if let Some(links) = links.filter(|p| !p.changes.is_empty()) {
        message.push_str(&format!(
            " · updated {} {}",
            links.changes.len(),
            if links.changes.len() == 1 {
                "link"
            } else {
                "links"
            }
        ));
    }
    message
}

fn undo_message(from: &str, to: &str) -> &'static str {
    if Path::new(from).parent() == Path::new(to).parent() {
        "Rename undone"
    } else {
        "Move undone"
    }
}

fn needs_move_confirmation(preview: &Preview) -> bool {
    preview.affected_paths().len() > 20
        || !preview.skipped.is_empty()
        || !preview.skipped_files.is_empty()
}

struct MoveProgress;

struct PendingMove {
    links: Preview,
    root: PathBuf,
    from: String,
    to: String,
    was_editing: bool,
    current: String,
    // In read mode hold the same path lock as source editing throughout preview.
    guard: Option<FileEditor>,
    destination_guard: Option<EditorLock>,
}

pub(super) struct Renaming {
    pub path: String,
    pub in_header: bool,
    root: PathBuf,
    pub input: Entity<InputState>,
    pub error: Option<String>,
    error_input: Option<String>,
    directory: bool,
    _subscription: Subscription,
}

impl Reader {
    fn enable_rename(&mut self, cx: &mut Context<Self>) {
        if let Some(rename) = &self.renaming {
            rename
                .input
                .update(cx, |input, cx| input.set_disabled(false, cx));
        }
    }

    fn rename_error(&mut self, message: String, cx: &mut Context<Self>) {
        eprintln!("Rename/move failed: {message}");
        let message = if message.contains("exist") || message.contains("already occupied") {
            "This name is already in use.".to_owned()
        } else if message.contains("Restore and save") {
            "Restore and save the draft first.".to_owned()
        } else if message.contains("stale") || message.contains("changed") {
            "Files changed. Try renaming again.".to_owned()
        } else if message.contains("unsaved") {
            "Save or discard unsaved edits first.".to_owned()
        } else if message.contains("inside")
            || message.contains("relative")
            || message.contains("component")
        {
            "Choose a name inside this vault.".to_owned()
        } else {
            "Could not rename. Check recovery.".to_owned()
        };
        self.enable_rename(cx);
        if let Some(rename) = self.renaming.as_mut() {
            rename.error_input = Some(rename.input.read(cx).value().to_string());
            rename.error = Some(message);
        } else {
            self.link_notice = Some(message);
        }
    }

    fn move_undo_toast(
        &mut self,
        journal: PathBuf,
        message: String,
        undone: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.session_directory.clone() else {
            return;
        };
        let reader = cx.weak_entity();
        self.move_notice_generation = self.move_notice_generation.wrapping_add(1);
        let generation = self.move_notice_generation;
        window.push_notification(
            Notification::new()
                .id::<MoveProgress>()
                .placement(Anchor::BottomRight)
                .py_2()
                .autohide(false)
                .message(message)
                .action(move |_, _, cx| {
                    let notice = cx.weak_entity();
                    let reader = reader.clone();
                    let journal = journal.clone();
                    let state = state.clone();
                    reader_icon_button("undo-move", IconName::Undo2, "Undo", cx)
                        .debug_selector(|| "undo-move".into())
                        .on_click(move |_, window, cx| {
                            let _ = reader.update(cx, |r, cx| {
                                if r.renaming.is_some() {
                                    reader_toast::error(
                                        "Finish or cancel the rename first.",
                                        window,
                                        cx,
                                    );
                                    return;
                                }
                                if r.note_move_pending || r.trash_pending {
                                    reader_toast::error(
                                        "Wait for the current operation to finish.",
                                        window,
                                        cx,
                                    );
                                    return;
                                }
                                match r.revert_link_move(&journal, &state, window, cx) {
                                    Ok(()) => {
                                        let _ = notice.update(cx, |n, cx| n.dismiss(window, cx));
                                        reader_toast::transient(undone, window, cx);
                                    }
                                    Err(error) => reader_toast::error(
                                        format!("Cannot undo: {error:#}"),
                                        window,
                                        cx,
                                    ),
                                }
                            });
                        })
                }),
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(8)).await;
            let _ = this.update_in(cx, |r, window, cx| {
                if r.move_notice_generation == generation {
                    window.remove_notification::<MoveProgress>(cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn rename_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_preview.is_some() {
            return;
        }
        self.begin_rename(self.current_rel.clone(), window, cx);
    }

    pub(super) fn rename_tree_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.tree.cursor.clone() {
            self.begin_rename(path, window, cx);
        }
    }

    pub(super) fn begin_rename(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_rename_at(path, false, window, cx);
    }

    pub(super) fn rename_note_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_rename_at(self.current_rel.clone(), true, window, cx);
    }

    fn begin_rename_at(
        &mut self,
        path: String,
        in_header: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.note_move_pending || self.trash_pending {
            return;
        }
        let directory =
            std::fs::symlink_metadata(self.vault_root.join(&path)).is_ok_and(|m| m.is_dir());
        if (!directory
            && !Path::new(&path)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md")))
            || self.loading.as_ref().is_some_and(|l| l.active)
        {
            self.link_notice =
                Some("Select a note or folder and wait for loading to finish.".into());
            cx.notify();
            return;
        }
        self.creation = None;
        let was_expanded = self
            .tree
            .rows
            .iter()
            .any(|row| row.path == path && row.expanded);
        if !in_header {
            self.reveal_in_tree(&path, window, cx);
        }
        if directory && !was_expanded {
            self.tree.toggle(&path);
        }
        self.tree_revealed = path.clone();
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Name");
            let name = if directory {
                Path::new(&path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            } else {
                display_name(&path)
            };
            input.set_value(name, window, cx);
            input
        });
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.commit_rename(window, cx),
                InputEvent::Change => {
                    if let Some(rename) = this.renaming.as_mut() {
                        // Input may emit Change on Enter without changing bytes.
                        // Keep a rejected destination visible until it is edited.
                        if rename.error_input.as_deref()
                            != Some(rename.input.read(cx).value().as_ref())
                        {
                            rename.error = None;
                            rename.error_input = None;
                        }
                    }
                    cx.notify();
                }
                _ => {}
            });
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.renaming = Some(Renaming {
            path,
            in_header,
            root: self.vault_root.clone(),
            input,
            error: None,
            error_input: None,
            directory,
            _subscription: subscription,
        });
        cx.notify();
    }

    pub(super) fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.note_move_pending {
            return;
        }
        let in_header = self.renaming.as_ref().is_some_and(|r| r.in_header);
        self.renaming = None;
        // Ending an inline rename is not document navigation. Keep the selected
        // folder's expansion instead of revealing its open descendant again.
        self.tree_revealed = self.selected_file().to_owned();
        if in_header {
            self.focus_handle.focus(window, cx);
        } else {
            self.tree_focus.focus(window, cx);
        }
        cx.notify();
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.note_move_pending {
            return;
        }
        let Some(rename) = self.renaming.as_ref() else {
            return;
        };
        let from = rename.path.clone();
        let value = rename.input.read(cx).value().to_string();
        if value.trim().is_empty() {
            if let Some(rename) = self.renaming.as_mut() {
                rename.error = Some("Enter a name.".into());
                rename.error_input = Some(value);
            }
            cx.notify();
            return;
        }
        let mut destination = if value.contains('/') {
            PathBuf::from(&value)
        } else {
            Path::new(&from)
                .parent()
                .unwrap_or(Path::new(""))
                .join(&value)
        };
        if !rename.directory
            && !destination
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            let name = destination
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            destination.set_file_name(format!("{name}.md"));
        }
        if rename.root == self.vault_root && destination == Path::new(&from) {
            self.cancel_rename(window, cx);
            return;
        }
        let result = if rename.root != self.vault_root {
            Err(anyhow::anyhow!("The open vault changed; start again"))
        } else {
            self.start_move_preview(&self.vault_root.join(&destination), &from, window, cx)
        };
        match result {
            Ok(()) => {
                if let Some(rename) = &self.renaming {
                    rename
                        .input
                        .update(cx, |input, cx| input.set_disabled(true, cx));
                }
            }
            Err(error) => {
                self.rename_error(format!("{error:#}"), cx);
            }
        }
        cx.notify();
    }

    pub(super) fn remap_move_sidebar(&mut self, from: &str, to: &str, cx: &mut Context<Self>) {
        for path in &mut self.sidebar.pinned {
            *path = tessera_core::link_rewrite::moved_path(path, from, to);
        }
        for (path, _) in &mut self.sidebar.recent {
            *path = tessera_core::link_rewrite::moved_path(path, from, to);
        }
        self.tree_revealed = tessera_core::link_rewrite::moved_path(&self.tree_revealed, from, to);
        self.save_sidebar(cx);
    }

    pub(super) fn start_move_preview(
        &mut self,
        destination: &Path,
        from: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.loading.as_ref().is_some_and(|l| l.active),
            "Wait for loading to finish"
        );
        self.check_move_editors(&[from.to_owned()], cx)?;
        let directory = std::fs::symlink_metadata(self.vault_root.join(from))?.is_dir();
        let other_source_editor = from == self.current_rel && self.source_has_other_editor(cx);
        let state = self
            .session_directory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No recovery storage is available"))?;
        let guard = if !directory
            && (from != self.current_rel || self.editing.is_none())
            && !other_source_editor
        {
            let editor =
                FileEditor::open(&self.vault_root.join(from), &state.join("editor-drafts"))?;
            anyhow::ensure!(
                !editor.dirty(),
                "Restore and save this note's unsaved draft before moving it"
            );
            Some(editor)
        } else {
            None
        };
        let mut path: PathBuf = destination.components().collect();
        if !directory && path.extension().is_none() {
            path.set_extension("md");
        }
        let to = path
            .strip_prefix(&self.vault_root)
            .map_err(|_| anyhow::anyhow!("Choose a destination inside the open folder"))?
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Use a UTF-8 filename"))?
            .to_owned();
        if !directory {
            MovePlan::prepare(&self.vault_root, Path::new(from), Path::new(&to))?;
        }
        let destination_guard =
            FileEditor::reserve_destination(&path, &state.join("editor-drafts"))?;
        let root = self.vault_root.clone();
        let from = from.to_owned();
        let current = self.current_rel.clone();
        let was_editing = self.editing.is_some();
        let index = self.move_index.clone();
        let state = self.session_directory.clone();
        let cancel = reader_loading::Cancellation::default();
        self.note_move_pending = true;
        self.move_notice_generation = self.move_notice_generation.wrapping_add(1);
        let button_cancel = cancel.clone();
        window.push_notification(
            Notification::new()
                .id::<MoveProgress>()
                .message(if Path::new(&from).parent() == Path::new(&to).parent() {
                    "Preparing rename…"
                } else {
                    "Preparing move…"
                })
                .autohide(false)
                .placement(Anchor::BottomRight)
                .action(move |_, _, cx| {
                    let cancel = button_cancel.clone();
                    reader_icon_button("cancel-move-scan", IconName::Close, "Cancel", cx)
                        .on_click(move |_, _, _| cancel.cancel())
                }),
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let worker_cancel = cancel.clone();
            let worker_root = root.clone();
            let worker_from = from.clone();
            let worker_to = to.clone();
            let worker = cx.background_executor().spawn(async move {
                let result = Preview::prepare_with(
                    &worker_root,
                    &worker_from,
                    &worker_to,
                    index.as_deref(),
                    &mut |_, _| worker_cancel.check(),
                );
                if let Ok(preview) = &result {
                    reader_diagnostics::record_move_preview(
                        &worker_root,
                        state.as_deref(),
                        &preview.timings,
                    );
                }
                result
            });
            let result = worker.await;
            let answer = this
                .update_in(cx, |this, window, cx| {
                    window.remove_notification::<MoveProgress>(cx);
                    if cancel.check().is_err() {
                        this.note_move_pending = false;
                        this.enable_rename(cx);
                        cx.notify();
                        return None;
                    }
                    let result = result.and_then(|links| {
                        anyhow::ensure!(
                            this.vault_root == root
                                && this.current_rel == current
                                && this.editing.is_some() == was_editing,
                            "The open note changed; preview again"
                        );
                        this.check_move_editors(&links.editor_paths(), cx)?;
                        Ok(PendingMove {
                            links,
                            root,
                            from,
                            to,
                            was_editing,
                            current,
                            guard,
                            destination_guard: Some(destination_guard),
                        })
                    });
                    match result {
                        Ok(pending) if needs_move_confirmation(&pending.links) => {
                            Some(this.show_move_preview(pending, window, cx))
                        }
                        Ok(pending) => {
                            this.finish_move(pending, true, window, cx);
                            None
                        }
                        Err(error) => {
                            this.note_move_pending = false;
                            this.rename_error(format!("{error:#}"), cx);
                            cx.notify();
                            None
                        }
                    }
                })
                .ok()
                .flatten();
            if let Some((pending, answer)) = answer {
                let confirmed = answer.recv().await.unwrap_or(None);
                let _ = this.update_in(cx, |this, window, cx| {
                    this.note_move_pending = false;
                    if let Some(update) = confirmed {
                        this.finish_move(pending, update, window, cx);
                    } else {
                        this.enable_rename(cx);
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        Ok(())
    }

    fn show_move_preview(
        &mut self,
        pending: PendingMove,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (PendingMove, async_channel::Receiver<Option<bool>>) {
        let (send, receive) = async_channel::bounded(1);
        let display = pending.links.clone();
        let expanded = Rc::new(std::cell::Cell::new(false));
        window.open_dialog(cx, move |dialog, _, _| {
            let cancel = send.clone();
            let update = send.clone();
            let close = send.clone();
            let enter = send.clone();
            let paths: std::collections::BTreeSet<_> =
                display.changes.iter().map(|c| c.path.clone()).collect();
            let names: Vec<_> = paths.iter().map(|path| display_name(path)).collect();
            let skipped: Vec<_> = display
                .skipped
                .iter()
                .map(|s| format!("{} · link unchanged", display_name(&s.path)))
                .chain(
                    display
                        .skipped_files
                        .iter()
                        .map(|s| format!("{} · could not read", display_name(&s.path))),
                )
                .collect();
            let show_all = expanded.get();
            let toggle = expanded.clone();
            let moving = Path::new(&display.from).parent() != Path::new(&display.to).parent();
            let action = if moving { "Move" } else { "Rename" };
            dialog
                .title(format!("{action} {}?", display_name(&display.to)))
                .width(px(440.))
                .overlay_closable(false)
                .on_ok(move |_, _, _| {
                    let _ = enter.try_send(Some(true));
                    true
                })
                .on_close(move |_, _, _| {
                    let _ = close.try_send(None);
                })
                .child(
                    v_flex()
                        .id("move-sheet-notes")
                        .max_h(px(300.))
                        .overflow_y_scroll()
                        .gap_2()
                        .when(!names.is_empty(), |d| {
                            d.child(format!("{} notes affected", paths.len()))
                        })
                        .children(
                            names
                                .iter()
                                .take(if show_all { usize::MAX } else { 5 })
                                .map(|name| div().text_size(px(14.)).child(name.clone())),
                        )
                        .when(names.len() > 5, |d| {
                            d.child(
                                Button::new("move-more-notes")
                                    .ghost()
                                    .small()
                                    .label(if show_all {
                                        "Show less".into()
                                    } else {
                                        format!("+{} more", names.len() - 5)
                                    })
                                    .on_click(move |_, window, _| {
                                        toggle.set(!toggle.get());
                                        window.refresh();
                                    }),
                            )
                        })
                        .when(!skipped.is_empty(), |d| {
                            d.child(div().mt_2().child("Not updated"))
                        })
                        .children(
                            skipped
                                .iter()
                                .map(|name| div().text_size(px(14.)).child(name.clone())),
                        ),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(Button::new("move-cancel").label("Cancel").on_click(
                            move |_, window, cx| {
                                let _ = cancel.try_send(None);
                                window.close_dialog(cx);
                            },
                        ))
                        .child(
                            Button::new("move-update")
                                .debug_selector(|| "move-update".into())
                                .primary()
                                .label(action)
                                .on_click(move |_, window, cx| {
                                    let _ = update.try_send(Some(true));
                                    window.close_dialog(cx);
                                }),
                        ),
                )
        });
        (pending, receive)
    }

    fn finish_move(
        &mut self,
        pending: PendingMove,
        update: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut pending = pending;
        let result = (|| -> anyhow::Result<_> {
            anyhow::ensure!(
                self.vault_root == pending.root
                    && self.current_rel == pending.current
                    && self.editing.is_some() == pending.was_editing,
                "The open note changed. Nothing was moved; preview again"
            );
            anyhow::ensure!(
                !self.loading.as_ref().is_some_and(|l| l.active),
                "Loading changed. Preview again"
            );
            self.check_move_editors(&pending.links.editor_paths(), cx)?;
            drop(pending.guard.take());
            drop(pending.destination_guard.take());
            let mut selected = pending.links.clone();
            if !update {
                selected.changes.clear();
            }
            let task = self.apply_link_move(&selected, window, cx)?;
            Ok(task)
        })();
        let task = match result {
            Ok(task) => task,
            Err(error) => {
                self.note_move_pending = false;
                self.rename_error(format!("{error:#}"), cx);
                cx.notify();
                return;
            }
        };
        self.note_move_pending = true;
        window.push_notification(
            Notification::new()
                .id::<MoveProgress>()
                .message("Renaming…")
                .autohide(false)
                .placement(Anchor::BottomRight),
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.and_then(|applied| {
                anyhow::ensure!(
                    applied.moved,
                    "{}",
                    applied
                        .warning
                        .unwrap_or_else(|| "Move interrupted; use Recover link moves".into())
                );
                Ok(applied)
            });
            let _ = this.update_in(cx, |this, window, cx| {
                this.note_move_pending = false;
                window.remove_notification::<MoveProgress>(cx);
                this.complete_move(pending, update, result, window, cx);
            });
        })
        .detach();
    }

    fn complete_move(
        &mut self,
        pending: PendingMove,
        update: bool,
        result: anyhow::Result<tessera_core::link_rewrite::Applied>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_move_editor(cx);
        match result {
            Err(error) => {
                self.sync_move_input(window, cx);
                self.rename_error(format!("{error:#}"), cx);
            }
            Ok(moved) => {
                self.renaming = None;
                let journal = moved.journal.clone();
                let mut changes = tessera_core::Changes {
                    changed: std::collections::BTreeSet::from([pending.to.clone()]),
                    removed: std::collections::BTreeSet::from([pending.from.clone()]),
                    ..Default::default()
                };
                if pending.links.directory.is_some() {
                    changes.directories.insert(pending.from.clone());
                    changes.directories.insert(pending.to.clone());
                    changes
                        .changed
                        .extend(pending.links.affected_paths().iter().map(|p| {
                            tessera_core::link_rewrite::moved_path(p, &pending.from, &pending.to)
                        }));
                }
                if update {
                    changes
                        .changed
                        .extend(pending.links.changes.iter().map(|c| {
                            tessera_core::link_rewrite::moved_path(
                                &c.path,
                                &pending.from,
                                &pending.to,
                            )
                        }));
                }
                if self.vault_root == pending.root {
                    self.queue_vault_mutation(changes, cx);
                }
                self.tree.note_moved(&pending.from, &pending.to);
                drop(pending.guard);
                drop(pending.destination_guard);
                for path in &mut self.history {
                    *path =
                        tessera_core::link_rewrite::moved_path(path, &pending.from, &pending.to);
                }
                self.remap_move_sidebar(&pending.from, &pending.to, cx);
                let next_current = tessera_core::link_rewrite::moved_path(
                    &self.current_rel,
                    &pending.from,
                    &pending.to,
                );
                let showing_file = self.file_preview.is_some();
                if let Some(file) = &self.file_preview {
                    let next = tessera_core::link_rewrite::moved_path(
                        &file.rel,
                        &pending.from,
                        &pending.to,
                    );
                    if next != file.rel {
                        self.preview_file(&next, window, cx);
                    }
                }
                if showing_file {
                    self.current_rel = next_current.clone();
                    self.current_title = self.vault.note_title(&next_current);
                }
                if showing_file || next_current == self.current_rel {
                    self.sync_move_input(window, cx);
                    if let Some(warning) = moved.warning {
                        self.link_notice = Some(warning);
                    } else {
                        self.move_undo_toast(
                            journal.clone(),
                            move_message(
                                &pending.from,
                                &pending.to,
                                update.then_some(&pending.links),
                            ),
                            undo_message(&pending.from, &pending.to),
                            window,
                            cx,
                        );
                    }
                    cx.notify();
                    return;
                }
                self.editing = None;
                self.document_preparation_generation =
                    self.document_preparation_generation.wrapping_add(1);
                let success_message =
                    move_message(&pending.from, &pending.to, update.then_some(&pending.links));
                let document = tessera_core::render::reader_document(&self.vault, &next_current)
                    .map(|d| prepared_links::PreparedDocument {
                        source: d.rendered,
                        original: Some(d.original_body),
                        identities: d.links,
                        frontmatter: d.frontmatter,
                    });
                // Even a concurrent deletion/read failure must not leave the UI
                // pointing at the old path after a successful rename.
                self.current_rel = next_current.clone();
                self.current_title = self.vault.note_title(&next_current);
                self.accept_prepared_document(
                    prepared_links::DocumentRequest {
                        rel: next_current,
                        jump: None,
                        heading: None,
                        history_index: None,
                        restore_position: None,
                    },
                    document,
                    window,
                    cx,
                );
                if pending.was_editing && moved.warning.is_none() {
                    self.toggle_source(window, cx);
                }
                if let Some(warning) = moved.warning {
                    self.link_notice = Some(match self.link_notice.take() {
                        Some(error) => format!("{warning} {error}"),
                        None => warning,
                    });
                } else {
                    self.move_undo_toast(
                        journal,
                        success_message,
                        undo_message(&pending.from, &pending.to),
                        window,
                        cx,
                    );
                }
            }
        }
        cx.notify();
    }
}

impl Reader {
    pub(super) fn recover_link_moves(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.session_directory.clone() else {
            self.link_notice = Some("No recovery storage is available in this window.".into());
            cx.notify();
            return;
        };
        let operations = match Operation::list(&state, &self.vault_root) {
            Ok(paths) => paths,
            Err(error) => {
                self.link_notice = Some(format!("Cannot read move recovery: {error:#}"));
                cx.notify();
                return;
            }
        };
        let mut entries = vec![];
        let warnings = operations.warnings;
        for path in operations.operations {
            match Operation::load(&path) {
                Ok(op) => entries.push((
                    path,
                    format!(
                        "{} → {} · {} files · {}",
                        op.from,
                        op.to,
                        op.files.len(),
                        if op.complete {
                            "completed"
                        } else {
                            "interrupted"
                        }
                    ),
                )),
                Err(error) => {
                    self.link_notice = Some(format!("Cannot read move recovery: {error:#}"));
                    cx.notify();
                    return;
                }
            }
        }
        let reader = cx.weak_entity();
        window.open_dialog(cx,move |dialog,_,_|{
            let reader=reader.clone();let state=state.clone();
            dialog.title("Recover link moves").width(px(720.))
                .child("Original bytes are retained for each operation. Revert refuses later external edits. Unsaved affected editors must be saved or discarded before reverting.")
                .children(warnings.iter().map(|warning|div().child(warning.clone())))
                .child(if entries.is_empty(){"No retained link moves for this folder."}else{"Select an operation to review its files before reverting:"})
                .child(v_flex().id("link-move-recovery-list").max_h(px(360.)).overflow_y_scroll().gap_2().children(entries.iter().enumerate().map(|(i,(path,label))|{
                    let reader=reader.clone();let state=state.clone();let path=path.clone();
                    Button::new(("recover-link-move",i)).label(label.clone()).on_click(move |_,window,cx|{
                        window.close_dialog(cx);
                        let _=reader.update(cx,|r,cx|r.confirm_revert_link_move(path.clone(),state.clone(),window,cx));
                    })
                })))
        });
    }
    fn confirm_revert_link_move(
        &mut self,
        path: PathBuf,
        state: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let operation = match Operation::load(&path) {
            Ok(op) => op,
            Err(error) => {
                self.link_notice = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let detail=format!("{} → {}\n\nRevert the move and link changes in:\n{}\n\nFiles edited since this operation will not be overwritten. Original bytes remain available if recovery cannot finish.",operation.to,operation.from,operation.files.keys().cloned().collect::<Vec<_>>().join("\n"));
        let answer = window.prompt(
            PromptLevel::Warning,
            "Revert this link move?",
            Some(&detail),
            &["Cancel", "Revert"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(1) {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                match this.revert_link_move(&path, &state, window, cx) {
                    Ok(()) => reader_toast::transient("Link move reverted. Original bytes restored.", window, cx),
                    Err(error) => this.link_notice = Some(format!(
                        "Recovery stopped: {error:#}. Original bytes are retained; resolve the reported file and try again."
                    )),
                }
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
    #[test]
    fn move_feedback_omits_empty_counts_and_keeps_real_updates() {
        assert_eq!(undo_message("Source", "Target/Source"), "Move undone");
        assert_eq!(
            undo_message("Folder/Old.md", "Folder/New.md"),
            "Rename undone"
        );
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("Folder")).unwrap();
        std::fs::write(root.path().join("Start.md"), "Body").unwrap();
        let empty = Preview::prepare(root.path(), "Start.md", "Folder/Next.md").unwrap();
        assert!(!needs_move_confirmation(&empty));
        let rename = Preview::prepare(root.path(), "Start.md", "Next.md").unwrap();
        assert!(!needs_move_confirmation(&rename));
        assert_eq!(
            move_message("Start.md", "Folder/Next.md", Some(&empty)),
            "Moved to Folder"
        );
        assert_eq!(
            move_message("Folder/Start.md", "Next.md", None),
            "Moved to Home"
        );
        assert_eq!(move_message("Start.md", "Next.md", None), "Renamed to Next");
        assert_eq!(
            move_message("Nested/Start.md", "Nested/Next.md", None),
            "Renamed to Next"
        );
        std::fs::write(root.path().join("Ref.md"), "[[Start]]").unwrap();
        let linked = Preview::prepare(root.path(), "Start.md", "Folder/Next.md").unwrap();
        assert!(!needs_move_confirmation(&linked));
        assert_eq!(
            move_message("Start.md", "Folder/Next.md", Some(&linked)),
            "Moved to Folder · updated 1 link"
        );
    }

    #[test]
    fn confirmation_is_reserved_for_large_or_unresolved_changes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Start.md"), "# Heading").unwrap();
        for index in 0..19 {
            std::fs::write(root.path().join(format!("Ref {index}.md")), "[[Start]]").unwrap();
        }
        let small = Preview::prepare(root.path(), "Start.md", "Next.md").unwrap();
        assert_eq!(small.affected_paths().len(), 20);
        assert!(!needs_move_confirmation(&small));
        std::fs::write(root.path().join("Another.md"), "[[Start]]").unwrap();
        let large = Preview::prepare(root.path(), "Start.md", "Next.md").unwrap();
        assert_eq!(large.affected_paths().len(), 21);
        assert!(needs_move_confirmation(&large));
        for index in 0..19 {
            std::fs::remove_file(root.path().join(format!("Ref {index}.md"))).unwrap();
        }
        std::fs::write(root.path().join("Another.md"), "[[No such note]]").unwrap();
        let unresolved = Preview::prepare(root.path(), "Start.md", "Next.md").unwrap();
        assert!(!unresolved.skipped.is_empty());
        assert!(needs_move_confirmation(&unresolved));
    }

    #[gpui::test]
    fn inline_names_receive_tree_keys_and_same_name_is_noop(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("notes")).unwrap();
        let root = dir.path().join("notes").canonicalize().unwrap();
        std::fs::write(root.join("start.md"), "Original bytes").unwrap();
        std::fs::write(root.join("Release.v2.md"), "Dotted name").unwrap();
        std::fs::create_dir(root.join("Archive.md")).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(dir.path().join("index")),
                        session_directory: Some(dir.path().join("state")),
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
        for rename in [false, true] {
            reader.update_in(visual, |r, window, cx| {
                if rename {
                    r.rename_note(window, cx);
                } else {
                    r.new_note(Some(""), window, cx);
                }
            });
            visual.run_until_parked();
            visual.simulate_input("two words");
            visual.simulate_keystrokes("left delete f2 down up");
            reader.read_with(visual, |r, cx| {
                let input = if rename {
                    &r.renaming.as_ref().unwrap().input
                } else {
                    &r.creation.as_ref().unwrap().input
                };
                assert_eq!(input.read(cx).value().as_ref(), "two word");
                assert!(!r.note_move_pending);
                assert!(!r.trash_pending);
                assert_eq!(r.current_rel, "start.md");
            });
            visual.simulate_keystrokes("escape");
        }
        // Positive control: F2 still starts rename when the tree itself owns focus.
        visual.simulate_keystrokes("f2");
        reader.read_with(visual, |r, _| assert!(r.renaming.is_some()));
        // The suggested full name is selected. Enter must not erase it or start a move.
        visual.simulate_keystrokes("enter");
        reader.update_in(visual, |r, window, cx| {
            assert!(r.renaming.is_none());
            assert!(!r.note_move_pending);
            assert!(r.link_notice.is_none());
            assert!(r.tree_focus.is_focused(window));
            r.rename_note(window, cx);
        });
        visual.run_until_parked();
        visual.simulate_input("start");
        visual.simulate_keystrokes("enter");
        reader.read_with(visual, |r, _| {
            assert!(r.renaming.is_none());
            assert!(!r.note_move_pending);
            assert!(r.link_notice.is_none());
        });
        assert_eq!(
            std::fs::read_to_string(root.join("start.md")).unwrap(),
            "Original bytes"
        );
        assert!(!root.join("two word.md").exists());
        for path in ["Release.v2.md", "Archive.md"] {
            reader.update_in(visual, |r, window, cx| {
                r.begin_rename(path.into(), window, cx)
            });
            visual.run_until_parked();
            visual.simulate_keystrokes("enter");
            visual.run_until_parked();
            reader.read_with(visual, |r, _| {
                assert!(r.renaming.is_none());
                assert!(!r.note_move_pending);
            });
            assert!(
                root.join(path).exists(),
                "dotted names must remain unchanged"
            );
        }
        // A real click preserves docked tree focus, but dismisses a compact
        // sidebar and focuses the document (#677), including after async load.
        for width in [1400., 800.] {
            visual.simulate_resize(size(px(width), px(860.)));
            visual.run_until_parked();
            reader.update_in(visual, |r, window, cx| {
                r.reveal_in_tree("start.md", window, cx);
                r.show_empty_vault(window, cx);
            });
            visual.run_until_parked();
            for key in ["f2", "enter"] {
                let row = visual.debug_bounds("tree-row-start.md").unwrap();
                // Stay away from the pin affordance at the right of the row.
                visual.simulate_click(
                    point(row.left() + px(70.), row.center().y),
                    Modifiers::default(),
                );
                visual.run_until_parked();
                reader.update_in(visual, |r, window, cx| {
                    assert_eq!(
                        r.current_rel, "start.md",
                        "width={width}, key={key}, row={row:?}, cursor={:?}, notice={:?}",
                        r.tree.cursor, r.link_notice
                    );
                    if crate::reader_layout::overlay(width) {
                        assert!(!r.panels.visible(crate::reader_layout::Panel::Notes, width));
                        assert!(!r.tree_focus.is_focused(window));
                        assert!(r.content.read(cx).focus_handle().is_focused(window));
                        // Rename remains available after explicitly reopening the tree.
                        r.reveal_in_tree("start.md", window, cx);
                    } else {
                        assert!(
                            r.tree_focus.is_focused(window),
                            "docked tree focus after load"
                        );
                        assert!(!r.content.read(cx).focus_handle().is_focused(window));
                    }
                });
                visual.run_until_parked();
                visual.simulate_keystrokes(key);
                reader.read_with(visual, |r, _| {
                    assert!(r.renaming.is_some(), "{key} after click")
                });
                visual.simulate_keystrokes("escape");
            }
        }
        // The toolbar rename lives in the document header, without opening
        // the compact navigator or changing bytes for a same-name submit.
        reader.update_in(visual, |r, window, cx| {
            r.close_panel(crate::reader_layout::Panel::Notes, window, cx);
            r.rename_note_title(window, cx);
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("reader-notes-panel").is_none());
        reader.read_with(visual, |r, _| {
            assert!(r.renaming.as_ref().unwrap().in_header)
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(r.renaming.is_none());
            assert!(!r.note_move_pending);
        });
        assert_eq!(
            std::fs::read_to_string(root.join("start.md")).unwrap(),
            "Original bytes"
        );
    }

    #[gpui::test]
    fn native_preview_cancel_move_and_recovery_guard(cx: &mut TestAppContext) {
        cx.update(|cx| {
            // Modal animations use wall time, not the test executor clock. Keep
            // pointer hit targets stable while testing the move/recovery flow.
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let dir = std::env::temp_dir().join(format!("tessera-move-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("notes/Folder")).unwrap();
        let root = dir.join("notes").canonicalize().unwrap();
        std::fs::write(root.join("start.md"), "# Source\r\n[[target]]\r\n").unwrap();
        std::fs::write(root.join("target.md"), "[[start]]").unwrap();
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
        let generation = reader.read_with(visual, |v, _| v.loading.as_ref().unwrap().generation);
        reader.update_in(visual, |reader, window, cx| {
            reader.reveal_in_tree("start.md", window, cx);
            reader.tree_focus.focus(window, cx);
        });
        visual.simulate_keystrokes("f2");
        visual.run_until_parked();
        assert!(visual.debug_bounds("inline-rename-row").is_some());
        assert!(!visual.did_prompt_for_new_path());
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| assert!(reader.renaming.is_none()));
        reader.update_in(visual, |reader, window, cx| reader.rename_note(window, cx));
        reader.update_in(visual, |reader, window, cx| {
            let input = reader.renaming.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| {
                input.set_value("Folder/Новое 🧠.md", window, cx)
            });
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            if let Some(rename) = &reader.renaming {
                assert!(
                    rename.input.read(cx).focus_handle(cx).is_focused(window),
                    "rename input must receive Enter"
                );
            }
        });
        // Cancel inline before submitting; Enter now commits immediately.
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(root.join("start.md").exists());
        assert!(!root.join("Folder/Новое 🧠.md").exists());
        reader.update_in(visual, |reader, window, cx| {
            reader.rename_note(window, cx);
            let input = reader.renaming.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("target.md", window, cx));
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            if let Some(rename) = &reader.renaming {
                assert!(
                    rename.input.read(cx).focus_handle(cx).is_focused(window),
                    "rename input must receive Enter"
                );
            }
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.renaming.as_ref().unwrap().error.is_some())
        });
        assert_eq!(
            std::fs::read_to_string(root.join("target.md")).unwrap(),
            "[[start]]"
        );
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        // Read-mode recovery must be resolved explicitly, never stranded by rename.
        {
            let mut draft =
                FileEditor::open(&root.join("start.md"), &dir.join("state/editor-drafts")).unwrap();
            draft.set_text("durable draft".into()).unwrap();
        }
        reader.update_in(visual, |reader, window, cx| reader.rename_note(window, cx));
        reader.update_in(visual, |reader, window, cx| {
            let input = reader.renaming.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| {
                input.set_value("Folder/Новое 🧠.md", window, cx)
            });
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            if let Some(rename) = &reader.renaming {
                assert!(
                    rename.input.read(cx).focus_handle(cx).is_focused(window),
                    "rename input must receive Enter"
                );
            }
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader
                .renaming
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("Restore and save"))
        });
        assert!(FileEditor::has_unsaved_draft(
            &root.join("start.md"),
            &dir.join("state/editor-drafts")
        )
        .unwrap());
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx);
            reader.rename_note(window, cx);
            assert!(
                !reader.note_move_pending,
                "Dirty editor must block before picker"
            );
            reader
                .renaming
                .as_ref()
                .unwrap()
                .input
                .update(cx, |input, cx| input.set_value("Dirty rename", window, cx));
            reader.commit_rename(window, cx);
            assert!(reader
                .renaming
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("unsaved edits"));
            assert!(reader.save_source(cx));
            reader.rename_note(window, cx);
        });
        reader.update_in(visual, |reader, window, cx| {
            let input = reader.renaming.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| {
                input.set_value("Folder/Новое 🧠.md", window, cx)
            });
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            if let Some(rename) = &reader.renaming {
                assert!(
                    rename.input.read(cx).focus_handle(cx).is_focused(window),
                    "rename input must receive Enter"
                );
            }
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("move-update").is_none(),
            "ordinary move has no modal"
        );
        reader.read_with(visual, |reader, _| {
            assert_eq!(
                reader.loading.as_ref().unwrap().generation,
                generation,
                "move must not run full prepare"
            );
            assert!(reader
                .vault
                .notes
                .iter()
                .any(|note| note.path == "Folder/Новое 🧠.md"));
            assert!(!reader
                .vault
                .notes
                .iter()
                .any(|note| note.path == "start.md"));

            assert!(
                !reader.note_move_pending,
                "Move did not finish; notice={:?}",
                reader.link_notice
            );
            assert!(
                !root.join("start.md").exists(),
                "Move was refused after confirmation: {:?}",
                reader.link_notice
            );
        });
        assert_eq!(
            std::fs::read_to_string(root.join("Folder/Новое 🧠.md")).unwrap(),
            "durable draft"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("target.md")).unwrap(),
            "[[Новое 🧠]]"
        );
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "Folder/Новое 🧠.md");
            assert_eq!(reader.current_title, "Новое 🧠");
            assert!(reader.note_source.contains("durable draft"));
            assert!(reader.editing.is_some());
        });
        std::fs::write(root.join("target.md"), "[[Folder/Новое 🧠#Heading|alias]]").unwrap();
        reader.update_in(visual, |reader, window, cx| reader.rename_note(window, cx));
        reader.update_in(visual, |reader, window, cx| {
            let input = reader.renaming.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("Final.md", window, cx));
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            if let Some(rename) = &reader.renaming {
                assert!(
                    rename.input.read(cx).focus_handle(cx).is_focused(window),
                    "rename input must receive Enter"
                );
            }
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(visual.debug_bounds("move-update").is_none());
        reader.read_with(visual, |reader, _| {
            assert!(
                root.join("Folder/Final.md").exists(),
                "pending={} renaming={} notice={:?}",
                reader.note_move_pending,
                reader.renaming.is_some(),
                reader.link_notice
            );
        });
        assert_eq!(
            std::fs::read_to_string(root.join("target.md")).unwrap(),
            "[[Folder/Final#Heading|alias]]"
        );
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "Folder/Final.md");
            assert!(reader.editing.is_some());
        });
        let move_index = reader.read_with(visual, |reader, _| reader.move_index.clone());
        // Missing index uses cancellable background fallback, without another dialog.
        reader.update_in(visual, |r, window, cx| {
            r.move_index = None;
            r.rename_note(window, cx);
        });
        visual.run_until_parked();
        visual.simulate_input("Fallback");
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(visual.debug_bounds("scan-move-links").is_none());
        assert!(visual.debug_bounds("move-update").is_none());
        reader.read_with(visual, |r, cx| {
            assert!(
                root.join("Folder/Fallback.md").exists(),
                "current={} pending={} rename={:?} notice={:?}",
                r.current_rel,
                r.note_move_pending,
                r.renaming
                    .as_ref()
                    .map(|n| (n.input.read(cx).value(), &n.error)),
                r.link_notice
            )
        });
        reader.update_in(visual, |r, window, cx| r.rename_note(window, cx));
        visual.run_until_parked();
        let undo = visual
            .debug_bounds("undo-move")
            .expect("previous move undo remains visible");
        visual.simulate_click(undo.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(
            root.join("Folder/Fallback.md").exists(),
            "Undo must not invalidate an open rename field"
        );
        reader.update_in(visual, |r, window, cx| r.cancel_rename(window, cx));
        visual.run_until_parked();
        let undo = visual.debug_bounds("undo-move").expect("move offers undo");
        visual.simulate_click(undo.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(root.join("Folder/Final.md").exists());
        assert!(!root.join("Folder/Fallback.md").exists());
        // Rename a selected row without changing the open source editor.
        reader.update_in(visual, |reader, window, cx| {
            reader.move_index = move_index;
            reader.reveal_in_tree("target.md", window, cx);
            reader.tree_focus.focus(window, cx);
        });
        visual.simulate_keystrokes("f2");
        visual.run_until_parked();
        assert!(visual.debug_bounds("inline-rename-row").is_some());
        reader.update_in(visual, |reader, window, cx| {
            let input = reader.renaming.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("Reference.md", window, cx));
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(root.join("Reference.md").exists());
        assert!(!root.join("target.md").exists());
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "Folder/Final.md");
            assert!(reader.editing.is_some());
        });
        // Large changes require the compact sheet; cancellation writes nothing.
        for index in 0..21 {
            std::fs::write(root.join(format!("Many {index}.md")), "[[Folder/Final]]").unwrap();
        }
        reader.update_in(visual, |r, window, cx| {
            r.move_index = None;
            r.rename_note(window, cx);
            r.renaming
                .as_ref()
                .unwrap()
                .input
                .update(cx, |input, cx| input.set_value("Confirmed", window, cx));
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("move-update").is_some(),
            "large preview positive control"
        );
        assert!(root.join("Folder/Final.md").exists());
        assert!(!root.join("Folder/Confirmed.md").exists());
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert!(!r.note_move_pending));
        assert!(root.join("Folder/Final.md").exists());
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        let confirm = visual
            .debug_bounds("move-update")
            .expect("retry requires a new preview");
        std::fs::write(root.join("Many 0.md"), "External edit").unwrap();
        visual.simulate_click(confirm.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(!r.note_move_pending);
            assert!(
                r.renaming.as_ref().unwrap().error.is_some(),
                "stale preview error stays under input"
            );
        });
        assert!(root.join("Folder/Final.md").exists());
        assert!(!root.join("Folder/Confirmed.md").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("Many 0.md")).unwrap(),
            "External edit"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn folder_keys_rename_without_toggling_and_move_updates_descendant_links(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("Folder/sub")).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("Folder/a.md"), "# A\r\nExact bytes\r\n").unwrap();
        std::fs::write(root.join("Ref.md"), "[[Folder/a|alias]]").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("Folder/a.md")),
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
        reader.update_in(visual, |r, window, cx| {
            r.reveal_in_tree("Folder", window, cx);
            r.tree.set_subtree("Folder", false);
            cx.notify();
        });
        visual.run_until_parked();
        for key in ["f2", "enter"] {
            visual.simulate_keystrokes(key);
            visual.run_until_parked();
            reader.read_with(visual, |r, _| {
                assert!(r.renaming.as_ref().unwrap().directory);
                assert!(
                    !r.tree
                        .rows
                        .iter()
                        .find(|row| row.path == "Folder")
                        .unwrap()
                        .expanded
                );
            });
            visual.simulate_keystrokes("enter");
            visual.run_until_parked();
            reader.read_with(visual, |r, _| {
                assert!(r.renaming.is_none());
                assert!(!r.note_move_pending);
            });
        }
        assert!(!root.join("Folder.md").exists());
        visual.simulate_keystrokes("right");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(
                r.tree
                    .rows
                    .iter()
                    .find(|row| row.path == "Folder")
                    .unwrap()
                    .expanded
            )
        });
        visual.simulate_keystrokes("right left");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(r.tree.cursor.as_deref(), Some("Folder"))
        });
        visual.simulate_keystrokes("left");
        visual.run_until_parked();
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-down");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-down");
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(
                r.tree
                    .rows
                    .iter()
                    .find(|row| row.path == "Folder")
                    .unwrap()
                    .expanded
            )
        });
        let row = visual.debug_bounds("tree-row-Folder").unwrap();
        let point = point(row.left() + px(65.), row.center().y);
        visual.simulate_click(point, Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(
                r.tree
                    .rows
                    .iter()
                    .find(|row| row.path == "Folder")
                    .unwrap()
                    .expanded,
                "single folder click only selects"
            )
        });
        visual.simulate_event(MouseDownEvent {
            position: point,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 2,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            position: point,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 2,
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(
                !r.tree
                    .rows
                    .iter()
                    .find(|row| row.path == "Folder")
                    .unwrap()
                    .expanded,
                "double click toggles once"
            )
        });
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        visual.simulate_input("Unsaved descendant");
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.begin_rename("Folder".into(), window, cx);
            r.renaming
                .as_ref()
                .unwrap()
                .input
                .update(cx, |input, cx| input.set_value("Dirty folder", window, cx));
            r.commit_rename(window, cx);
            assert!(r
                .renaming
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("unsaved edits"));
            assert!(root.join("Folder/a.md").exists());
        });
        reader.update_in(visual, |r, window, cx| {
            r.cancel_rename(window, cx);
            r.editing
                .as_ref()
                .unwrap()
                .test_input()
                .update(cx, |input, cx| input.focus(window, cx));
        });
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-z");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-z");
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert!(r.save_source(cx));
            r.toggle_source(window, cx);
            r.link_notice = None;
            r.reveal_in_tree("Folder", window, cx);
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("f2");
        visual.run_until_parked();
        visual.simulate_input("Новое 🧠//");
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        if let Some(scan) = visual.debug_bounds("scan-move-links") {
            visual.simulate_click(scan.center(), Modifiers::default());
            visual.run_until_parked();
        }
        assert!(visual.debug_bounds("move-update").is_none());
        reader.read_with(visual, |r, _| {
            assert!(!r.note_move_pending, "{:?}", r.link_notice);
            assert_eq!(r.current_rel, "Новое 🧠/a.md", "{:?}", r.link_notice);
        });
        assert!(!root.join("Folder").exists());
        assert!(root.join("Новое 🧠/sub").is_dir());
        assert_eq!(
            std::fs::read(root.join("Новое 🧠/a.md")).unwrap(),
            b"# A\r\nExact bytes\r\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("Ref.md")).unwrap(),
            "[[Новое 🧠/a|alias]]"
        );
        std::fs::write(root.join("Новое 🧠/asset.bin"), [0, 255, 128]).unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.preview_file("Новое 🧠/asset.bin", window, cx);
            r.begin_rename("Новое 🧠".into(), window, cx);
        });
        visual.run_until_parked();
        visual.simulate_input("Again");
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(visual.debug_bounds("move-update").is_none());
        reader.read_with(visual, |r, _| {
            assert_eq!(r.selected_file(), "Again/asset.bin", "{:?}", r.link_notice);
            assert!(r.file_preview.is_some());
        });
        assert_eq!(
            std::fs::read(root.join("Again/asset.bin")).unwrap(),
            [0, 255, 128]
        );
    }
}
