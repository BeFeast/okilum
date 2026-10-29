//! Explicit feedback drafts. Attachments are chosen by the user; nothing is
//! captured or transmitted until Send. Failed reports survive closing the independent Feedback window.
use super::*;
use gpui_component::{
    checkbox::Checkbox,
    input::{Paste, Textarea, TextareaState},
};
mod model;
pub(crate) mod sprint;
use model::{Kind, Metadata, Receipt, Report};

gpui::actions!(
    okilum_feedback,
    [SendFeedback, CloseFeedback, SendSprintReport]
);
#[derive(Default)]
struct Drafts(std::collections::HashMap<WindowId, Draft>);
struct Draft {
    view: Entity<FeedbackView>,
    window: WindowHandle<Root>,
}
impl Global for Drafts {}

pub(crate) fn install(cx: &mut App) {
    cx.set_global(Drafts::default());
    cx.bind_keys([
        KeyBinding::new("secondary-v", Paste, Some("OkilumFeedback")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("shift-insert", Paste, Some("OkilumFeedback")),
        KeyBinding::new("secondary-w", CloseFeedback, Some("OkilumFeedbackWindow")),
        KeyBinding::new("escape", CloseFeedback, Some("OkilumFeedbackWindow")),
    ]);
    cx.on_action(|_: &SendSprintReport, cx| {
        if !sprint::enabled() {
            return;
        }
        let Some(handle) = cx.active_window() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| show_mode(window, cx, true));
        });
    });
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
        scale: backing_scale(native_backing_scale(window), window.scale_factor()),
        window_width: window.bounds().size.width.into(),
        window_height: window.bounds().size.height.into(),
        decorations: match window.window_decorations() {
            Decorations::Server => "SSD",
            Decorations::Client { .. } => "CSD",
        }
        .into(),
    }
}
// Prefer the backing scale of this actual AppKit window, not the main screen
// or the scale cached at GPUI window creation. External displays can be 1x.
fn backing_scale(native: Option<f32>, rendered: f32) -> f32 {
    native
        .filter(|scale| scale.is_finite() && *scale > 0.)
        .unwrap_or(rendered)
}
#[cfg(not(target_os = "macos"))]
fn native_backing_scale(_: &Window) -> Option<f32> {
    None
}
#[cfg(target_os = "macos")]
fn native_backing_scale(window: &Window) -> Option<f32> {
    use objc2::{msg_send, runtime::AnyObject};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
    else {
        return None;
    };
    // SAFETY: called synchronously on the GPUI UI thread. The borrowed live
    // AppKit handle owns this NSView and its NSWindow for the duration of the call.
    unsafe {
        let view = &*handle.ns_view.as_ptr().cast::<AnyObject>();
        let native: *mut AnyObject = msg_send![view, window];
        let native = native.as_ref()?;
        let scale: f64 = msg_send![native, backingScaleFactor];
        Some(scale as f32)
    }
}

fn show(window: &mut Window, cx: &mut App) {
    show_mode(window, cx, false);
}
/// What a Sprint report says when the tester typed nothing (#1597). It used to be
/// the field's starting value, with the caret before it, so typing produced
/// "ываыSprint test report". Now it is the placeholder and the sent fallback.
const SPRINT_REPORT_TEXT: &str = "Sprint test report";

fn placeholder(sprint: bool) -> &'static str {
    if sprint {
        SPRINT_REPORT_TEXT
    } else {
        "What happened, or what would you like to see?"
    }
}

/// The report text to send, or `None` when there is nothing to send. A Sprint
/// build sends its default line for an empty field, as the menu's one-click
/// Sprint report always has.
fn report_text(value: &str, sprint: bool) -> Option<String> {
    if !value.trim().is_empty() {
        Some(value.to_string())
    } else if sprint {
        Some(SPRINT_REPORT_TEXT.to_string())
    } else {
        None
    }
}

fn show_mode(window: &mut Window, cx: &mut App, send_immediately: bool) {
    cx.default_global::<Drafts>();
    let id = window.window_handle().window_id();
    // Help -> Send feedback in the Feedback window reuses its originating draft.
    let owner = cx
        .global::<Drafts>()
        .0
        .iter()
        .find_map(|(owner, draft)| (draft.window.window_id() == id).then_some(*owner))
        .unwrap_or(id);
    let owner = cx
        .windows()
        .into_iter()
        .find(|handle| handle.window_id() == owner)
        .map(|handle| reader_owned_windows::owner(handle, cx))
        .unwrap_or_else(|| window.window_handle());
    let meta = metadata(window);
    // Menu/button dispatch may still borrow the caller's window. Opening or
    // activating another window happens only after that borrow is released.
    cx.defer(move |cx| show_for(owner.window_id(), meta, send_immediately, cx));
}

fn show_for(owner: WindowId, meta: Metadata, send_immediately: bool, cx: &mut App) {
    if let Some(handle) = cx
        .global::<Drafts>()
        .0
        .get(&owner)
        .map(|draft| draft.window)
    {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            if send_immediately {
                let view = cx.global::<Drafts>().0[&owner].view.clone();
                view.update(cx, |view, cx| view.send(cx));
            }
            return;
        }
    }
    let alive = cx
        .windows()
        .into_iter()
        .map(|window| window.window_id())
        .collect::<std::collections::HashSet<_>>();
    cx.global_mut::<Drafts>().0.retain(|id, draft| {
        *id == owner || alive.contains(id) || alive.contains(&draft.window.window_id())
    });
    let previous = cx
        .global::<Drafts>()
        .0
        .get(&owner)
        .map(|draft| draft.view.clone())
        .filter(|view| !matches!(view.read(cx).state, SendState::Sent(_)));
    let owner_window = cx
        .windows()
        .into_iter()
        .find(|handle| handle.window_id() == owner);
    let options = reader_owned_windows::options(
        owner_window,
        size(px(560.), px(700.)),
        size(px(400.), px(360.)),
        cx,
    );
    let mut opened = None;
    let result = cx.open_window(options, |window, cx| {
        window.set_window_title("Send feedback — Okilum");
        reader_owned_windows::attach(owner_window, window, cx);
        let view = if let Some(view) = previous {
            // Input focus subscriptions belong to the window that created them.
            // Rebuild the field on reopen, keeping text and all draft attachments.
            view.update(cx, |view, cx| view.remount(window, cx));
            view
        } else {
            cx.new(|cx| FeedbackView::new(meta, window, cx))
        };
        view.update(cx, |view, _| view.visible = true);
        let weak = view.downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            weak.update(cx, |view, _| {
                if matches!(view.state, SendState::Sending) {
                    return false;
                }
                view.visible = false;
                true
            })
            .unwrap_or(true)
        });
        Focusable::focus_handle(view.read(cx).text.read(cx), cx).focus(window, cx);
        opened = Some(view.clone());
        let content = cx.new(|cx| {
            cx.on_release(|this: &mut FeedbackWindow, cx| {
                this.view.update(cx, |view, _| view.visible = false);
            })
            .detach();
            let sent = matches!(view.read(cx).state, SendState::Sent(_));
            let state = cx.observe_in(&view, window, |this, view, window, cx| {
                let sent = matches!(view.read(cx).state, SendState::Sent(_));
                // Transfer focus once when the textarea leaves the screen.
                if sent && !this.sent {
                    this.focus.focus(window, cx);
                }
                this.sent = sent;
            });
            FeedbackWindow {
                view,
                focus: cx.focus_handle(),
                sent,
                _state: state,
            }
        });
        cx.new(|cx| Root::new(content, window, cx))
    });
    match result {
        Ok(window) => {
            reader_owned_windows::register(owner_window, window.into(), cx);
            let view = opened.unwrap();
            cx.global_mut::<Drafts>().0.insert(
                owner,
                Draft {
                    view: view.clone(),
                    window,
                },
            );
            if send_immediately {
                view.update(cx, |view, cx| view.send(cx));
            }
        }
        Err(error) => eprintln!("Could not open Feedback: {error}"),
    }
}

struct FeedbackWindow {
    view: Entity<FeedbackView>,
    focus: FocusHandle,
    sent: bool,
    _state: Subscription,
}
impl Render for FeedbackWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.view.clone();
        let p = brand::palette(cx);
        v_flex()
            .size_full()
            .bg(p.surface)
            .text_color(p.text)
            .key_context("OkilumFeedbackWindow")
            .track_focus(&self.focus)
            .capture_action(move |_: &CloseFeedback, window, cx| {
                let sending = view.update(cx, |view, _| {
                    if matches!(view.state, SendState::Sending) {
                        return true;
                    }
                    view.visible = false;
                    false
                });
                if !sending {
                    window.remove_window();
                }
            })
            .child(TitleBar::new().child("Send feedback"))
            .child(div().flex_1().min_h_0().p_5().child(self.view.clone()))
    }
}
/// The video slot's hint; the size is the client cap (pomoi's limit).
fn video_hint() -> String {
    format!(
        "Video: MP4, MOV or WebM up to {}. Drop it here or use Attach video…",
        model::human_size(model::MAX_VIDEO)
    )
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
    /// Image data read from the platform pasteboard (#1609); a TIFF is
    /// converted to PNG first.
    Pasted(crate::note_paste::offer::PastedImage),
}
type Sender = Arc<dyn Fn(Report) -> anyhow::Result<Receipt> + Send + Sync>;
struct FeedbackView {
    text: Entity<TextareaState>,
    meta: Metadata,
    kind: Kind,
    visible: bool,
    image: Option<Attachment>,
    image_error: Option<String>,
    /// What a paste did to the attachments, e.g. replaced the screenshot.
    image_note: Option<String>,
    attaching: bool,
    image_generation: u64,
    /// Optional screen recording, sent as the multipart part "video".
    video: Option<model::Video>,
    video_error: Option<String>,
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
                .auto_grow(3, 8)
                .placeholder(placeholder(sprint::enabled()))
        });
        let subscription = cx.subscribe(
            &text,
            |_, _, event: &gpui_component::input::InputEvent, cx| {
                if matches!(event, gpui_component::input::InputEvent::Change) {
                    cx.notify();
                }
            },
        );
        let log_path = reader_diagnostics::feedback_log_path(cx).ok();
        let sprint_log = log_path.clone();
        Self {
            text,
            meta,
            kind: Kind::Bug,
            visible: false,
            image: None,
            image_error: None,
            image_note: None,
            attaching: false,
            image_generation: 0,
            video: None,
            video_error: None,
            include_log: sprint::enabled(),
            log_path,
            state: SendState::Compose,
            sender: Arc::new(move |report| {
                model::send(sprint::enrich(report, sprint_log.as_deref())?)
            }),
            _send: None,
            _image: None,
            _text: subscription,
        }
    }
    fn remount(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.text.read(cx).value();
        let selection = self.text.read(cx).selected_range();
        let scroll = self.text.read(cx).scroll_offset();
        self.text = cx.new(|cx| {
            let mut text = TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder(placeholder(sprint::enabled()));
            text.set_value(value, window, cx);
            text.set_selected_range(selection, cx);
            text.set_scroll_offset(scroll, cx);
            text
        });
        self._text = cx.subscribe(
            &self.text,
            |_, _, event: &gpui_component::input::InputEvent, cx| {
                if matches!(event, gpui_component::input::InputEvent::Change) {
                    cx.notify();
                }
            },
        );
    }
    fn busy(&self) -> bool {
        matches!(self.state, SendState::Sending | SendState::Sent(_))
    }
    fn report_without_log(&self, cx: &App) -> Report {
        Report {
            text: report_text(&self.text.read(cx).value(), sprint::enabled()).unwrap_or_default(),
            kind: self.kind,
            meta: self.meta.clone(),
            screenshot: self.image.as_ref().map(|image| image.png.clone()),
            log: None,
            video: self.video.clone(),
        }
    }
    fn report(&self, cx: &App) -> anyhow::Result<Report> {
        let mut report = self.report_without_log(cx);
        if self.include_log && sprint::enabled() {
            report.log = Some(String::new());
        } else if self.include_log {
            let path = self.log_path.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "Diagnostic log unavailable. Uncheck the log attachment to send without it."
                )
            })?;
            report.log = Some(model::log_tail(path)?);
        }
        Ok(report)
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
                let mut text = self.report_without_log(cx).copy_text();
                text.push_str(&format!(
                    "\n\nDiagnostic log could not be included: {error}"
                ));
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.state =
                    SendState::Failed(format!("Copied report without the diagnostic log. {error}"));
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
                        ImageInput::Pasted(image) => {
                            match crate::note_paste::attachments::normalize(&image) {
                                Ok((bytes, _)) => model::image_bytes(&bytes, true),
                                Err(_) => Err(anyhow::anyhow!("Choose a PNG or JPEG screenshot.")),
                            }
                        }
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
        self.image_note = None;
        cx.notify();
    }
    /// Says so when a paste replaces the screenshot: a report takes one.
    fn note_replacement(&mut self) {
        self.image_note = if self.image.is_some() || self.attaching {
            Some("Replaced the screenshot with the pasted image: a report takes one.".into())
        } else {
            // Pasted media is attached, never typed in as a path (#1609).
            Some("Attached the pasted image as the screenshot.".into())
        };
    }
    /// Attaches a video by path. Only metadata is read here; the file is
    /// streamed on Send. A rejected file keeps any previous video.
    fn attach_video(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        match model::video_file(path) {
            Ok(video) => {
                self.video = Some(video);
                self.video_error = None;
            }
            Err(error) => self.video_error = Some(error.to_string()),
        }
        cx.notify();
    }
    fn remove_video(&mut self, cx: &mut Context<Self>) {
        if matches!(self.state, SendState::Sending) {
            return;
        }
        self.video = None;
        self.video_error = None;
        cx.notify();
    }
    /// Routes dropped or pasted files: videos (mp4/mov/webm) to the video
    /// slot, anything else to the screenshot slot. At most one of each.
    fn attach_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let (videos, images): (Vec<_>, Vec<_>) = paths
            .iter()
            .partition(|path| model::video_mime(path).is_some());
        if paths.is_empty() || videos.len() > 1 || images.len() > 1 {
            self.image_error =
                Some("Attach one screenshot and at most one video at a time.".into());
            cx.notify();
            return;
        }
        // A new attempt replaces the reasons from the last one, also for the
        // slot this drop does not touch.
        self.image_error = None;
        self.video_error = None;
        if let Some(video) = videos.first() {
            self.attach_video(video, cx);
        }
        if let Some(image) = images.first() {
            self.note_replacement();
            self.image_input(ImageInput::File((*image).clone()), cx);
        }
    }
    fn video_picker(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let result = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Attach MP4, MOV or WebM video".into()),
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = result.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.attach_video(&path, cx);
                    }
                }
                Ok(Ok(None)) => {}
                _ => {
                    this.video_error =
                        Some("Couldn't open the file picker. Drop the video here instead.".into());
                    cx.notify();
                }
            });
        })
        .detach();
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
        // #1609: the platform pasteboard first. GPUI's clipboard read reports
        // a copied file's path as text before the file (Raycast's history
        // pasted ".../com.raycast.macos/clipboard/file-….png" into the text).
        let platform = cx
            .try_global::<crate::note_paste::ManagedNoteClipboard>()
            .map(|managed| managed.0.clone());
        if let Some(attachment) = platform.and_then(|clipboard| clipboard.read_attachment()) {
            use crate::note_paste::offer::ClipAttachment;
            // Media never goes into the text as a path, not even while an
            // attachment is still loading.
            if self.busy() {
                return true;
            }
            match attachment {
                ClipAttachment::Files(paths) => self.attach_paths(&paths, cx),
                ClipAttachment::Image(image) => {
                    self.note_replacement();
                    self.image_input(ImageInput::Pasted(image), cx);
                }
            }
            return true;
        }
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
                ClipboardEntry::ExternalPaths(paths) if (1..=2).contains(&paths.0.len()) => {
                    let paths = paths.0.to_vec();
                    self.attach_paths(&paths, cx);
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
            .id("feedback-form")
            .debug_selector(|| "feedback-form".into())
            .key_context("OkilumFeedback")
            .gap_3()
            .w_full()
            .max_h((window.viewport_size().height - px(88.)).max(px(160.)))
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
                let paths = paths.0.to_vec();
                if paths.is_empty() {
                    return;
                }
                this.attach_paths(&paths, cx);
            }));
        if let SendState::Sent(receipt) = &self.state {
            let url = receipt.issue_url.clone();
            let delivery_host = receipt
                .delivery_endpoint
                .as_deref()
                .and_then(|endpoint| url::Url::parse(endpoint).ok())
                .and_then(|endpoint| endpoint.host_str().map(str::to_owned));
            return body
                .child("Thank you — feedback sent.")
                .when_some(delivery_host, |body, host| {
                    body.child(div().text_sm().child(format!("Sent via {host}")))
                })
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
            .child(
                div()
                    .debug_selector(|| "feedback-public-notice".into())
                    .text_sm()
                    .child("Sending creates a public issue. Your message, system details and chosen attachments will be visible to everyone."),
            )
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
            .child(Textarea::new(&self.text).flex_shrink_0().disabled(busy))
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
                    .child(
                        Button::new("feedback-attach-video")
                            .debug_selector(|| "feedback-attach-video".into())
                            .small()
                            .ghost()
                            .disabled(busy)
                            .label("Attach video…")
                            .on_click(cx.listener(|this, _, _, cx| this.video_picker(cx))),
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
            )
            .child(
                div()
                    .debug_selector(|| "feedback-video-hint".into())
                    .text_sm()
                    .text_color(p.text_muted)
                    .child(video_hint()),
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
        if let Some(video) = &self.video {
            body = body.child(
                h_flex()
                    .debug_selector(|| "feedback-video".into())
                    .w_full()
                    .flex_none()
                    .gap_3()
                    .items_center()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(p.border)
                    .bg(Hsla {
                        a: 1.,
                        ..p.surface_raised
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .truncate()
                                    .child(format!("Video attached: {}", video.name)),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(p.text_muted)
                                    .child(video.size_label()),
                            ),
                    )
                    .child(
                        Button::new("feedback-remove-video")
                            .debug_selector(|| "feedback-remove-video".into())
                            .small()
                            .ghost()
                            .disabled(matches!(
                                self.state,
                                SendState::Sending | SendState::Sent(_)
                            ))
                            .label("Remove video")
                            .on_click(cx.listener(|this, _, _, cx| this.remove_video(cx))),
                    ),
            );
        }
        if let Some(error) = &self.video_error {
            body = body.child(
                div()
                    .debug_selector(|| "feedback-video-error".into())
                    .text_sm()
                    .text_color(p.text_muted)
                    .child(error.clone()),
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
        if let Some(note) = &self.image_note {
            body = body.child(
                div()
                    .debug_selector(|| "feedback-paste-note".into())
                    .text_sm()
                    .text_color(p.text_muted)
                    .child(note.clone()),
            );
        }
        body = body
            .child(
                Checkbox::new("feedback-log")
                    .debug_selector(|| "feedback-log".into())
                    .label(if sprint::enabled() {
                        "Attach Sprint and app diagnostic logs"
                    } else {
                        "Attach last 250 log lines"
                    })
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
            SendState::Sending if self.video.is_some() => "Uploading video…",
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
                            busy || self.attaching
                                || report_text(&self.text.read(cx).value(), sprint::enabled())
                                    .is_none(),
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
