//! Contextual offline Help sheets: the written guide in the app, plus a bundled
//! video page opened from an immutable local copy. Reading Help needs no
//! vault, repository, Manager or network access.
use crate::*;
use gpui_component::{text::TextView, WindowExt};

/// One bundled guide. Selectors are static so UI tests can click real controls.
pub(crate) struct Guide {
    /// Help bundle directory name under the state directory.
    pub(crate) topic: &'static str,
    /// Sheet title, tooltip and accessible name of the entry button.
    pub(crate) title: &'static str,
    pub(crate) entry_id: &'static str,
    pub(crate) content_id: &'static str,
    pub(crate) text_id: &'static str,
    pub(crate) copy_id: &'static str,
    pub(crate) video_id: &'static str,
    pub(crate) markdown: &'static str,
    pub(crate) files: &'static [(&'static str, &'static [u8])],
}

pub(crate) fn entry(guide: &'static Guide) -> Button {
    Button::new(guide.entry_id)
        .debug_selector(move || guide.entry_id.into())
        .label("Help")
        .tooltip(guide.title)
        .accessibility_label(guide.title)
        .ghost()
        .small()
        .on_click(move |_, window, cx| {
            let view = cx.new(|_| Help {
                guide,
                busy: false,
                failed: false,
            });
            window.open_sheet(cx, move |sheet, _, _| {
                sheet
                    .title(guide.title)
                    .size(px(560.))
                    .resizable(false)
                    .child(view.clone())
            });
        })
}

struct Help {
    guide: &'static Guide,
    busy: bool,
    failed: bool,
}
impl Help {
    fn open_video(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.failed = false;
        cx.notify();
        let guide = self.guide;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let state = reader_history::state_directory()?;
                    let file =
                        crate::reader_help_assets::publish(&state, guide.topic, guide.files)?;
                    let mut url = url::Url::from_file_path(file)
                        .map_err(|_| anyhow::anyhow!("Local Help path cannot be opened"))?;
                    url.set_fragment(Some("watch"));
                    Ok::<_, anyhow::Error>(url.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(url) => cx.open_url(&url),
                    Err(_) => this.failed = true,
                }
                cx.notify();
            });
        })
        .detach();
    }
}
impl Render for Help {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let guide = self.guide;
        let mut content = v_flex()
            .id(guide.content_id)
            .debug_selector(move || guide.content_id.into())
            .size_full()
            .gap_4()
            .overflow_y_scroll()
            .child(
                TextView::markdown(guide.text_id, guide.markdown)
                    .selectable(true)
                    .w_full(),
            )
            .child(
                Button::new(guide.copy_id)
                    .label("Copy written guide")
                    .ghost()
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(guide.markdown.to_owned()))
                    }),
            )
            .child(
                Button::new(guide.video_id)
                    .label(if self.busy {
                        "Preparing local guide…"
                    } else {
                        "Open local video guide"
                    })
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.open_video(cx))),
            )
            .child(
                div().text_sm().child(
                    "Opens the bundled video and captions in your browser. Available offline.",
                ),
            );
        if self.failed {
            content = content.child(div().text_sm().child("Could not open the local video guide. The written steps above remain available; you can copy them."));
        }
        content
    }
}
