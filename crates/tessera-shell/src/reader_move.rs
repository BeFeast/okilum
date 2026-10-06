//! Explicit rename/move with lossless link previews and revision-bound application.
use super::*;
use gpui_component::WindowExt;
use tessera_core::{
    file_editor::{EditorLock, FileEditor},
    link_rewrite::{Operation, Preview},
    note_move::MovePlan,
};

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
    root: PathBuf,
    pub input: Entity<InputState>,
    pub error: Option<String>,
    error_input: Option<String>,
    _subscription: Subscription,
}

impl Reader {
    pub(super) fn rename_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_preview.is_some() {
            return;
        }
        self.begin_rename(self.current_rel.clone(), window, cx);
    }

    pub(super) fn rename_tree_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.tree.cursor.clone() {
            if self.tree.cursor_folder().as_deref() == Some(path.as_str()) {
                self.tree_key(TreeKey::Open, window, cx);
            } else {
                self.begin_rename(path, window, cx);
            }
        }
    }

    pub(super) fn begin_rename(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.note_move_pending || self.trash_pending {
            return;
        }
        if !Path::new(&path)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
            || self.loading.as_ref().is_some_and(|l| l.active)
        {
            self.link_notice =
                Some("Select a Markdown note and wait for loading to finish.".into());
            cx.notify();
            return;
        }
        if let Err(error) = self.check_move_editors(std::slice::from_ref(&path), cx) {
            self.link_notice = Some(error.to_string());
            cx.notify();
            return;
        }
        self.creation = None;
        self.reveal_in_tree(&path, window, cx);
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Path inside vault");
            input.set_value(path.clone(), window, cx);
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
            root: self.vault_root.clone(),
            input,
            error: None,
            error_input: None,
            _subscription: subscription,
        });
        cx.notify();
    }

    pub(super) fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.renaming = None;
        self.tree_focus.focus(window, cx);
        cx.notify();
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.renaming.as_ref() else {
            return;
        };
        let from = rename.path.clone();
        let value = rename.input.read(cx).value().to_string();
        let mut destination = PathBuf::from(&value);
        if destination.extension().is_none() {
            destination.set_extension("md");
        }
        if rename.root == self.vault_root && destination == Path::new(&from) {
            self.cancel_rename(window, cx);
            return;
        }
        let result = if rename.root != self.vault_root {
            Err(anyhow::anyhow!("The open vault changed; start again"))
        } else {
            self.start_move_preview(&self.vault_root.join(&value), &from, window, cx)
        };
        match result {
            Ok(()) => self.renaming = None,
            Err(error) => {
                if let Some(rename) = self.renaming.as_mut() {
                    rename.error = Some(format!("{error:#}"));
                    rename.error_input = Some(value);
                }
            }
        }
        cx.notify();
    }

    fn start_move_preview(
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
        let other_source_editor = from == self.current_rel && self.source_has_other_editor(cx);
        let state = self
            .session_directory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No recovery storage is available"))?;
        let guard = if (from != self.current_rel || self.editing.is_none()) && !other_source_editor
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
        let mut path = destination.to_path_buf();
        if path.extension().is_none() {
            path.set_extension("md");
        }
        let to = path
            .strip_prefix(&self.vault_root)
            .map_err(|_| anyhow::anyhow!("Choose a destination inside the open folder"))?
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Use a UTF-8 filename"))?
            .to_owned();
        MovePlan::prepare(&self.vault_root, Path::new(from), Path::new(&to))?;
        let destination_guard =
            FileEditor::reserve_destination(&path, &state.join("editor-drafts"))?;
        let root = self.vault_root.clone();
        let from = from.to_owned();
        let current = self.current_rel.clone();
        let was_editing = self.editing.is_some();
        let index = self.move_index.clone();
        let state = self.session_directory.clone();
        let cancel = reader_loading::Cancellation::default();
        let progress = Arc::new(std::sync::Mutex::new(String::from("Checking link index…")));
        let (confirm, confirmed) = async_channel::bounded(1);
        if index.is_none() {
            window.open_dialog(cx, move |dialog, _, _| {
                let yes = confirm.clone();
                let no = confirm.clone();
                dialog.title("Link index unavailable")
                    .child("Scan the folder to find links? You can cancel the scan before any file is changed.")
                    .on_close(move |_, _, _| { let _ = no.try_send(false); })
                    .footer(Button::new("scan-move-links").debug_selector(|| "scan-move-links".into()).label("Scan folder").on_click(move |_, window, cx| {
                        let _ = yes.try_send(true); window.close_dialog(cx);
                    }))
            });
        } else {
            let _ = confirm.try_send(true);
        }
        self.note_move_pending = true;
        cx.spawn_in(window, async move |this, cx| {
            if !confirmed.recv().await.unwrap_or(false) {
                let _ = this.update_in(cx, |this, _, cx| {
                    this.note_move_pending = false;
                    cx.notify();
                });
                return;
            }
            let (events, updates) = async_channel::unbounded();
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
                    &mut |phase, count| {
                        worker_cancel.check()?;
                        if count % 64 == 0 {
                            let _ = events.try_send(format!("{phase} · {count}"));
                        }
                        Ok(())
                    },
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
            let progress_display = progress.clone();
            let dialog_cancel = cancel.clone();
            let _ = this.update_in(cx, |_, window, cx| {
                window.open_dialog(cx, move |dialog, _, _| {
                    let close = dialog_cancel.clone();
                    let button = dialog_cancel.clone();
                    dialog
                        .title("Preparing move preview")
                        .overlay_closable(false)
                        .child(progress_display.lock().unwrap().clone())
                        .on_close(move |_, _, _| close.cancel())
                        .footer(Button::new("cancel-move-scan").label("Cancel").on_click(
                            move |_, window, cx| {
                                button.cancel();
                                window.close_dialog(cx);
                            },
                        ))
                });
            });
            while let Ok(phase) = updates.recv().await {
                *progress.lock().unwrap() = phase;
                let _ = this.update_in(cx, |_, window, _| window.refresh());
            }
            let result = worker.await;
            let answer = this
                .update_in(cx, |this, window, cx| {
                    if cancel.check().is_err() {
                        this.note_move_pending = false;
                        cx.notify();
                        return None;
                    }
                    window.close_dialog(cx);
                    let result = result.and_then(|links| {
                        anyhow::ensure!(
                            this.vault_root == root
                                && this.current_rel == current
                                && this.editing.is_some() == was_editing,
                            "The open note changed; preview again"
                        );
                        this.check_move_editors(&links.affected_paths(), cx)?;
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
                        Ok(pending) => Some(this.show_move_preview(pending, window, cx)),
                        Err(error) => {
                            this.note_move_pending = false;
                            this.link_notice = Some(format!("Cannot preview move: {error:#}"));
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
        window.open_dialog(cx, move |dialog, _, _| {
            let cancel = send.clone();
            let update = send.clone();
            let without = send.clone();
            let close = send.clone();
            let enter = send.clone();
            dialog
                .title("Rename / move note")
                .on_ok(move |_, _, _| {
                    let _ = enter.try_send(Some(true));
                    true
                })
                .width(px(760.))
                .overlay_closable(false)
                .on_close(move |_, _, _| {
                    let _ = close.try_send(None);
                })
                .child(div().child(format!("{} → {}", display.from, display.to)))
                .when(!display.skipped_files.is_empty(), |dialog| {
                    dialog.child(format!(
                        "{} files skipped — see details in the preview below.",
                        display.skipped_files.len()
                    ))
                })
                .child(
                    v_flex()
                        .id("move-link-preview")
                        .max_h(px(360.))
                        .overflow_y_scroll()
                        .gap_2()
                        .children(display.changes.iter().map(|c| {
                            div()
                                .child(format!("{}:{}  {} → {}", c.path, c.line, c.before, c.after))
                        }))
                        .when(!display.skipped_files.is_empty(), |view| {
                            view.child(format!("{} files skipped:", display.skipped_files.len()))
                                .children(display.skipped_files.iter().map(|file| {
                                    div().child(format!("{} — {}", file.path, file.reason))
                                }))
                        })
                        .child("Not updated:")
                        .children(display.skipped.iter().map(|s| {
                            div().child(format!(
                                "{}:{}  {} — {}",
                                s.path, s.line, s.target, s.reason
                            ))
                        })),
                )
                .footer(
                    v_flex()
                        .gap_2()
                        .child(
                            Button::new("move-update")
                                .primary()
                                .label(format!(
                                    "Move and update {} links in {} notes",
                                    display.changes.len(),
                                    display.changed_notes()
                                ))
                                .on_click(move |_, window, cx| {
                                    let _ = update.try_send(Some(true));
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("move-without")
                                .debug_selector(|| "move-without".into())
                                .label("Move without updating")
                                .on_click(move |_, window, cx| {
                                    let _ = without.try_send(Some(false));
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(Button::new("move-cancel").label("Cancel").on_click(
                            move |_, window, cx| {
                                let _ = cancel.try_send(None);
                                window.close_dialog(cx);
                            },
                        )),
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
            self.check_move_editors(&pending.links.affected_paths(), cx)?;
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
                self.link_notice = Some(format!("Cannot move: {error:#}"));
                cx.notify();
                return;
            }
        };
        self.note_move_pending = true;
        window.open_dialog(cx, |dialog, _, _| {
            dialog
                .title("Moving note…")
                .overlay_closable(false)
                .close_button(false)
                .keyboard(false)
                .child(
                    h_flex()
                        .gap_2()
                        .child(gpui_component::spinner::Spinner::new())
                        .child("Saving link updates and recovery copies. Please wait…"),
                )
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.and_then(|applied| {
                anyhow::ensure!(
                    applied.moved,
                    "{}",
                    applied
                        .warning
                        .unwrap_or_else(|| "Move interrupted; use Recover link moves".into())
                );
                Ok(tessera_core::note_move::Moved {
                    warning: applied.warning,
                })
            });
            let _ = this.update_in(cx, |this, window, cx| {
                this.note_move_pending = false;
                window.close_dialog(cx);
                this.complete_move(pending, update, result, window, cx);
            });
        })
        .detach();
    }

    fn complete_move(
        &mut self,
        pending: PendingMove,
        update: bool,
        result: anyhow::Result<tessera_core::note_move::Moved>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_move_editor(cx);
        match result {
            Err(error) => {
                self.sync_move_input(window, cx);
                self.link_notice = Some(format!("Cannot move: {error:#}"));
            }
            Ok(moved) => {
                let mut changes = tessera_core::Changes {
                    changed: std::collections::BTreeSet::from([pending.to.clone()]),
                    removed: std::collections::BTreeSet::from([pending.from.clone()]),
                    ..Default::default()
                };
                if update {
                    changes
                        .changed
                        .extend(pending.links.changes.iter().map(|c| c.path.clone()));
                }
                if self.vault_root == pending.root {
                    self.queue_vault_mutation(changes, cx);
                }
                self.tree.note_moved(&pending.from, &pending.to);
                drop(pending.guard);
                drop(pending.destination_guard);
                for path in &mut self.history {
                    if *path == pending.from {
                        *path = pending.to.clone();
                    }
                }
                for path in &mut self.sidebar.pinned {
                    if *path == pending.from {
                        *path = pending.to.clone();
                    }
                }
                for (path, _) in &mut self.sidebar.recent {
                    if *path == pending.from {
                        *path = pending.to.clone();
                    }
                }
                self.save_sidebar(cx);
                if self.current_rel != pending.from {
                    self.sync_move_input(window, cx);
                    self.link_notice = Some(moved.warning.unwrap_or_else(|| "Note moved.".into()));
                    cx.notify();
                    return;
                }
                self.editing = None;
                self.document_preparation_generation =
                    self.document_preparation_generation.wrapping_add(1);
                let document =
                    tessera_core::render::reader_document(&self.vault, &pending.to).map(|d| {
                        prepared_links::PreparedDocument {
                            source: d.rendered,
                            original: Some(d.original_body),
                            identities: d.links,
                            frontmatter: d.frontmatter,
                        }
                    });
                // Even a concurrent deletion/read failure must not leave the UI
                // pointing at the old path after a successful rename.
                self.current_rel = pending.to.clone();
                self.current_title = self.vault.note_title(&pending.to);
                self.accept_prepared_document(
                    prepared_links::DocumentRequest {
                        rel: pending.to,
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
                let notice = moved.warning.unwrap_or_else(|| {
                    if update {
                        format!(
                            "Note moved. Updated {} links in {} notes.",
                            pending.links.changes.len(),
                            pending.links.changed_notes()
                        )
                    } else {
                        "Note moved. Link text was not changed.".into()
                    }
                });
                self.link_notice = Some(match self.link_notice.take() {
                    Some(error) => format!("{notice} {error}"),
                    None => notice,
                });
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
                this.link_notice = Some(match this.revert_link_move(&path, &state, window, cx) {
                    Ok(()) => "Link move reverted. Original bytes restored.".into(),
                    Err(error) => format!(
                        "Recovery stopped: {error:#}. Original bytes are retained; resolve the reported file and try again."
                    ),
                });
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
        // A real click opens the note but leaves focus in the tree, including
        // after asynchronous document replacement, at docked and overlay widths.
        for width in [1400., 800.] {
            visual.simulate_resize(size(px(width), px(860.)));
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
                    assert!(
                        r.tree_focus.is_focused(window),
                        "tree focus after click and load"
                    );
                    assert!(!r.content.read(cx).focus_handle().is_focused(window));
                });
                visual.simulate_keystrokes(key);
                reader.read_with(visual, |r, _| {
                    assert!(r.renaming.is_some(), "{key} after click")
                });
                visual.simulate_keystrokes("escape");
            }
        }
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
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
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
            assert!(reader
                .link_notice
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
        let bounds = visual.debug_bounds("move-without").unwrap();
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
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
                "Move button did not finish confirmation; bounds={bounds:?}, notice={:?}",
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
            "[[start]]"
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
            assert!(
                root.join("Final.md").exists(),
                "pending={} renaming={} notice={:?}",
                reader.note_move_pending,
                reader.renaming.is_some(),
                reader.link_notice
            );
        });
        assert_eq!(
            std::fs::read_to_string(root.join("target.md")).unwrap(),
            "[[Final#Heading|alias]]"
        );
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "Final.md");
            assert!(reader.editing.is_some());
        });
        let move_index = reader.read_with(visual, |reader, _| reader.move_index.clone());
        // A missing index never silently starts a full-vault source scan.
        reader.update_in(visual, |reader, window, cx| {
            reader.move_index = None;
            reader.rename_note(window, cx);
        });
        reader.update_in(visual, |reader, window, cx| {
            let input = reader.renaming.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("Fallback.md", window, cx));
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
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("scan-move-links").is_some());
        assert!(root.join("Final.md").exists());
        assert!(!root.join("Fallback.md").exists());
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| assert!(!reader.note_move_pending));
        assert!(root.join("Final.md").exists());
        assert!(!root.join("Fallback.md").exists());
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
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(root.join("Reference.md").exists());
        assert!(!root.join("target.md").exists());
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "Final.md");
            assert!(reader.editing.is_some());
        });
        std::fs::remove_dir_all(dir).unwrap();
    }
}
