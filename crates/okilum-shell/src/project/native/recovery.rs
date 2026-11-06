//! Conversation-local recovery preserves the retained transport and action scope.
use super::recovery_state::{ErrorDismissKey, ErrorOrigin, SessionDismissals};
use super::*;
use gpui_component::popover::Popover;

struct ErrorDetailsState {
    focus: gpui::FocusHandle,
    scroll: gpui::ScrollHandle,
}

/// TestScheduler polls blocking work on its own loop. A live socket waiting for
/// the test's next input would otherwise stop that input from being delivered.
/// Production keeps its existing background-executor path; test I/O is bounded
/// by the same client timeouts and cancellation sockets.
pub(in crate::project) async fn stream_io<T: Send + 'static>(
    executor: &gpui::BackgroundExecutor,
    operation: impl FnOnce() -> T + Send + 'static,
) -> T {
    #[cfg(test)]
    {
        let _ = executor;
        let (send, receive) = futures::channel::oneshot::channel();
        let thread = std::thread::spawn(move || {
            let _ = send.send(operation());
        });
        let result = receive.await.expect("native test I/O thread completed");
        thread.join().expect("native test I/O thread did not panic");
        result
    }
    #[cfg(not(test))]
    executor.spawn(async move { operation() }).await
}

/// The mask is presentation-only. Neither dismissal nor opening details mutates
/// the canonical error, action records, readiness, or a retained Retry command.
pub(super) fn error_banner<T: 'static>(
    scope: Option<(String, String)>,
    error_origin: ErrorOrigin,
    full_safe_text: String,
    preview: Option<String>,
    selector: &'static str,
    window: &mut Window,
    cx: &mut Context<T>,
) -> Option<AnyElement> {
    let key = scope.map(|(origin, thread)| {
        ErrorDismissKey::new(&origin, &thread, error_origin, &full_safe_text)
    });
    if key.as_ref().is_some_and(|key| {
        cx.try_global::<SessionDismissals>()
            .is_some_and(|session| session.is_dismissed(key))
    }) {
        return None;
    }
    let preview = preview.unwrap_or_else(|| full_safe_text.clone());
    let preview_line_height = window.line_height();
    let preview_selector = format!("{selector}-preview");
    let details_selector = format!("{selector}-details-body");
    let details_id = SharedString::from(format!(
        "error-details:{selector}:{key:?}:{full_safe_text:?}"
    ));
    let state = window.use_keyed_state(details_id.clone(), cx, |_, cx| ErrorDetailsState {
        focus: cx.focus_handle(),
        scroll: gpui::ScrollHandle::new(),
    });
    let scroll = state.read(cx).scroll.clone();
    let details_focus = state.read(cx).focus.clone();
    let content_focus = details_focus.clone();
    let full = full_safe_text.clone();
    let details = Popover::new(details_id)
        .track_focus(&details_focus)
        .trigger(
            Button::new(SharedString::from(format!("{selector}-details-button")))
                .debug_selector(move || format!("{selector}-details-button"))
                .ghost()
                .label("Details")
                .tooltip(full_safe_text.clone()),
        )
        .content(move |_, _, cx| {
            let keyboard_scroll = scroll.clone();
            div()
                .id("error-full-text")
                .track_focus(&content_focus)
                .debug_selector({
                    let selector = details_selector.clone();
                    move || selector.clone()
                })
                .w(px(480.))
                .max_w_full()
                .max_h(px(280.))
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .on_key_down(cx.listener(move |_, event: &gpui::KeyDownEvent, _, cx| {
                    let delta = match event.keystroke.key.as_str() {
                        "up" => 24.,
                        "down" => -24.,
                        "pageup" => 240.,
                        "pagedown" => -240.,
                        _ => return,
                    };
                    let mut offset = keyboard_scroll.offset();
                    offset.y += px(delta);
                    keyboard_scroll.set_offset(offset);
                    cx.notify();
                    cx.stop_propagation();
                }))
                .child(
                    div()
                        .debug_selector(move || format!("{selector}-details-text"))
                        .whitespace_normal()
                        .child(full.clone()),
                )
        });
    Some(
        v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .debug_selector(move || selector.to_owned())
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .line_clamp(3)
                    // GPUI's line clamp does not count explicit newlines.
                    // Define and bound the same three actual line boxes.
                    .line_height(preview_line_height)
                    .max_h(preview_line_height * 3.)
                    .overflow_hidden()
                    .text_color(cx.theme().danger)
                    .debug_selector(move || preview_selector.clone())
                    .child(preview),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        div()
                            .key_context("ProjectErrorDetailsTrigger")
                            .child(details),
                    )
                    .child(
                        div().key_context("ProjectErrorDismiss").child(
                            Button::new(SharedString::from(format!("{selector}-dismiss")))
                                .debug_selector(move || format!("{selector}-dismiss"))
                                .ghost()
                                .label("Dismiss")
                                .disabled(key.is_none())
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    if let Some(key) = key.clone() {
                                        cx.default_global::<SessionDismissals>().dismiss(key);
                                        cx.notify();
                                    }
                                })),
                        ),
                    ),
            )
            .into_any_element(),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConnectionPhase {
    Connecting,
    Connected,
    Reconnecting,
    Terminal,
    Paused,
}

/// Credential completions also need an epoch before a retained model exists.
pub(super) struct ConnectionAttempt {
    generation: u64,
    epoch: u64,
    origin: String,
    thread: Option<String>,
}

impl ConnectionAttempt {
    pub(super) fn begin(view: &mut ProjectView, origin: String) -> Self {
        view.native.connection_epoch = view.native.connection_epoch.wrapping_add(1);
        Self {
            generation: view.generation,
            epoch: view.native.connection_epoch,
            origin,
            thread: view.chat.as_ref().map(|chat| chat.thread_id.clone()),
        }
    }

    pub(super) fn current(&self, view: &ProjectView) -> bool {
        view.generation == self.generation
            && view.native.connection_epoch == self.epoch
            && view
                .environment
                .as_ref()
                .map(Environment::origin)
                .as_deref()
                == Some(self.origin.as_str())
            && view.chat.as_ref().map(|chat| &chat.thread_id) == self.thread.as_ref()
    }
}

pub(super) struct ConnectionSource {
    generation: u64,
    connection_epoch: u64,
    client: Arc<NativeClient>,
    retained: Arc<Mutex<Retained>>,
    thread: String,
    pub(super) epoch: u64,
}

impl ConnectionSource {
    pub(super) fn new(
        view: &ProjectView,
        client: Arc<NativeClient>,
        retained: Arc<Mutex<Retained>>,
        thread: String,
        epoch: u64,
    ) -> Self {
        Self {
            generation: view.generation,
            connection_epoch: view.native.connection_epoch,
            client,
            retained,
            thread,
            epoch,
        }
    }

    /// Does not lock Retained: receive already holds that mutex while applying.
    pub(super) fn same_source(&self, view: &ProjectView) -> bool {
        view.generation == self.generation
            && view.native.client.as_ref().is_some_and(|client| {
                Arc::ptr_eq(client, &self.client)
                    && view.environment.as_ref().map(Environment::origin) == Some(client.origin())
            })
            && view
                .native
                .retained
                .as_ref()
                .is_some_and(|retained| Arc::ptr_eq(retained, &self.retained))
            && view.chat.as_ref().map(|chat| chat.thread_id.as_str()) == Some(self.thread.as_str())
    }

    pub(super) fn matches(&self, view: &ProjectView) -> bool {
        self.same_source(view) && view.native.connection_epoch == self.connection_epoch
    }

    pub(super) fn current(&self, view: &ProjectView) -> bool {
        self.matches(view) && self.retained.lock().unwrap().epoch == self.epoch
    }
}

impl ProjectView {
    pub(in crate::project) fn native_setup_error_banner(
        &self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.native_error_banner(ErrorOrigin::Setup, text, None, "project-error", window, cx)
    }

    pub(super) fn native_error_banner(
        &self,
        origin: ErrorOrigin,
        text: String,
        preview: Option<String>,
        selector: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let scope = self
            .environment
            .as_ref()
            .zip(self.chat.as_ref())
            .map(|(environment, chat)| (environment.origin(), chat.thread_id.clone()));
        error_banner(scope, origin, text, preview, selector, window, cx)
    }

    pub(super) fn render_native_recovery(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut view = v_flex().w_full().min_w_0();
        if let Some(banner) = self.native.history_error.as_ref().and_then(|error| {
            self.native_error_banner(
                ErrorOrigin::History,
                format!("Could not load earlier messages: {error}"),
                None,
                "project-history-error",
                window,
                cx,
            )
        }) {
            view = view.child(banner);
        }
        view.into_any_element()
    }
    pub(super) fn freeze_native_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.native.stream_ready = false;
        self.native.actions.update(cx, |actions, cx| {
            let mut state = actions.state.clone();
            state.ready = false;
            actions.set_state(state, window, cx);
            actions.set_enabled(false, cx);
        });
    }

    pub(super) fn native_retry_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.native.busy {
            return;
        }
        self.native.pause();
        self.freeze_native_actions(window, cx);
        self.native_start(window, cx);
        cx.notify();
    }

    pub(super) fn native_failed(
        &mut self,
        cause: StreamError,
        context: ConnectionContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.freeze_native_actions(window, cx);
        self.native.phase = if recovery_state::is_transient(&cause) {
            ConnectionPhase::Reconnecting
        } else {
            ConnectionPhase::Terminal
        };
        if let Some((env, client)) = self.environment.as_ref().zip(self.native.client.as_ref()) {
            let stage = match &cause {
                StreamError::Expired | StreamError::PairingRequired => {
                    reader_t3_settings::manager::Stage::Expired
                }
                error if recovery_state::is_transient(error) => {
                    reader_t3_settings::manager::Stage::Offline
                }
                _ => reader_t3_settings::manager::Stage::Error,
            };
            reader_t3_settings::manager::transport(
                env,
                client.clone(),
                self.native.expires_at.unwrap_or(0),
                stage,
                Some(cause.to_string()),
                cx,
            );
        }
        self.native.failure = Some(ConnectionFailure { cause, context });
    }
}
