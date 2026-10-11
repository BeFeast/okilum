//! UI workers retain displayed evidence and delegate every write to FileEditor.
use super::*;
use gpui_component::notification::Notification;
use okilum_core::{
    task_edit::Change,
    tasks::{Index, Task},
};

use okilum_core::task_edit::{self, write::Receipt};

impl Reader {
    pub(super) fn apply_task_change(
        &mut self,
        index: Arc<Index>,
        task: Task,
        change: Change,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(directory) = self.session_directory.clone() else {
            reader_toast::error("Task recovery storage is unavailable.", window, cx);
            return;
        };
        let root = self.vault_root.clone();
        let drafts = directory.join("editor-drafts");
        cx.spawn_in(window, async move |reader, cx| {
            let result =
                cx.background_executor()
                    .spawn({
                        let root = root.clone();
                        let drafts = drafts.clone();
                        async move {
                            task_edit::write::apply_indexed(&root, &drafts, &index, &task, change)
                        }
                    })
                    .await;
            let _ = reader.update_in(cx, |_, window, cx| match result {
                Ok(Some(receipt)) => {
                    let weak = cx.weak_entity();
                    reader_toast::push(
                        Notification::new()
                            .message("Task updated")
                            .content(move |_, _, _| {
                                let reader = weak.clone();
                                let receipt = receipt.clone();
                                let root = root.clone();
                                let drafts = drafts.clone();
                                Button::new("task-undo")
                                    .debug_selector(|| "task-undo".into())
                                    .small()
                                    .ghost()
                                    .label("Undo")
                                    .on_click(move |_, window, cx| {
                                        let _ = reader.update(cx, |this, cx| {
                                            this.undo_task(
                                                receipt.clone(),
                                                root.clone(),
                                                drafts.clone(),
                                                window,
                                                cx,
                                            )
                                        });
                                    })
                                    .into_any_element()
                            }),
                        Some(Duration::from_secs(8)),
                        window,
                        cx,
                    );
                }
                Ok(None) => {}
                Err(error) => reader_toast::error(format!("Task unchanged: {error}"), window, cx),
            });
        })
        .detach();
    }

    fn undo_task(
        &mut self,
        receipt: Receipt,
        root: PathBuf,
        drafts: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |reader, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receipt.undo(&root, &drafts) })
                .await;
            let _ = reader.update_in(cx, |_, window, cx| match result {
                Ok(()) => reader_toast::transient("Task change undone", window, cx),
                Err(error) => {
                    reader_toast::error(format!("Cannot undo task change: {error}"), window, cx)
                }
            });
        })
        .detach();
    }
}
