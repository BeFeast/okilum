//! Contextual Board instructions; available before and after Board creation.
use crate::*;
use gpui_component::{text::TextView, WindowExt};

const GUIDE: &str = include_str!("../../../../../docs/help/board/setup.md");
const FILES: &[(&str, &[u8])] = &[
    (
        "assets/Manrope.ttf",
        include_bytes!("../../../../../docs/help/board/assets/Manrope.ttf"),
    ),
    (
        "assets/OFL.txt",
        include_bytes!("../../../../../docs/help/board/assets/OFL.txt"),
    ),
    (
        "index.html",
        include_bytes!("../../../../../docs/help/board/index.html"),
    ),
    (
        "captions.vtt",
        include_bytes!("../../../../../docs/help/board/captions.vtt"),
    ),
    (
        "assets/board-tutorial.mp4",
        include_bytes!("../../../../../docs/help/board/assets/board-tutorial.mp4"),
    ),
    (
        "assets/board-saved.png",
        include_bytes!("../../../../../docs/help/board/assets/board-saved.png"),
    ),
];

pub(super) fn entry() -> Button {
    Button::new("board-help")
        .debug_selector(|| "board-help".into())
        .label("Help")
        .tooltip("Board help")
        .accessibility_label("Board help")
        .ghost()
        .small()
        .on_click(|_, window, cx| {
            let view = cx.new(|_| Help {
                busy: false,
                failed: false,
            });
            window.open_sheet(cx, move |sheet, _, _| {
                sheet
                    .title("Board help")
                    .size(px(560.))
                    .resizable(false)
                    .child(view.clone())
            });
        })
}

struct Help {
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
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let state = reader_history::state_directory()?;
                    let file = crate::reader_help_assets::publish(&state, "board", FILES)?;
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
        let mut content =
            v_flex()
                .id("board-help-content")
                .debug_selector(|| "board-help-content".into())
                .size_full()
                .gap_4()
                .overflow_y_scroll()
                .child(
                    TextView::markdown("board-help-written-guide", GUIDE)
                        .selectable(true)
                        .w_full(),
                )
                .child(
                    Button::new("board-help-copy")
                        .label("Copy written guide")
                        .ghost()
                        .on_click(|_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(GUIDE.to_owned()))
                        }),
                )
                .child(
                    Button::new("board-help-video")
                        .label(if self.busy {
                            "Preparing local guide…"
                        } else {
                            "Open local video guide"
                        })
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| this.open_video(cx))),
                )
                .child(div().text_sm().child(
                    "Opens the bundled video and captions in your browser. Available offline.",
                ));
        if self.failed {
            content = content.child(div().text_sm().child("Could not open the local video guide. The written steps above remain available; you can copy them."));
        }
        content
    }
}
