//! Explicit feedback drafts. Attachments are chosen by the user; nothing is
//! captured or transmitted until Send. Failed reports survive dialog dismissal.
use super::*;
use gpui_component::{
    checkbox::Checkbox,
    input::{Paste, Textarea, TextareaState},
    WindowExt,
};
mod model;
use model::{Kind, Metadata, Receipt, Report};

gpui::actions!(okilum_feedback, [SendFeedback]);
#[derive(Default)]
struct Drafts(std::collections::HashMap<WindowId, Entity<FeedbackView>>);
impl Global for Drafts {}

pub(crate) fn install(cx: &mut App) {
    cx.set_global(Drafts::default());
    cx.bind_keys([KeyBinding::new(
        "secondary-v",
        Paste,
        Some("OkilumFeedback"),
    )]);
    cx.on_action(|_: &SendFeedback, cx| {
        let Some(handle) = cx.active_window() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| show(window, cx));
        });
    });
}
pub(crate) fn button(cx: &App) -> impl IntoElement {
    reader_icon_button(
        "reader-feedback",
        Icon::default().path("icons/message-square.svg"),
        "Send feedback",
        cx,
    )
    .debug_selector(|| "reader-feedback".into())
    .on_click(|_, window, cx| show(window, cx))
}
fn metadata(window: &Window) -> Metadata {
    Metadata {
        version: env!("OKILUM_RELEASE_VERSION").into(),
        build: env!("OKILUM_BUILD_VERSION").into(),
        channel: updater::channel().to_ascii_lowercase(),
        source: env!("OKILUM_SOURCE_COMMIT").into(),
        os: std::env::consts::OS.into(),
        os_version: version_info::os(),
        distro: cfg!(target_os = "linux").then(version_info::os),
        xdg_session_type: cfg!(target_os = "linux")
            .then(|| std::env::var("XDG_SESSION_TYPE").ok())
            .flatten(),
        desktop: cfg!(target_os = "linux")
            .then(|| std::env::var("XDG_CURRENT_DESKTOP").ok())
            .flatten(),
        arch: std::env::consts::ARCH.into(),
        scale: window.scale_factor(),
        window_width: window.bounds().size.width.into(),
        window_height: window.bounds().size.height.into(),
        decorations: match window.window_decorations() {
            Decorations::Server => "SSD",
            Decorations::Client { .. } => "CSD",
        }
        .into(),
    }
}
fn show(window: &mut Window, cx: &mut App) {
    cx.default_global::<Drafts>();
    let alive = cx
        .windows()
        .into_iter()
        .map(|h| h.window_id())
        .collect::<std::collections::HashSet<_>>();
    cx.global_mut::<Drafts>()
        .0
        .retain(|id, _| alive.contains(id));
    let id = window.window_handle().window_id();
    let previous = cx
        .global::<Drafts>()
        .0
        .get(&id)
        .cloned()
        .filter(|view| !matches!(view.read(cx).state, SendState::Sent(_)));
    let view = previous.unwrap_or_else(|| {
        let view = cx.new(|cx| FeedbackView::new(metadata(window), window, cx));
        cx.global_mut::<Drafts>().0.insert(id, view.clone());
        view
    });
    if view.read(cx).visible && window.has_active_dialog(cx) {
        return;
    }
    view.update(cx, |view, _| view.visible = true);
    let focus = Focusable::focus_handle(view.read(cx).text.read(cx), cx);
    window.open_dialog(cx, move |dialog, window, _| {
        let close = view.clone();
        let cancel = view.clone();
        dialog
            .title("Send feedback")
            .width(px(560.).min((window.viewport_size().width - px(48.)).max(px(280.))))
            .max_h((window.viewport_size().height - px(48.)).max(px(240.)))
            .overlay_closable(false)
            .on_ok(|_, _, _| false)
            .on_cancel(move |_, _, cx| {
                cancel.update(cx, |view, _| view.visible = false);
                true
            })
            .on_close(move |_, _, cx| close.update(cx, |view, _| view.visible = false))
            .child(view.clone())
    });
    // open_dialog first focuses its own container. Move focus to the field
    // immediately afterwards so the first Paste belongs to this report.
    focus.focus(window, cx);
}
fn screenshot_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Screenshot a window: ⇧⌘4, then Space. Paste or drop the image here."
    } else if cfg!(windows) {
        "Screenshot: Win+Shift+S. Paste or drop the image here."
    } else {
        "GNOME screenshot: PrtSc. Paste or drop the image here."
    }
}

enum SendState {
    Compose,
    Sending,
    Failed(String),
    Sent(Receipt),
}
struct Attachment {
    png: Arc<Vec<u8>>,
    preview: Arc<RenderImage>,
}
enum ImageInput {
    File(PathBuf),
    Clipboard(Vec<u8>),
}
type Sender = Arc<dyn Fn(Report) -> anyhow::Result<Receipt> + Send + Sync>;
struct FeedbackView {
    text: Entity<TextareaState>,
    meta: Metadata,
    kind: Kind,
    visible: bool,
    image: Option<Attachment>,
    image_error: Option<String>,
    attaching: bool,
    image_generation: u64,
    include_log: bool,
    log_path: Option<PathBuf>,
    state: SendState,
    sender: Sender,
    _send: Option<Task<()>>,
    _image: Option<Task<()>>,
    _text: Subscription,
}
impl FeedbackView {
    fn new(meta: Metadata, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .placeholder("What happened, or what would you like to see?")
        });
        let subscription = cx.subscribe(
            &text,
            |_, _, event: &gpui_component::input::InputEvent, cx| {
                if matches!(event, gpui_component::input::InputEvent::Change) {
                    cx.notify();
                }
            },
        );
        Self {
            text,
            meta,
            kind: Kind::Bug,
            visible: false,
            image: None,
            image_error: None,
            attaching: false,
            image_generation: 0,
            include_log: false,
            log_path: reader_diagnostics::feedback_log_path(cx).ok(),
            state: SendState::Compose,
            sender: Arc::new(model::send),
            _send: None,
            _image: None,
            _text: subscription,
        }
    }
    fn busy(&self) -> bool {
        matches!(self.state, SendState::Sending | SendState::Sent(_))
    }
    fn report(&self, cx: &App) -> anyhow::Result<Report> {
        Ok(Report {
            text: self.text.read(cx).value().to_string(),
            kind: self.kind,
            meta: self.meta.clone(),
            screenshot: self.image.as_ref().map(|image| image.png.clone()),
            log: if self.include_log {
                Some(model::log_tail(self.log_path.as_deref().ok_or_else(|| anyhow::anyhow!("Diagnostic log unavailable. Uncheck the log attachment to send without it."))?)?)
            } else {
                None
            },
        })
    }
    fn send(&mut self, cx: &mut Context<Self>) {
        if self.busy() || self.attaching {
            return;
        }
        let report = match self.report(cx) {
            Ok(report) => report,
            Err(error) => {
                self.state = SendState::Failed(error.to_string());
                cx.notify();
                return;
            }
        };
        self.state = SendState::Sending;
        let sender = self.sender.clone();
        self._send = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { sender(report) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(receipt) => SendState::Sent(receipt),
                    Err(error) => SendState::Failed(error.to_string()),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn copy(&mut self, cx: &mut Context<Self>) {
        match self.report(cx) {
            Ok(report) => cx.write_to_clipboard(ClipboardItem::new_string(report.copy_text())),
            Err(error) => {
                self.state = SendState::Failed(error.to_string());
                cx.notify();
            }
        }
    }
    fn image_input(&mut self, input: ImageInput, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.image_generation += 1;
        let generation = self.image_generation;
        self.attaching = true;
        self.image_error = None;
        self._image = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match input {
                        ImageInput::File(path) => model::image_file(&path),
                        ImageInput::Clipboard(bytes) => model::image_bytes(&bytes, true),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if generation != this.image_generation {
                    return;
                }
                this.attaching = false;
                match result {
                    Ok((png, preview)) => {
                        this.image = Some(Attachment {
                            png,
                            preview: Arc::new(RenderImage::new(vec![image::Frame::new(preview)])),
                        })
                    }
                    Err(error) => this.image_error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn remove_image(&mut self, cx: &mut Context<Self>) {
        self.image_generation += 1;
        self._image = None;
        self.attaching = false;
        self.image = None;
        self.image_error = None;
        cx.notify();
    }
    fn picker(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let result = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Attach PNG or JPEG screenshot".into()),
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = result.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.image_input(ImageInput::File(path), cx);
                    }
                }
                Ok(Ok(None)) => {}
                _ => {
                    this.image_error = Some(
                        "Couldn't open the image picker. Paste or drop a screenshot instead."
                            .into(),
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }
    fn paste_image(&mut self, cx: &mut Context<Self>) -> bool {
        if self.busy() {
            return false;
        }
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        for entry in item.into_entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    self.image_input(ImageInput::Clipboard(image.bytes), cx);
                    return true;
                }
                ClipboardEntry::ExternalPaths(paths) if paths.0.len() == 1 => {
                    self.image_input(ImageInput::File(paths.0[0].clone()), cx);
                    return true;
                }
                _ => {}
            }
        }
        false
    }
}
impl Render for FeedbackView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy();
        let p = brand::palette(cx);
        let mut body = v_flex()
            .id("feedback-dialog")
            .debug_selector(|| "feedback-dialog".into())
            .key_context("OkilumFeedback")
            .gap_3()
            .w_full()
            .max_h((window.viewport_size().height - px(160.)).max(px(160.)))
            .overflow_y_scroll()
            .capture_action(cx.listener(|this, _: &Paste, _, cx| {
                if !this.paste_image(cx) {
                    cx.propagate();
                }
            }))
            .drag_over::<ExternalPaths>(|style, _, _, cx| {
                style.bg(brand::palette(cx).selected.opacity(0.2))
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if this.busy() {
                    return;
                }
                if paths.0.len() == 1 {
                    this.image_input(ImageInput::File(paths.0[0].clone()), cx);
                } else {
                    this.image_error = Some("Attach one PNG or JPEG screenshot at a time.".into());
                    cx.notify();
                }
            }));
        if let SendState::Sent(receipt) = &self.state {
            let url = receipt.issue_url.clone();
            return body
                .child("Thank you — feedback sent.")
                .child(
                    h_flex().child(
                        div()
                            .id("feedback-issue")
                            .debug_selector(|| "feedback-issue".into())
                            .cursor_pointer()
                            .hover(|row| row.underline())
                            .child(format!("Open issue #{} ↗", receipt.issue_number))
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
                )
                .into_any_element();
        }
        body = body
            .child(h_flex().gap_1().children(
                [(Kind::Bug, "Problem"), (Kind::Feature, "Idea")].map(|(kind, label)| {
                    Button::new(label)
                        .small()
                        .ghost()
                        .selected(self.kind == kind)
                        .disabled(busy)
                        .label(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.kind = kind;
                            cx.notify();
                        }))
                }),
            ))
            .child(Textarea::new(&self.text).disabled(busy))
            .child(
                div()
                    .debug_selector(|| "feedback-metadata".into())
                    .text_sm()
                    .text_color(p.text_muted)
                    .child(self.meta.summary()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("feedback-attach")
                            .debug_selector(|| "feedback-attach".into())
                            .small()
                            .ghost()
                            .disabled(busy)
                            .label("Attach screenshot…")
                            .on_click(cx.listener(|this, _, _, cx| this.picker(cx))),
                    )
                    .when(self.image.is_some() || self.attaching, |row| {
                        row.child(
                            Button::new("feedback-remove")
                                .debug_selector(|| "feedback-remove".into())
                                .small()
                                .ghost()
                                .disabled(busy)
                                .label("Remove")
                                .on_click(cx.listener(|this, _, _, cx| this.remove_image(cx))),
                        )
                    }),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(p.text_muted)
                    .child(screenshot_hint()),
            );
        if let Some(image) = &self.image {
            body = body.child(
                div()
                    .debug_selector(|| "feedback-preview".into())
                    .w_full()
                    .child(
                        img(image.preview.clone())
                            .w_full()
                            .max_h(px(160.))
                            .object_fit(ObjectFit::Contain),
                    ),
            );
        }
        if self.attaching {
            body = body.child(div().text_sm().child("Preparing screenshot…"));
        }
        if let Some(error) = &self.image_error {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(p.text_muted)
                    .child(error.clone()),
            );
        }
        body = body
            .child(
                Checkbox::new("feedback-log")
                    .debug_selector(|| "feedback-log".into())
                    .label("Attach last 250 log lines")
                    .checked(self.include_log)
                    .disabled(busy)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.include_log = *checked;
                        cx.notify();
                    })),
            )
            .child(div().text_sm().text_color(p.text_muted).child(
                "The log names your vault folder and opened files. Review it before sharing.",
            ));
        if let SendState::Failed(error) = &self.state {
            body = body.child(
                div()
                    .debug_selector(|| "feedback-error".into())
                    .text_sm()
                    .child(error.clone()),
            );
        }
        let label = match self.state {
            SendState::Sending => "Sending…",
            SendState::Failed(_) => "Retry",
            _ => "Send",
        };
        body.child(
            h_flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("feedback-copy")
                        .debug_selector(|| "feedback-copy".into())
                        .ghost()
                        .disabled(busy)
                        .label("Copy report")
                        .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
                )
                .child(
                    Button::new("feedback-send")
                        .debug_selector(|| "feedback-send".into())
                        .primary()
                        .disabled(
                            busy || self.attaching || self.text.read(cx).value().trim().is_empty(),
                        )
                        .label(label)
                        .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                ),
        )
        .into_any_element()
    }
}
#[cfg(test)]
#[path = "reader_feedback/ui_tests.rs"]
mod ui_tests;
