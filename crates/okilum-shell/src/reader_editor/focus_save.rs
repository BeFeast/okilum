//! Focus loss requests canonical publication and immediately protects recovery,
//! including when conflict, IME or another save prevents publication right now.
use super::*;

impl Reader {
    pub(super) fn flush_focus_recovery(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self
            .editing
            .as_mut()
            .filter(|e| e.store.has_pending_revision())
        else {
            return;
        };
        let write = editing
            .recovery_pending
            .as_ref()
            .and_then(|pending| pending.ticket().borrow_mut().take())
            .unwrap_or_else(|| editing.store.recovery_write());
        editing.recovery_pending = None;
        editing.protecting = true;
        let input = editing.input.entity_id();
        let epoch = editing.recovery_epoch;
        cx.spawn(async move |reader, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { write.persist() })
                .await;
            let _ = reader.update(cx, |reader, cx| {
                let Some(editing) = reader
                    .editing
                    .as_mut()
                    .filter(|e| e.input.entity_id() == input && e.recovery_epoch == epoch)
                else {
                    return;
                };
                editing.protecting = false;
                if let Err(error) = result {
                    editing.save_failed = true;
                    reader.link_notice = Some(DRAFT_WRITE_NOTICE.into());
                    diagnostic_error!("Focus-loss recovery could not be saved: {error:#}");
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn focus_save_ready(&self, window: &Window, cx: &App) -> bool {
        !self.move_applying
            && self.deleted_current().is_none()
            && self.editing.as_ref().is_some_and(|e| {
                e.focus_save_pending
                    && e.autosaving.is_none()
                    && !e.conflict_detected
                    && !e.input.read(cx).ime_composing()
                    && (!window.is_window_active() || !e.input.focus_handle(cx).is_focused(window))
            })
    }

    pub(super) fn queue_focus_save_followup(&mut self, input: EntityId, cx: &mut Context<Self>) {
        let handle = self.reader_window;
        cx.spawn(async move |reader, cx| {
            let _ = handle.update(cx, |_, window, cx| {
                let _ = reader.update(cx, |reader, cx| {
                    if reader
                        .editing
                        .as_ref()
                        .is_some_and(|e| e.input.entity_id() == input)
                    {
                        reader.save_source_in_background(window, cx);
                    }
                });
            });
        })
        .detach();
    }
}
