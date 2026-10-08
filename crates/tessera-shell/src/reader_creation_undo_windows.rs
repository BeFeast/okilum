//! Windows Undo is restricted to the unchanged item just created in this window.
use super::*;
impl Reader {
    pub(super) fn delete_created_path(
        &mut self,
        item: Arc<reader_create::CreatedUndo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.trash_pending
            || self.note_move_pending
            || self.source_is_dirty(cx)
            || self.vault_root != item.root
        {
            reader_toast::transient(
                "Finish editing before undoing creation; your item was kept",
                window,
                cx,
            );
            return;
        }
        let Some(state) = self.session_directory.clone() else {
            return;
        };
        if !self.save_source(cx) {
            return;
        }
        let own = self.selected_file() == item.relative;
        let held_editor = if own { self.editing.take() } else { None };
        let has_editor = held_editor.is_some();
        self.trash_pending = true;
        cx.spawn_in(window, async move |this, cx| {
            let target = item.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let _lock = if !has_editor && target.source.is_some() {
                        Some(tessera_core::file_editor::FileEditor::reserve_destination(
                            &target.root.join(&target.relative),
                            &state.join("editor-drafts"),
                        )?)
                    } else {
                        None
                    };
                    tessera_core::windows_files::undo_created(
                        &target.root.join(&target.relative),
                        target.identity,
                        target.source.as_deref().map(str::as_bytes),
                    )
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.trash_pending = false;
                match result {
                    Ok(()) => {
                        this.invalidate_creation_undo(window, cx);
                        if this.vault_root == item.root {
                            let mut changes = tessera_core::Changes::default();
                            if item.source.is_some() {
                                changes.removed.insert(item.relative.clone());
                            } else {
                                changes.directories.insert(item.relative.clone());
                            }
                            this.queue_vault_mutation(changes, cx);
                            if own && this.selected_file() == item.relative {
                                this.show_empty_vault(window, cx);
                            }
                        }
                        reader_toast::transient("Creation undone", window, cx);
                    }
                    Err(error) => {
                        if own
                            && this.vault_root == item.root
                            && this.selected_file() == item.relative
                        {
                            this.editing = held_editor;
                        }
                        reader_toast::transient(
                            format!("Cannot undo creation: {error:#}"),
                            window,
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
