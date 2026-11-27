//! Reader glue for whole-file copy (#3238).
use super::*;
pub(crate) use okilum_shell_viewers::reader_file_copy::*;

impl Reader {
    pub(crate) fn copy_file_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !reader_viewer_chrome::copy_content_available(self) {
            return;
        }
        if let Some(editing) = self.pane.editing.as_ref() {
            if editing.input().read(cx).value().len() as u64 > LIMIT {
                reader_toast::error("File content exceeds the 64 MB copy limit", window, cx);
                return;
            }
            if let Some(text) = self.source_text(cx) {
                copy(text, window, cx);
            }
            return;
        }
        let root = self.vault_root.clone();
        let rel = self.selected_file().to_owned();
        let read = cx
            .background_executor()
            .spawn(async move { read_text(&root, &rel) });
        window
            .spawn(cx, async move |cx| {
                let result = read.await;
                let _ = cx.update(|window, cx| match result {
                    Ok(text) => copy(text, window, cx),
                    Err(error) => reader_toast::error(
                        format!("Cannot copy file content: {error}"),
                        window,
                        cx,
                    ),
                });
            })
            .detach();
    }
}
