//! Project routing for otherwise generic attachments. Dedicated media/log/archive
//! viewers retain priority; source/data viewers resolve associations themselves.
use super::*;

impl Reader {
    pub(super) fn resolve_json_viewer(
        &self,
        tree_had_focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = &self.file_preview else {
            return;
        };
        if preview.code.is_some()
            || preview.image
            || preview.pdf.is_some()
            || preview.log.is_some()
            || preview.virtual_log.is_some()
            || preview.har.is_some()
            || preview.archive.is_some()
            || preview.audio.is_some()
            || preview.canvas.is_some()
            || preview.drawing.is_some()
            || reader_media::is_video(&preview.path)
        {
            return;
        }
        let (root, path) = (self.vault_root.clone(), preview.path.clone());
        let identity = preview.live_identity.clone();
        let task = cx
            .background_executor()
            .spawn(async move { okilum_core::json_schema::associate(&root, &path) });
        cx.spawn_in(window, async move |this, cx| {
            let association = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(preview) = this
                    .file_preview
                    .as_mut()
                    .filter(|p| Arc::ptr_eq(&p.live_identity, &identity))
                else {
                    return;
                };
                if let Err(error) = &association {
                    reader_toast::error(
                        format!("Project viewer associations unavailable: {error}"),
                        window,
                        cx,
                    );
                    return;
                }
                if !association.as_ref().is_ok_and(|a| a.json) {
                    return;
                }
                let (root, rel, path) = (
                    this.vault_root.clone(),
                    preview.rel.clone(),
                    preview.path.clone(),
                );
                preview.text = None;
                #[cfg(target_os = "macos")]
                {
                    preview.thumbnail = None;
                }
                preview.code = Some(cx.new(|cx| {
                    reader_code_file::CodePreview::new_associated(
                        root,
                        rel,
                        path,
                        Some(association),
                        window,
                        cx,
                    )
                    .without_header()
                }));
                this.reopen_chosen_source(tree_had_focus, window, cx);
                cx.notify();
            });
        })
        .detach();
    }
}
