//! Mode-independent confirmation and identity-bound navigation continuation.
use super::*;

impl Reader {
    /// Native window close must offer the same choices as leaving this note.
    /// The callback is synchronous; a chosen save/discard resumes it later.
    pub(crate) fn prepare_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // After each choice the close is attempted again, so every pane with
        // a deleted note is asked in turn before the window goes (#773).
        if self.ask_for_deleted_pane(
            Box::new(|this, window, cx| {
                if this.prepare_window_close(window, cx) {
                    window.remove_window();
                }
            }),
            window,
            cx,
        ) {
            return false;
        }
        self.finish_window_close(cx)
    }

    /// The first pane of this window whose deleted note would be left
    /// behind, asked about through the ordinary Save As / Don't Save dialog.
    /// An inactive pane is focused first, so the dialog is about the note on
    /// screen. Returns whether a dialog is now open (or already was).
    pub(crate) fn ask_for_deleted_pane(
        &mut self,
        retry: Continue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.deleted_note_asking {
            return true;
        }
        let needs = self.for_each_pane(cx, |this, cx| {
            this.leaving_deleted_note(None, cx).then(|| this.pane.id())
        });
        let Some(pane) = needs.into_iter().flatten().next() else {
            return false;
        };
        self.focus_pane(pane, window, cx);
        self.ask_leaving_deleted_note(retry, window, cx);
        true
    }

    fn finish_window_close(&mut self, cx: &mut Context<Self>) -> bool {
        // Every pane of the window, the inactive one too (#773): an editor
        // writes nothing when it is dropped with its window.
        let saved = self.for_each_pane(cx, |this, cx| this.save_source_for_close(cx));
        if !saved.into_iter().all(|saved| saved) {
            return false;
        }
        self.record_ui_state(self.ui_state.was_active(), cx);
        reader_ui_state::flush(cx);
        true
    }

    /// The person chose Save As / Don't Save for the deleted note on screen,
    /// and the navigation they asked for is under way.
    pub(crate) fn left_deleted_note(&self) -> bool {
        self.pane
            .deleted_note_leaving
            .as_ref()
            .is_some_and(|(root, rel, generation)| {
                root == &self.vault_root
                    && rel == &self.pane.current_rel
                    && *generation == self.pane.navigation.generation
            })
    }

    /// Navigation away from the note on screen (`target`, or another vault
    /// with `None`). Every presentation mode asks before leaving. The caller
    /// hands its navigation to `ask_leaving_deleted_note`.
    pub(crate) fn leaving_deleted_note(
        &mut self,
        target: Option<&str>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.left_deleted_note() {
            return false;
        }
        // Navigation can beat the debounced watcher notification.
        self.track_deleted_note(&Default::default(), cx);
        if self.pane.deleted_note.is_some() && self.deleted_current().is_none() {
            self.pane.deleted_note = None;
        }
        self.pane.deleted_note.is_some() && target != Some(self.pane.current_rel.as_str())
    }

    /// Ask before a deleted note's contents are left behind; `retry` runs the
    /// navigation once the person has decided to save them or not.
    pub(crate) fn ask_leaving_deleted_note(
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
        let rel = self.pane.current_rel.clone();
        let generation = self.pane.navigation.preparation_generation;
        cx.spawn_in(window, async move |this, cx| {
            let choice = answer.recv().await.unwrap_or(Leave::Cancel);
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                this.deleted_note_asking = false;
                let current = this.vault_root == root
                    && this.pane.current_rel == rel
                    && this.pane.navigation.preparation_generation == generation;
                if !current {
                    return;
                }
                match choice {
                    Leave::Cancel => {}
                    Leave::DontSave => {
                        let had_editor = this.pane.editing.is_some();
                        if !this.release_deleted_editor(cx) {
                            return;
                        }
                        if had_editor {
                            reader_toast::transient("Draft kept in recovery. Use More → Recover notes… to save it later.", window, cx);
                        }
                        this.pane.deleted_note = None;
                        this.continue_deleted_navigation(retry, window, cx);
                    }
                    Leave::SaveAs if current && this.pane.deleted_note.is_some() => {
                        this.save_deleted_note_as(Some(retry), window, cx);
                    }
                    Leave::SaveAs => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The dialog's explicit choice authorizes exactly this continuation. A
    /// missing path must not immediately ask the same question again.
    pub(crate) fn continue_deleted_navigation(
        &mut self,
        retry: Continue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pane.deleted_note_leaving = Some((
            self.vault_root.clone(),
            self.pane.current_rel.clone(),
            self.pane.navigation.generation,
        ));
        retry(self, window, cx);
    }
}
