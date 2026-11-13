//! Explicit feedback drafts. Attachments are chosen by the user; nothing is
//! captured or transmitted until Send. Failed reports survive closing the independent Feedback window.
use super::*;
use gpui_component::{
    checkbox::Checkbox,
    input::{Input, InputState, Paste, Textarea, TextareaState},
};
mod identity;
mod model;
pub(crate) mod sprint;
mod video;
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

/// Empty feedback is valid; Sprint builds retain their one-click default line.
fn report_text(value: &str, sprint: bool) -> String {
    if value.trim().is_empty() && sprint {
        SPRINT_REPORT_TEXT.to_string()
    } else {
        value.to_string()
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
        window.on_window_should_close(cx, move |window, cx| {
            crate::reader_diagnostics::window_close_requested(
                window,
                cx,
                "native platform request",
            );
            weak.update(cx, |view, cx| view.prepare_close(cx))
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
        Err(error) => diagnostic_error!("Could not open Feedback: {error}"),
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
            .capture_action({
                let view = view.clone();
                move |_: &CloseFeedback, window, cx| {
                    if view.update(cx, |view, cx| view.prepare_close(cx)) {
                        window.remove_window();
                    }
                }
            })
            .child(
                // GNOME draws our own close button; it keeps the draft like
                // the platform close (#2805), never by bare removal.
                TitleBar::new()
                    .on_close_window(move |_, window, cx| {
                        crate::reader_diagnostics::window_close_requested(
                            window,
                            cx,
                            "title bar close button",
                        );
                        if view.update(cx, |view, cx| view.prepare_close(cx)) {
                            window.remove_window();
                        }
                    })
                    .child("Send feedback"),
            )
            .child(div().flex_1().min_h_0().p_5().child(self.view.clone()))
    }
}
/// The video slot's hint; the size is the client cap (pomoi's limit).
#[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
fn video_hint() -> String {
    video_hint_for(model::Limits::default())
}
fn video_hint_for(limits: model::Limits) -> String {
    format!(
        "Video: MP4, MOV or WebM up to {}. Drop it here or use Attach video…",
        model::human_size(limits.video_bytes)
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

fn feedback_draft_root() -> Option<PathBuf> {
    #[cfg(test)]
    {
        TEST_DRAFT_ROOT.with(|root| root.borrow().clone())
    }
    #[cfg(not(test))]
    {
        reader_layout::config_base().map(|base| base.join("okilum/feedback-drafts"))
    }
}

fn feedback_limits_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        TEST_DRAFT_ROOT.with(|root| {
            root.borrow()
                .as_ref()
                .and_then(|p| p.parent())
                .map(|p| p.join("feedback-limits.json"))
        })
    }
    #[cfg(not(test))]
    {
        reader_layout::config_base().map(|base| base.join("okilum/feedback-limits.json"))
    }
}

#[cfg(test)]
struct SendHold {
    reached: async_channel::Sender<()>,
    release: async_channel::Receiver<()>,
}

#[cfg(test)]
thread_local! {
    static TEST_DRAFT_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    // Holds only the existing sender's scheduling boundary, keyed to its view.
    // The original Sender, cancellation signal and completion path still run.
    static TEST_SEND_HOLD: std::cell::RefCell<Option<(EntityId, SendHold)>> = const { std::cell::RefCell::new(None) };
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
    reporter: Entity<InputState>,
    identity: identity::Data,
    identity_store: Option<identity::Store>,
    identity_error: Option<String>,
    _reporter: Subscription,
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
    send_abort: Option<futures_util::future::AbortHandle>,
    send_cancel: Arc<std::sync::Mutex<Option<model::CancelSignal>>>,
    _image: Option<Task<()>>,
    draft_store: Option<model::draft::Store>,
    saved_drafts: Vec<(model::draft::Store, model::draft::Saved)>,
    persistence_error: Option<String>,
    recovery_error: Option<String>,
    unavailable_screenshot: Option<String>,
    video_needs_reselect: bool,
    limits: model::Limits,
    limits_path: Option<PathBuf>,
    limits_error: Option<String>,
    limit_inputs: Option<[Entity<InputState>; 6]>,
    preflight_error: Option<String>,
    sender_limits: Arc<std::sync::Mutex<model::Limits>>,
    _text: Subscription,
    _draft_checkpoint: Option<Task<()>>,
}
impl FeedbackView {
    fn new(meta: Metadata, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (identity_store, identity_error) = match feedback_limits_path()
            .map(|path| identity::Store::open(path.with_file_name("feedback-identity.json")))
            .transpose()
        {
            Ok(store) => (store, None),
            Err(_) => (None, Some("Couldn't read the private feedback identity. Its file was kept; restore it before sending.".into())),
        };
        let identity = identity_store
            .as_ref()
            .map(|store| store.data.clone())
            .unwrap_or_else(identity::Data::anonymous);
        let reporter = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Name or GitHub (optional)");
            input.set_value(identity.reporter_name.clone(), window, cx);
            input
        });
        let reporter_subscription = cx.subscribe(
            &reporter,
            |this, _, event: &gpui_component::input::InputEvent, cx| {
                if matches!(event, gpui_component::input::InputEvent::Change) {
                    this.schedule_checkpoint(cx);
                    cx.notify();
                }
            },
        );
        let text = cx.new(|cx| {
            let mut text = TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder(placeholder(sprint::enabled()));
            text.set_ime_settle(std::time::Duration::from_millis(500));
            text
        });
        let subscription = cx.subscribe(
            &text,
            |this, _, event: &gpui_component::input::InputEvent, cx| {
                if matches!(event, gpui_component::input::InputEvent::Change) {
                    this.preflight_error = None;
                    this.schedule_checkpoint(cx);
                    cx.notify();
                }
            },
        );
        let log_path = reader_diagnostics::feedback_log_path(cx).ok();
        let sprint_log = log_path.clone();
        let send_cancel = Arc::new(std::sync::Mutex::new(None::<model::CancelSignal>));
        let cancellation = send_cancel.clone();
        let limits_path = feedback_limits_path();
        let (limits, limits_error) = match limits_path.as_ref().map(|p| model::Limits::load(p)).transpose() {
            Ok(limits) => (limits.unwrap_or_default(), None),
            Err(error) => (
                model::Limits::default(),
                Some(format!(
                    "Couldn't read attachment limits: {error}. Review Attachment limits before sending."
                )),
            ),
        };
        let sender_limits = Arc::new(std::sync::Mutex::new(limits));
        let sending_limits = sender_limits.clone();
        let (saved_drafts, recovery_error) = match feedback_draft_root()
            .map(|root| model::draft::discover(&root))
            .transpose()
        {
            Ok(saved) => {
                let (drafts, errors) = saved.unwrap_or_default();
                (drafts, (!errors.is_empty()).then(|| errors.join("\n")))
            }
            Err(error) => (
                Vec::new(),
                Some(format!(
                    "Couldn't read saved Feedback drafts: {error}. Their files have been kept."
                )),
            ),
        };
        let saved_drafts = saved_drafts
            .into_iter()
            .filter(|(store, _)| {
                !cx.try_global::<Drafts>().is_some_and(|drafts| {
                    drafts.0.values().any(|draft| {
                        draft
                            .view
                            .read(cx)
                            .draft_store
                            .as_ref()
                            .is_some_and(|active| active.directory == store.directory)
                    })
                })
            })
            .collect();
        Self {
            text,
            reporter,
            identity,
            identity_store,
            identity_error,
            _reporter: reporter_subscription,
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
            include_log: true,
            log_path,
            state: SendState::Compose,
            sender: Arc::new(move |report| {
                let report = sprint::enrich(report, sprint_log.as_deref())?;
                sending_limits
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .check_report(&report)?;
                let cancel = cancellation
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                match cancel {
                    Some(cancel) => model::send_cancellable(report, cancel),
                    None => model::send(report),
                }
            }),
            _send: None,
            send_abort: None,
            send_cancel,
            _image: None,
            draft_store: None,
            saved_drafts,
            persistence_error: None,
            recovery_error,
            unavailable_screenshot: None,
            video_needs_reselect: false,
            limits,
            limits_path,
            limits_error,
            limit_inputs: None,
            preflight_error: None,
            sender_limits,
            _text: subscription,
            _draft_checkpoint: None,
        }
    }
    fn schedule_checkpoint(&mut self, cx: &mut Context<Self>) {
        // Coalesce authored text changes. Send and window close synchronously
        // flush the latest state before crossing their lifecycle boundaries.
        self._draft_checkpoint = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(300))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.checkpoint(cx);
                cx.notify();
            });
        }));
    }
    /// Commit authored data before any network request; failures stay visible
    /// and stop Send rather than pretending restart recovery is available.
    fn checkpoint(&mut self, cx: &App) -> bool {
        let reporter = self.reporter.read(cx).value().to_string();
        if let Some(store) = &mut self.identity_store {
            if store.remember(&reporter).is_err() {
                self.identity_error = Some("Couldn't save the private reporter name. Use at most 128 characters without line breaks and check the app data folder.".into());
                return false;
            }
            self.identity = store.data.clone();
            self.identity_error = None;
        } else {
            #[cfg(not(test))]
            {
                self.identity_error = Some("Feedback identity storage is unavailable. Sending is stopped; keep this window open and check the app data folder.".into());
                return false;
            }
        }
        if self.draft_store.is_none() {
            if self.text.read(cx).value().is_empty() && self.image.is_none() && self.video.is_none()
            {
                return true;
            }
            let Some(root) = feedback_draft_root() else {
                #[cfg(test)]
                return true;
                #[cfg(not(test))]
                {
                    self.persistence_error = Some("Couldn't locate Feedback draft storage. Sending is stopped; copy your report before closing.".into());
                    return false;
                }
            };
            self.draft_store = Some(model::draft::Store::new(
                root.join(uuid::Uuid::new_v4().to_string()),
            ));
        }
        let status = match &self.state {
            SendState::Compose => "Saved draft. Review it before sending.".to_string(),
            SendState::Sending => "Sending was interrupted. Delivery is uncertain; check whether an issue was created before retrying.".to_string(),
            SendState::Failed(error) => error.clone(),
            SendState::Sent(receipt) => format!("Sent: {}", receipt.issue_url),
        };
        let saved = model::draft::Saved {
            version: 1,
            text: self.text.read(cx).value().to_string(),
            feature: self.kind == Kind::Feature,
            include_log: self.include_log,
            video: self.video.clone(),
            screenshot: self.unavailable_screenshot.clone(),
            status,
            sent: matches!(self.state, SendState::Sent(_)),
        };
        let screenshot = self.image.as_ref().map(|image| image.png.as_slice());
        match self.draft_store.as_mut().unwrap().save(saved, screenshot) {
            Ok(()) => {
                self.persistence_error = None;
                true
            }
            Err(error) => {
                self.persistence_error = Some(format!("Couldn't save Feedback draft: {error}. Sending is stopped; copy the report before closing."));
                false
            }
        }
    }
    fn reopen_saved_revision(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || self.attaching {
            return;
        }
        let Some(previous) = self.draft_store.take() else {
            return;
        };
        // Preserve unsaved authored changes under a new identity before loading
        // a revision changed by another window/process.
        if !self.checkpoint(cx) {
            self.draft_store = Some(previous);
            cx.notify();
            return;
        }
        match model::draft::Store::load(previous.directory) {
            Ok(loaded) => {
                let index = self.saved_drafts.len();
                self.saved_drafts.push(loaded);
                self.restore_draft(index, window, cx);
            }
            Err(error) => {
                self.persistence_error = Some(format!("Couldn't reopen the saved revision: {error}. The current draft was kept as a separate copy."));
                cx.notify();
            }
        }
    }
    fn restore_draft(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || self.attaching || index >= self.saved_drafts.len() {
            return;
        }
        if (!self.text.read(cx).value().is_empty() || self.image.is_some() || self.video.is_some())
            && !self.checkpoint(cx)
        {
            cx.notify();
            return;
        }
        let (store, _) = &self.saved_drafts[index];
        if cx.try_global::<Drafts>().is_some_and(|drafts| {
            drafts.0.values().any(|draft| {
                draft.view.entity_id() != cx.entity_id()
                    && draft
                        .view
                        .read(cx)
                        .draft_store
                        .as_ref()
                        .is_some_and(|active| active.directory == store.directory)
            })
        }) {
            self.persistence_error =
                Some("This saved draft is already open in another Feedback window.".into());
            cx.notify();
            return;
        }
        self._draft_checkpoint = None;
        let (store, _) = self.saved_drafts.remove(index);
        // Re-read the committed revision at the explicit selection boundary.
        let (store, saved) = match model::draft::Store::load(store.directory.clone()) {
            Ok(loaded) => loaded,
            Err(error) => {
                self.persistence_error = Some(format!("Couldn't reopen Feedback draft: {error}"));
                cx.notify();
                return;
            }
        };
        if saved.sent {
            self.persistence_error = Some("This draft has already been sent. Its saved receipt is kept; it will not be resent.".into());
            cx.notify();
            return;
        }
        let image = store.screenshot(&saved).and_then(|bytes| {
            bytes
                .map(|bytes| {
                    let preview = image::load_from_memory(&bytes)?.into_rgba8();
                    Ok(Attachment {
                        png: Arc::new(bytes),
                        preview: Arc::new(RenderImage::new(vec![image::Frame::new(preview)])),
                    })
                })
                .transpose()
        });
        self.image = None;
        self.unavailable_screenshot = None;
        self.image_error = None;
        match image {
            Ok(image) => self.image = image,
            Err(error) => {
                self.unavailable_screenshot = saved.screenshot.clone();
                self.image_error = Some(format!("Couldn't restore the selected screenshot: {error}. Remove it or attach it again."));
            }
        }
        self.video = saved.video;
        self.video_needs_reselect = self.video.as_ref().is_some_and(|video| {
            model::video_file(&video.path).map_or(true, |current| current != *video)
        });
        self.video_error = self.video_needs_reselect.then(|| "The selected video changed, was replaced or is unavailable. Select it again or remove it before retrying.".into());
        self.preflight_error = self
            .video
            .as_ref()
            .filter(|_| !self.video_needs_reselect)
            .and_then(|video| self.limits.check_video(video).err())
            .map(|error| format!("{error:#}"));
        self.kind = if saved.feature {
            Kind::Feature
        } else {
            Kind::Bug
        };
        self.include_log = saved.include_log;
        self.image_note = None;
        self.state = SendState::Failed(format!(
            "Reopened saved draft. {} Review the attachments and explicitly choose Retry.",
            saved.status
        ));
        self.draft_store = Some(store);
        self.persistence_error = None;
        self.text
            .update(cx, |text, cx| text.set_value(saved.text, window, cx));
        cx.notify();
    }
    fn remount(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.limit_inputs = None;
        let reporter = self.reporter.read(cx).value();
        self.reporter = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Name or GitHub (optional)");
            input.set_value(reporter, window, cx);
            input
        });
        self._reporter = cx.subscribe(
            &self.reporter,
            |this, _, event: &gpui_component::input::InputEvent, cx| {
                if matches!(event, gpui_component::input::InputEvent::Change) {
                    this.schedule_checkpoint(cx);
                    cx.notify();
                }
            },
        );
        let value = self.text.read(cx).value();
        let selection = self.text.read(cx).selected_range();
        let scroll = self.text.read(cx).scroll_offset();
        self.text = cx.new(|cx| {
            let mut text = TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder(placeholder(sprint::enabled()));
            text.set_ime_settle(std::time::Duration::from_millis(500));
            text.set_value(value, window, cx);
            text.set_selected_range(selection, cx);
            text.set_scroll_offset(scroll, cx);
            text
        });
        self._text = cx.subscribe(
            &self.text,
            |this, _, event: &gpui_component::input::InputEvent, cx| {
                if matches!(event, gpui_component::input::InputEvent::Change) {
                    this.preflight_error = None;
                    this.schedule_checkpoint(cx);
                    cx.notify();
                }
            },
        );
    }
    fn busy(&self) -> bool {
        matches!(self.state, SendState::Sending | SendState::Sent(_))
    }
    fn toggle_limits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() || self.attaching {
            return;
        }
        if self.limit_inputs.is_some() {
            self.limit_inputs = None;
        } else {
            let values = self.limits.fields();
            self.limit_inputs = Some(std::array::from_fn(|index| {
                cx.new(|cx| {
                    let mut input = InputState::new(window, cx).placeholder(if index == 1 {
                        "No duration limit"
                    } else {
                        "Limit"
                    });
                    input.set_value(values[index].clone(), window, cx);
                    input
                })
            }));
        }
        cx.notify();
    }
    fn apply_limits(&mut self, cx: &mut Context<Self>) {
        if self.busy() || self.attaching {
            return;
        }
        let Some(inputs) = &self.limit_inputs else {
            return;
        };
        let result = model::Limits::from_fields(std::array::from_fn(|i| {
            inputs[i].read(cx).value().to_string()
        }))
        .and_then(|limits| {
            let path = self.limits_path.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "Attachment limits storage is unavailable. Limits were not changed."
                )
            })?;
            limits.save(path)?;
            Ok(limits)
        });
        match result {
            Ok(limits) => {
                self.limits = limits;
                *self.sender_limits.lock().unwrap_or_else(|e| e.into_inner()) = limits;
                self.limits_error = None;
                self.preflight_error = self
                    .video
                    .as_ref()
                    .and_then(|video| limits.check_video(video).err())
                    .map(|error| format!("{error:#}"));
                self.limit_inputs = None;
            }
            Err(error) => self.limits_error = Some(format!("{error:#}")),
        }
        cx.notify();
    }
    fn selection_limits_error(&self, cx: &App) -> Option<String> {
        let mut identity = self.identity.clone();
        identity.reporter_name = self.reporter.read(cx).value().trim().to_owned();
        if let Err(error) = identity.validate() {
            return Some(error.to_string());
        }
        let text = self.text.read(cx).value();
        if text.chars().count() > 10_000 || text.contains('\0') {
            return Some(
                "Feedback must contain at most 10,000 characters and no NUL characters.".into(),
            );
        }
        self.limits
            .check_sizes(
                self.image.as_ref().map(|image| image.png.len() as u64),
                self.video.as_ref().map(|video| video.size),
                self.include_log.then_some(0),
            )
            .err()
            .map(|error| error.to_string())
    }
    fn report_without_log(&self, cx: &App) -> Report {
        Report {
            text: report_text(&self.text.read(cx).value(), sprint::enabled()),
            kind: self.kind,
            meta: self.meta.clone(),
            identity: identity::Data {
                install_id: self.identity.install_id,
                reporter_name: self.reporter.read(cx).value().trim().to_owned(),
            },
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
            report.log = Some(model::log_tail_with_limit(
                path,
                self.limits.log_bytes as usize,
            )?);
        }
        Ok(report)
    }
    fn can_send(&self, cx: &App) -> bool {
        !self.busy()
            && !self.attaching
            && self.unavailable_screenshot.is_none()
            && !self.video_needs_reselect
            && self.limits_error.is_none()
            && self.identity_error.is_none()
            && self.limit_inputs.is_none()
            && self.preflight_error.is_none()
            && self.selection_limits_error(cx).is_none()
    }
    fn send(&mut self, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        self._draft_checkpoint = None;
        let report = match self.report(cx) {
            Ok(report) => report,
            Err(error) => {
                self.state = SendState::Failed(error.to_string());
                self.checkpoint(cx);
                cx.notify();
                return;
            }
        };
        if let Err(error) = self.limits.check_report(&report) {
            self.preflight_error = Some(format!("{error:#}"));
            self.checkpoint(cx);
            cx.notify();
            return;
        }
        use futures_util::FutureExt;
        let (abort, registration) = futures_util::future::AbortHandle::new_pair();
        let cancel =
            futures_util::future::Abortable::new(std::future::pending::<()>(), registration)
                .boxed()
                .shared();
        *self.send_cancel.lock().unwrap_or_else(|e| e.into_inner()) = Some(cancel);
        self.send_abort = Some(abort);
        self.state = SendState::Sending;
        if !self.checkpoint(cx) {
            self.send_abort = None;
            *self.send_cancel.lock().unwrap_or_else(|e| e.into_inner()) = None;
            self.state =
                SendState::Failed("Sending stopped because the draft could not be saved.".into());
            cx.notify();
            return;
        }
        let sender = self.sender.clone();
        let selected_video = report.video.clone();
        #[cfg(test)]
        let hold = TEST_SEND_HOLD.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|(owner, _)| *owner == cx.entity_id())
            {
                slot.take().map(|(_, hold)| hold)
            } else {
                None
            }
        });
        self._send = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    if let Some(hold) = hold {
                        hold.reached.try_send(()).expect("mounted sender reached");
                        hold.release.recv().await.expect("mounted sender released");
                    }
                    let result = sender(report);
                    // Validate on the worker after a failed/cancelled provider
                    // completion. Hashing a recording must not block the UI.
                    let selection_error = result
                        .as_ref()
                        .err()
                        .and_then(|_| video::recovery_error(selected_video.as_ref()));
                    (result, selection_error)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.send_abort = None;
                *this.send_cancel.lock().unwrap_or_else(|e| e.into_inner()) = None;
                let (result, selection_error) = result;
                if result.is_err() {
                    this.video_needs_reselect = selection_error.is_some();
                    this.video_error = selection_error;
                }
                this.state = match result {
                    Ok(receipt) => SendState::Sent(receipt),
                    Err(error) => SendState::Failed(error.to_string()),
                };
                this.checkpoint(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn cancel_send(&mut self, cx: &mut Context<Self>) {
        if let Some(abort) = &self.send_abort {
            abort.abort();
        }
        // Remain busy until the transport acknowledges cancellation. A new
        // Send cannot race an old upload or consume its completion.
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
        self.preflight_error = None;
        let limits = self.limits;
        self._image = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let image = match input {
                        ImageInput::File(path) => model::image_file(&path),
                        ImageInput::Clipboard(bytes) => model::image_bytes(&bytes, true),
                        ImageInput::Pasted(image) => {
                            match crate::note_paste::attachments::normalize(&image) {
                                Ok((bytes, _)) => model::image_bytes(&bytes, true),
                                Err(_) => Err(anyhow::anyhow!("Choose a PNG or JPEG screenshot.")),
                            }
                        }
                    }?;
                    limits.check_sizes(Some(image.0.len() as u64), None, None)?;
                    Ok::<_, anyhow::Error>(image)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if generation != this.image_generation {
                    return;
                }
                this.attaching = false;
                match result {
                    Ok((png, preview)) => {
                        this.unavailable_screenshot = None;
                        this.image = Some(Attachment {
                            png,
                            preview: Arc::new(RenderImage::new(vec![image::Frame::new(preview)])),
                        })
                    }
                    Err(error) => this.image_error = Some(error.to_string()),
                }
                this.checkpoint(cx);
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
        self.unavailable_screenshot = None;
        self.image_error = None;
        self.image_note = None;
        self.preflight_error = None;
        self.checkpoint(cx);
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
    /// Captures an explicit video selection and its identity. A rejected
    /// replacement keeps the previous selection; transport verifies it again.
    fn attach_video(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        match model::video_file_with_limits(path, self.limits) {
            Ok(video) => {
                self.video = Some(video);
                self.video_needs_reselect = false;
                self.video_error = None;
                self.preflight_error = None;
                self.checkpoint(cx);
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
        self.video_needs_reselect = false;
        self.video_error = None;
        self.preflight_error = None;
        self.checkpoint(cx);
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
impl FeedbackView {
    /// Every close of the Feedback window keeps the draft; a send in flight
    /// keeps the window open. `true` means close now.
    fn prepare_close(&mut self, cx: &mut Context<Self>) -> bool {
        if matches!(self.state, SendState::Sending) {
            return false;
        }
        self.checkpoint(cx);
        self.visible = false;
        true
    }
}

impl Render for FeedbackView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy();
        let p = brand::palette(cx);
        let mut body = v_flex()
            .id("feedback-form")
            .debug_selector(|| "feedback-form".into())
            .key_context("OkilumFeedback")
            .gap_3()
            .w_full()
            .flex_1()
            .min_h_0()
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
            return v_flex()
                .size_full()
                .child(
                    body.child("Thank you — feedback sent.")
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
                        ),
                )
                .into_any_element();
        }
        for (index, (_, saved)) in self.saved_drafts.iter().enumerate() {
            let title: String = saved.text.chars().take(60).collect();
            body = body.child(
                Button::new(("feedback-reopen-draft", index))
                    .when(cfg!(test), |button| {
                        button.debug_selector(move || format!("feedback-reopen-draft-{index}"))
                    })
                    .small()
                    .ghost()
                    .disabled(busy || self.attaching)
                    .label(format!(
                        "Reopen saved draft: {}",
                        if title.is_empty() { "Untitled" } else { &title }
                    ))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.restore_draft(index, window, cx)
                    })),
            );
        }
        for error in [
            &self.recovery_error,
            &self.persistence_error,
            &self.identity_error,
        ]
        .into_iter()
        .flatten()
        {
            body = body.child(div().text_sm().child(error.clone()));
        }
        if self.persistence_error.is_some() && self.draft_store.is_some() {
            body = body.child(
                Button::new("feedback-reopen-revision")
                    .small()
                    .ghost()
                    .disabled(busy || self.attaching)
                    .label("Reopen saved revision; keep current draft as a copy")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.reopen_saved_revision(window, cx)),
                    ),
            );
        }
        body = body
            .child(div().text_sm().child(self.limits.summary()))
            .child(
                Button::new("feedback-limits")
                    .small()
                    .ghost()
                    .debug_selector(|| "feedback-limits".into())
                    .disabled(busy || self.attaching)
                    .label("Attachment limits…")
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_limits(window, cx))),
            );
        if let Some(inputs) = &self.limit_inputs {
            let labels = [
                "Video (MB, up to 90)",
                "Video duration (seconds; blank = no limit)",
                "Screenshot (MB)",
                "Diagnostic log (KB)",
                "All attachments combined (MB)",
                "Attachment count (1–3)",
            ];
            body = body.child(
                v_flex()
                    .gap_2()
                    .children(inputs.iter().enumerate().map(|(index, input)| {
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().text_sm().child(labels[index]))
                            .child(Input::new(input).w(px(160.)))
                    }))
                    .child(
                        Button::new("feedback-apply-limits")
                            .small()
                            .label("Save limits")
                            .debug_selector(|| "feedback-apply-limits".into())
                            .on_click(cx.listener(|this, _, _, cx| this.apply_limits(cx))),
                    ),
            );
        }
        let selection_error = self.selection_limits_error(cx);
        for error in [
            self.limits_error.as_ref(),
            self.preflight_error.as_ref(),
            selection_error.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            body = body.child(
                div()
                    .debug_selector(|| "feedback-limit-error".into())
                    .text_sm()
                    .child(error.clone()),
            );
        }
        body = body
            .child(
                div()
                    .debug_selector(|| "feedback-public-notice".into())
                    .text_sm()
                    .child("Sending creates a public issue. Your message, system details and chosen screenshot or video are public. Diagnostic logs stay private."),
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
                            this.checkpoint(cx);
                            cx.notify();
                        }))
                }),
            ))
            .child(
                div()
                    .debug_selector(|| "feedback-description".into())
                    .flex_shrink_0()
                    // Only the description sends with Enter. Reporter and
                    // attachment-limit inputs retain their own key handling.
                    .capture_action(cx.listener(|this, action: &gpui_component::input::Enter, _, cx| {
                        // A candidate-confirming Enter may arrive just after
                        // its marked range has cleared. Never send or alter it.
                        if this.text.read(cx).ime_settling(cx) {
                            cx.stop_propagation();
                            return;
                        }
                        if action.secondary || action.shift {
                            cx.propagate();
                        } else {
                            cx.stop_propagation();
                            this.send(cx);
                        }
                    }))
                    .child(Textarea::new(&self.text).disabled(busy)),
            )
            .child(div().text_sm().text_color(p.text_muted).child(if cfg!(target_os = "macos") {
                "Enter to send · ⌘Enter for a new line"
            } else {
                "Enter to send · Ctrl+Enter for a new line"
            }))
            .child(div().debug_selector(|| "feedback-reporter".into()).child(Input::new(&self.reporter).disabled(busy)))
            .child(div().text_sm().child("Your optional name and a random installation ID are sent privately to group reports. Public issues show only an opaque label. Your name is remembered on this installation."))
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
                    .when(self.image.is_some() || self.attaching || self.unavailable_screenshot.is_some(), |row| {
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
                    .child(video_hint_for(self.limits)),
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
                        this.preflight_error = None;
                        this.checkpoint(cx);
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
        v_flex()
            .size_full()
            .gap_3()
            .child(body)
            .child(
                h_flex()
                    .flex_shrink_0()
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
                        Button::new("feedback-cancel")
                            .debug_selector(|| "feedback-cancel".into())
                            .ghost()
                            .disabled(
                                self.send_abort
                                    .as_ref()
                                    .is_some_and(|abort| abort.is_aborted()),
                            )
                            .label(
                                if self
                                    .send_abort
                                    .as_ref()
                                    .is_some_and(|abort| abort.is_aborted())
                                {
                                    "Cancelling…"
                                } else if matches!(self.state, SendState::Sending) {
                                    "Cancel sending"
                                } else {
                                    "Cancel"
                                },
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                if matches!(this.state, SendState::Sending) {
                                    this.cancel_send(cx);
                                } else {
                                    window.dispatch_action(Box::new(CloseFeedback), cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("feedback-send")
                            .debug_selector(|| "feedback-send".into())
                            .primary()
                            .disabled(self.busy() || self.attaching)
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

#[cfg(test)]
mod recovery_mounted_tests {
    use super::*;
    use ::core::prelude::v1::test;
    use futures_util::FutureExt;

    struct TestRoots;
    impl Drop for TestRoots {
        fn drop(&mut self) {
            TEST_SEND_HOLD.with(|slot| slot.borrow_mut().take());
            TEST_DRAFT_ROOT.with(|slot| slot.borrow_mut().take());
        }
    }

    struct Fixture {
        _roots: TestRoots,
        directory: tempfile::TempDir,
        drafts: PathBuf,
        note: PathBuf,
    }

    fn draw(visual: &mut VisualTestContext) {
        visual.run_until_parked();
        for _ in 0..2 {
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
    }

    fn click(visual: &mut VisualTestContext, selector: &'static str) {
        let bounds = visual
            .debug_bounds(selector)
            .expect("real Feedback control painted");
        visual.simulate_click(bounds.center(), Modifiers::default());
        draw(visual);
    }

    fn mount(cx: &mut TestAppContext) -> (Fixture, &mut VisualTestContext) {
        let directory = tempfile::tempdir().unwrap();
        let vault = directory.path().join("vault");
        std::fs::create_dir(&vault).unwrap();
        let note = vault.join("start.md");
        std::fs::write(&note, "# Feedback fixture\n").unwrap();
        let drafts = directory.path().join("operational/feedback-drafts");
        TEST_DRAFT_ROOT.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(drafts.clone());
        });
        let state = directory.path().join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::write(
            state.join("reader-diagnostic.log"),
            "synthetic diagnostic\n",
        )
        .unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            cx.set_global(reader_diagnostics::LaunchTrace(
                reader_diagnostics::Trace::new(Some(state), None),
            ));
            version_info::install(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault),
                        note: Some("start.md".into()),
                        session_directory: Some(directory.path().join("state")),
                        index_dir: Some(directory.path().join("index")),
                        panel_settings_override: Some(directory.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            Root::new(reader, window, cx)
        });
        visual.simulate_resize(size(px(1200.), px(900.)));
        draw(visual);
        (
            Fixture {
                _roots: TestRoots,
                directory,
                drafts,
                note,
            },
            visual,
        )
    }

    fn open(reader: &mut VisualTestContext) -> (Entity<FeedbackView>, VisualTestContext) {
        reader.update(|window, _| window.activate_window());
        draw(reader);
        click(reader, "reader-feedback");
        let (view, handle) = reader.update(|window, cx| {
            let draft = &cx.global::<Drafts>().0[&window.window_handle().window_id()];
            (draft.view.clone(), draft.window)
        });
        let mut feedback = VisualTestContext::from_window(handle.into(), &reader.cx);
        feedback.simulate_resize(size(px(600.), px(900.)));
        draw(&mut feedback);
        assert!(feedback.debug_bounds("feedback-form").is_some());
        feedback.update(|window, cx| {
            assert!(Focusable::focus_handle(view.read(cx).text.read(cx), cx).is_focused(window));
        });
        (view, feedback)
    }

    fn choose(visual: &mut VisualTestContext, path: &Path, video: bool) {
        click(
            visual,
            if video {
                "feedback-attach-video"
            } else {
                "feedback-attach"
            },
        );
        assert!(visual.did_prompt_for_paths(), "actual picker route reached");
        visual.simulate_path_prompt_response(|options| {
            assert!(options.files && !options.directories && !options.multiple);
            Some(vec![path.to_owned()])
        });
        draw(visual);
        assert!(visual
            .debug_bounds(if video {
                "feedback-video"
            } else {
                "feedback-preview"
            })
            .is_some());
    }

    fn hold(
        view: &Entity<FeedbackView>,
    ) -> (async_channel::Receiver<()>, async_channel::Sender<()>) {
        let (reached, started) = async_channel::bounded(1);
        let (release, wait) = async_channel::bounded(1);
        TEST_SEND_HOLD.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some((
                view.entity_id(),
                SendHold {
                    reached,
                    release: wait,
                },
            ));
        });
        (started, release)
    }

    fn receipt() -> Receipt {
        Receipt {
            issue_number: 2801,
            issue_url: "https://git.oklabs.uk/BeFeast/okilum/issues/2801".into(),
            delivery_endpoint: None,
        }
    }

    fn configure_limits(
        view: &Entity<FeedbackView>,
        visual: &mut VisualTestContext,
        limits: model::Limits,
    ) {
        if view.read_with(visual, |view, _| view.limit_inputs.is_none()) {
            click(visual, "feedback-limits");
        }
        let inputs = view.read_with(visual, |view, _| view.limit_inputs.clone().unwrap());
        for (input, value) in inputs.iter().zip(limits.fields()) {
            input.update_in(visual, |input, window, cx| {
                input.set_value(value, window, cx)
            });
        }
        draw(visual);
        click(visual, "feedback-apply-limits");
    }

    #[gpui::test]
    fn feedback1575_limits_controls_persist_caps_and_block_sender_before_upload(
        cx: &mut TestAppContext,
    ) {
        let (f, reader) = mount(cx);
        let path = f.directory.path().join("clip.webm");
        std::fs::write(&path, [0; 64]).unwrap();
        let screenshot = f.directory.path().join("screenshot.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]))
            .save(&screenshot)
            .unwrap();
        let (view, mut feedback) = open(reader);
        view.update(&mut feedback, |view, _| view.include_log = false);
        feedback.simulate_input("Keep this report while adjusting limits");
        choose(&mut feedback, &path, true);
        choose(&mut feedback, &screenshot, false);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let capture = calls.clone();
        view.update(&mut feedback, |view, _| {
            view.sender = Arc::new(move |report| {
                capture.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                assert_eq!(report.text, "Keep this report while adjusting limits");
                assert!(report.video.is_some() && report.screenshot.is_some());
                Ok(receipt())
            });
        });
        let default = model::Limits::default();
        for (limits, reason) in [
            (
                model::Limits {
                    video_bytes: 32,
                    ..default
                },
                "Video",
            ),
            (
                model::Limits {
                    screenshot_bytes: 1,
                    ..default
                },
                "Screenshot",
            ),
            (
                model::Limits {
                    max_attachments: 1,
                    ..default
                },
                "attachments",
            ),
            (
                model::Limits {
                    total_bytes: 65,
                    ..default
                },
                "combined limit",
            ),
        ] {
            configure_limits(&view, &mut feedback, limits);
            assert!(feedback.debug_bounds("feedback-limit-error").is_some());
            view.read_with(&feedback, |view, cx| {
                assert!(!view.can_send(cx));
                assert!(view.selection_limits_error(cx).unwrap().contains(reason));
                assert!(view.image.is_some() && view.video.is_some());
            });
            click(&mut feedback, "feedback-send");
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
        configure_limits(
            &view,
            &mut feedback,
            model::Limits {
                video_bytes: 90_000_001,
                ..default
            },
        );
        assert!(view.read_with(&feedback, |view, _| view.limits_error.is_some()));
        let valid = model::Limits {
            video_bytes: 80,
            screenshot_bytes: 512,
            log_bytes: 64,
            total_bytes: 1000,
            max_attachments: 2,
            ..default
        };
        configure_limits(&view, &mut feedback, valid);
        assert!(view.read_with(&feedback, |view, cx| view.can_send(cx)));
        let stored = view.read_with(&feedback, |view, _| view.limits_path.clone().unwrap());
        assert_eq!(model::Limits::load(&stored).unwrap(), valid);
        click(&mut feedback, "feedback-send");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(feedback.debug_bounds("feedback-issue").is_some());
        feedback.simulate_keystrokes("escape");
        draw(reader);
        forget_closed_view(reader, view);
        let (fresh, feedback) = open(reader);
        fresh.read_with(&feedback, |view, cx| {
            assert_eq!(
                view.limits, valid,
                "new Feedback entity loads the saved limits"
            );
            assert!(view.text.read(cx).value().is_empty());
        });
    }

    #[gpui::test]
    fn feedback1575_limits_duration_is_rechecked_before_sender_and_reselection_recovers(
        cx: &mut TestAppContext,
    ) {
        fn movie(seconds: u32) -> Vec<u8> {
            let mut bytes = vec![
                0, 0, 0, 36, b'm', b'o', b'o', b'v', 0, 0, 0, 28, b'm', b'v', b'h', b'd',
            ];
            let mut timing = vec![0; 20];
            timing[12..16].copy_from_slice(&1000u32.to_be_bytes());
            timing[16..20].copy_from_slice(&(seconds * 1000).to_be_bytes());
            bytes.extend(timing);
            bytes
        }
        let (f, reader) = mount(cx);
        let path = f.directory.path().join("clip.mp4");
        std::fs::write(&path, movie(12)).unwrap();
        let (view, mut feedback) = open(reader);
        feedback.simulate_input("Duration validation retains my report");
        choose(&mut feedback, &path, true);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let capture = calls.clone();
        view.update(&mut feedback, |view, _| {
            view.sender = Arc::new(move |_| {
                capture.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(receipt())
            })
        });
        configure_limits(
            &view,
            &mut feedback,
            model::Limits {
                video_seconds: Some(5),
                ..Default::default()
            },
        );
        assert!(feedback.debug_bounds("feedback-limit-error").is_some());
        click(&mut feedback, "feedback-send");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        configure_limits(
            &view,
            &mut feedback,
            model::Limits {
                video_seconds: Some(20),
                ..Default::default()
            },
        );
        assert!(view.read_with(&feedback, |view, cx| view.can_send(cx)));
        // Same byte length: preflight must inspect timing, not infer duration
        // from file size or wait for a rejected upload.
        std::fs::write(&path, movie(30)).unwrap();
        click(&mut feedback, "feedback-send");
        view.read_with(&feedback, |view, cx| {
            assert!(matches!(view.state, SendState::Compose));
            assert!(view
                .preflight_error
                .as_ref()
                .unwrap()
                .contains("configured limit is 20 seconds"));
            assert_eq!(
                view.text.read(cx).value(),
                "Duration validation retains my report"
            );
        });
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        std::fs::write(&path, movie(12)).unwrap();
        choose(&mut feedback, &path, true);
        click(&mut feedback, "feedback-send");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(feedback.debug_bounds("feedback-issue").is_some());
    }

    fn saved(
        view: &Entity<FeedbackView>,
        visual: &VisualTestContext,
    ) -> (model::draft::Store, model::draft::Saved) {
        let directory = view.read_with(visual, |view, _| {
            view.draft_store.as_ref().unwrap().directory.clone()
        });
        model::draft::Store::load(directory).unwrap()
    }

    fn forget_closed_view(reader: &mut VisualTestContext, view: Entity<FeedbackView>) {
        reader.update(|window, cx| {
            let draft = cx
                .global_mut::<Drafts>()
                .0
                .remove(&window.window_handle().window_id())
                .unwrap();
            assert_eq!(draft.view, view);
        });
        drop(view);
        draw(reader);
    }

    #[gpui::test]
    fn feedback28_cancel_keeps_durable_draft_until_ack_and_retry_preserves_receipt(
        cx: &mut TestAppContext,
    ) {
        let (f, reader) = mount(cx);
        let path = f.directory.path().join("selected.webm");
        std::fs::write(&path, b"original video").unwrap();
        let (view, mut feedback) = open(reader);
        feedback.simulate_input("Keep this feedback Ω");
        choose(&mut feedback, &path, true);
        let selected = view.read_with(&feedback, |view, _| view.video.clone().unwrap());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        view.update(&mut feedback, |view, _| {
            let cancellation = view.send_cancel.clone();
            view.sender = Arc::new(move |_| {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let signal = cancellation.lock().unwrap().clone().expect("real send cancellation signal");
                assert!(matches!(signal.now_or_never(), Some(Err(_))), "real Cancel action aborted this attempt");
                anyhow::bail!("Sending cancelled. The server may already have received the report; check before retrying.")
            });
        });
        let (started, release) = hold(&view);
        click(&mut feedback, "feedback-send");
        started
            .try_recv()
            .expect("real Send reached worker scheduling boundary");
        assert!(saved(&view, &feedback)
            .1
            .status
            .contains("Delivery is uncertain"));
        click(&mut feedback, "feedback-cancel");
        view.read_with(&feedback, |view, cx| {
            assert!(matches!(view.state, SendState::Sending));
            assert!(view.send_abort.as_ref().unwrap().is_aborted());
            assert!(
                !view.can_send(cx),
                "Retry waits for transport acknowledgement"
            );
            assert_eq!(view.video.as_ref(), Some(&selected));
        });
        click(&mut feedback, "feedback-send");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        release.try_send(()).unwrap();
        draw(&mut feedback);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let (_, recovered) = saved(&view, &feedback);
        assert_eq!(recovered.text, "Keep this feedback Ω");
        assert_eq!(recovered.video.as_ref(), Some(&selected));
        assert!(recovered
            .status
            .contains("server may already have received"));
        let old_id = view.entity_id();
        feedback.simulate_keystrokes("escape");
        draw(reader);
        let (reopened, mut feedback) = open(reader);
        assert_eq!(
            reopened.entity_id(),
            old_id,
            "ordinary reopen retains this failed draft"
        );
        reopened.update(&mut feedback, |view, _| {
            view.sender = Arc::new(|report| {
                assert_eq!(report.text, "Keep this feedback Ω");
                Ok(receipt())
            });
        });
        click(&mut feedback, "feedback-send");
        assert!(feedback.debug_bounds("feedback-issue").is_some());
        let (_, sent) = saved(&reopened, &feedback);
        assert!(sent.sent && sent.status.contains(&receipt().issue_url));
        assert!(
            model::draft::discover(&f.drafts).unwrap().0.is_empty(),
            "sent receipt is retained and excluded from retry discovery"
        );
        assert_eq!(
            std::fs::read_to_string(&f.note).unwrap(),
            "# Feedback fixture\n"
        );
    }

    #[gpui::test]
    fn feedback28_changed_video_completion_blocks_retry_until_picker_reselection(
        cx: &mut TestAppContext,
    ) {
        let (f, reader) = mount(cx);
        let path = f.directory.path().join("selected.webm");
        std::fs::write(&path, b"original").unwrap();
        let (view, mut feedback) = open(reader);
        feedback.simulate_input("Keep exact selected video");
        choose(&mut feedback, &path, true);
        let selected = view.read_with(&feedback, |view, _| view.video.clone().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::<Report>::new()));
        let record = requests.clone();
        view.update(&mut feedback, |view, _| {
            view.sender = Arc::new(move |report| {
                record.lock().unwrap().push(report);
                anyhow::bail!("offline provider failure")
            });
        });
        let (started, release) = hold(&view);
        click(&mut feedback, "feedback-send");
        started.try_recv().unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, b"replaced").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        release.try_send(()).unwrap();
        draw(&mut feedback);
        assert!(feedback.debug_bounds("feedback-video-error").is_some());
        view.read_with(&feedback, |view, cx| {
            assert!(matches!(view.state, SendState::Failed(_)));
            assert!(view.video_needs_reselect && !view.can_send(cx));
            assert_eq!(
                view.video.as_ref(),
                Some(&selected),
                "completion preserves authored identity"
            );
        });
        click(&mut feedback, "feedback-send");
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "blocked Retry cannot call the provider"
        );
        assert_eq!(saved(&view, &feedback).1.video.as_ref(), Some(&selected));
        choose(&mut feedback, &path, true);
        let reselected = view.read_with(&feedback, |view, cx| {
            assert!(view.can_send(cx));
            assert!(!view.video_needs_reselect && view.video_error.is_none());
            view.video.clone().unwrap()
        });
        assert_ne!(reselected, selected);
        let record = requests.clone();
        view.update(&mut feedback, |view, _| {
            view.sender = Arc::new(move |report| {
                record.lock().unwrap().push(report);
                Ok(receipt())
            });
        });
        click(&mut feedback, "feedback-send");
        assert!(feedback.debug_bounds("feedback-issue").is_some());
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].video.as_ref(), Some(&selected));
        assert_eq!(requests[1].video.as_ref(), Some(&reselected));
        assert_eq!(
            std::fs::read_to_string(&f.note).unwrap(),
            "# Feedback fixture\n"
        );
    }

    #[gpui::test]
    fn feedback28_restart_missing_video_keeps_choice_until_actual_remove(cx: &mut TestAppContext) {
        let (f, reader) = mount(cx);
        let path = f.directory.path().join("selected.mov");
        std::fs::write(&path, b"original video").unwrap();
        let (view, mut feedback) = open(reader);
        feedback.simulate_input("Restart recovery Ω");
        choose(&mut feedback, &path, true);
        let selected = view.read_with(&feedback, |view, _| view.video.clone().unwrap());
        feedback.simulate_keystrokes("escape");
        draw(reader);
        forget_closed_view(reader, view);
        std::fs::remove_file(&path).unwrap();
        let (view, mut feedback) = open(reader);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let record = calls.clone();
        view.update(&mut feedback, |view, _| {
            view.sender = Arc::new(move |report| {
                record.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                assert_eq!(report.text, "Restart recovery Ω");
                assert!(report.video.is_none());
                Ok(receipt())
            });
        });
        click(&mut feedback, "feedback-reopen-draft-0");
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "explicit recovery never auto-sends"
        );
        view.read_with(&feedback, |view, cx| {
            assert_eq!(view.text.read(cx).value(), "Restart recovery Ω");
            assert_eq!(view.video.as_ref(), Some(&selected));
            assert!(view.video_needs_reselect && !view.can_send(cx));
        });
        click(&mut feedback, "feedback-send");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        click(&mut feedback, "feedback-remove-video");
        assert!(saved(&view, &feedback).1.video.is_none());
        click(&mut feedback, "feedback-send");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(feedback.debug_bounds("feedback-issue").is_some());
        assert_eq!(
            std::fs::read_to_string(&f.note).unwrap(),
            "# Feedback fixture\n"
        );
    }

    #[gpui::test]
    fn feedback28_restart_changed_screenshot_keeps_identity_until_actual_remove(
        cx: &mut TestAppContext,
    ) {
        let (f, reader) = mount(cx);
        let path = f.directory.path().join("selected.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([30, 60, 90, 255]))
            .save(&path)
            .unwrap();
        let (view, mut feedback) = open(reader);
        feedback.simulate_input("Keep screenshot choice");
        choose(&mut feedback, &path, false);
        feedback.simulate_keystrokes("escape");
        draw(reader);
        let (store, before) = saved(&view, &feedback);
        let identity = before.screenshot.unwrap();
        forget_closed_view(reader, view);
        std::fs::write(store.directory.join(&identity), b"changed screenshot bytes").unwrap();
        let (view, mut feedback) = open(reader);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let record = calls.clone();
        view.update(&mut feedback, |view, _| {
            view.sender = Arc::new(move |report| {
                record.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                assert_eq!(report.text, "Keep screenshot choice");
                assert!(report.screenshot.is_none());
                Ok(receipt())
            });
        });
        click(&mut feedback, "feedback-reopen-draft-0");
        view.read_with(&feedback, |view, cx| {
            assert_eq!(view.unavailable_screenshot.as_ref(), Some(&identity));
            assert!(view.image.is_none() && view.image_error.is_some());
            assert!(!view.can_send(cx));
        });
        click(&mut feedback, "feedback-send");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        click(&mut feedback, "feedback-remove");
        assert!(saved(&view, &feedback).1.screenshot.is_none());
        click(&mut feedback, "feedback-send");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(feedback.debug_bounds("feedback-issue").is_some());
        assert_eq!(
            std::fs::read_to_string(&f.note).unwrap(),
            "# Feedback fixture\n"
        );
    }
}
