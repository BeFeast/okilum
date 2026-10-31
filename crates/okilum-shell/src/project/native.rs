//! Linux live history. The background task owns the socket; the retained model
//! and cursor are paired under a mutex, including during concurrent history loads.
use super::*;
mod actions;
use okilum_t3::{
    projection::{Metadata, Projection},
    stream::{
        CancelSubscription, NativeClient, NativeToken, ResumeCursor, StreamError,
        ThreadSubscription,
    },
};
use std::sync::Mutex;

struct Retained {
    model: Projection,
    cursor: Option<ResumeCursor>,
    valid: bool,
    epoch: u64,
}
pub(super) struct Native {
    input: Entity<InputState>,
    pub(super) actions: Entity<actions::Actions>,
    open: bool,
    busy: bool,
    error: Option<String>,
    stream_error: Option<String>,
    stream_ready: bool,
    expires_at: Option<u64>,
    client: Option<Arc<NativeClient>>,
    retained: Option<Arc<Mutex<Retained>>>,
    cancel: Option<CancelSubscription>,
    worker: Option<Task<()>>,
    history_busy: bool,
    metadata: Metadata,
}
impl Native {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ProjectView>) -> Self {
        let parent = cx.entity().downgrade();
        let actions = cx.new(|_| actions::Actions::new(parent));
        Self {
            actions,
            input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("One-time T3 pairing token")
                    .masked(true)
            }),
            open: false,
            busy: false,
            error: None,
            stream_error: None,
            stream_ready: false,
            expires_at: None,
            client: None,
            retained: None,
            cancel: None,
            worker: None,
            history_busy: false,
            metadata: Metadata::default(),
        }
    }
    pub(super) fn stream_error_is_clear(&self) -> bool {
        self.stream_ready && self.stream_error.is_none()
    }
    pub(super) fn client_can_operate(&self) -> bool {
        self.stream_ready && self.client.as_ref().is_some_and(|c| c.can_operate())
    }
    pub(super) fn deleted(&self) -> bool {
        self.metadata.deleted
    }
    pub(super) fn active(&self) -> bool {
        self.client.is_some()
    }
    pub(super) fn connection(&self) -> Option<(Arc<NativeClient>, u64)> {
        self.client.clone().zip(self.expires_at)
    }
    pub(super) fn model_client(&self) -> Option<Arc<NativeClient>> {
        self.client.clone()
    }
    pub(super) fn current_model(&self, _: &str) -> Option<&str> {
        (!self.metadata.model.is_empty()).then_some(self.metadata.model.as_str())
    }
    pub(super) fn title(&self) -> &str {
        &self.metadata.title
    }
    pub(super) fn status_label(&self, cx: &App) -> &'static str {
        if self.busy {
            return "Connecting";
        }
        if self.client.is_none() || self.error.is_some() || self.stream_error.is_some() {
            return "Offline";
        }
        let state = &self.actions.read(cx).state;
        if !self.stream_ready {
            "Connecting"
        } else if !state.requests.is_empty() {
            "Waiting"
        } else if state.active_run.is_some() {
            "Working"
        } else {
            "Connected"
        }
    }
    pub(super) fn renew_due(&self) -> bool {
        self.expires_at.is_some_and(|expiry| {
            expiry.saturating_sub(chrono::Utc::now().timestamp().max(0) as u64)
                <= okilum_t3::EXPIRES_SOON_SECS
        })
    }
    pub(super) fn pause(&mut self) {
        self.stream_ready = false;
        if let Some(retained) = &self.retained {
            retained.lock().unwrap().epoch += 1;
        }
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.worker.take();
    }
    pub(super) fn clear(&mut self) {
        self.pause();
        self.client = None;
        self.retained = None;
        self.error = None;
        self.stream_error = None;
        self.metadata = Metadata::default();
        self.busy = false;
        self.history_busy = false;
        self.open = false;
        self.expires_at = None;
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        self.pause();
    }
}
impl ProjectView {
    pub(super) fn native_pairing_busy(&self) -> bool {
        self.native.busy
    }
    pub(super) fn pair_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.environment.is_none() {
            match self.connection_environment(cx) {
                Ok(environment) => self.environment = Some(environment),
                Err(_) => {
                    reader_settings::show_t3(cx);
                    return;
                }
            }
        }
        self.native_restore(true, window, cx);
    }

    pub(super) fn native_restore(
        &mut self,
        ask: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
        let access = reader_features::access(reader_features::Feature::Agents, cx);
        if self.native.busy {
            return;
        }
        let Some(environment) = self.environment.clone() else {
            return;
        };
        self.native.busy = true;
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let task = match this.update_in(cx, |_, _, cx| {
                cx.read_credentials(&NativeToken::credential_key(&environment))
            }) {
                Ok(task) => task,
                Err(_) => return,
            };
            let result = match task.await {
                Ok(Some((_, bytes))) => NativeToken::from_secret(&environment, &bytes).map(Some),
                Ok(None) => Ok(None),
                Err(_) => Err(StreamError::Protocol(
                    "the saved live connection could not be read; pair again explicitly",
                )),
            };
            let result = match result {
                Ok(Some(token)) => {
                    cx.background_executor()
                        .spawn(async move {
                            let expiry = token.expires_at();
                            NativeClient::new(environment, token, Duration::from_secs(10))
                                .map(|client| Some((Arc::new(client.with_access(access)), expiry)))
                        })
                        .await
                }
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                this.native.busy = false;
                match result {
                    Ok(Some((client, expiry))) => this.install_native(client, expiry, window, cx),
                    Ok(None) => {
                        if ask {
                            this.pair_enrolled_device(window, cx);
                        }
                        cx.notify();
                    }
                    Err(error) => {
                        this.native.error = Some(error.to_string());
                        if ask {
                            this.pair_enrolled_device(window, cx);
                        }
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        if ask {
            cx.notify();
        }
    }
    fn native_pair(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let credential = self.native.input.read(cx).value().to_string();
        self.pair_native_credential(credential, window, cx);
    }
    fn pair_enrolled_device(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        self.native.busy = true;
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let task = match this.update_in(cx, |_, _, cx| {
                cx.read_credentials(&NativeToken::pairing_key(&environment))
            }) {
                Ok(task) => task,
                Err(_) => return,
            };
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                this.native.busy = false;
                match result {
                    Ok(Some((_, bytes))) => match String::from_utf8(bytes) {
                        Ok(credential) => this.pair_native_credential(credential, window, cx),
                        Err(_) => {
                            this.native.error =
                                Some("The saved device enrollment could not be read".into())
                        }
                    },
                    _ => this.native.error = Some(
                        "This device has not been enrolled with T3. Open Diagnostics to connect."
                            .into(),
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn pair_native_credential(
        &mut self,
        credential: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
        let access = reader_features::access(reader_features::Feature::Agents, cx);
        if self.native.busy {
            return;
        }
        let Some(environment) = self.environment.clone() else {
            return;
        };
        if credential.trim().is_empty() {
            self.native.error = Some("Enter a one-time T3 pairing token".into());
            cx.notify();
            return;
        }
        self.native
            .input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.native.busy = true;
        self.native.error = None;
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let env = environment.clone();
            let pair_access = access.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let _access = pair_access.enter();
                    NativeToken::pair(&env, &credential, Duration::from_secs(10))
                })
                .await;
            let result = match result {
                Ok(token) => {
                    let expiry = token.expires_at();
                    let bytes = token.to_secret();
                    let task = match this.update_in(cx, |this, _, cx| {
                        if this.generation != generation {
                            return None;
                        }
                        Some(cx.write_credentials(
                            &NativeToken::credential_key(&environment),
                            &environment.alias,
                            &bytes,
                        ))
                    }) {
                        Ok(Some(task)) => task,
                        _ => return,
                    };
                    match task.await {
                        Ok(()) => {
                            if let Ok(cleanup) = this.update_in(cx, |_, _, cx| {
                                cx.delete_credentials(&NativeToken::pairing_key(&environment))
                            }) {
                                let _ = cleanup.await;
                            }
                            cx.background_executor()
                                .spawn(async move {
                                    NativeClient::new(environment, token, Duration::from_secs(10))
                                        .map(|client| {
                                            (Arc::new(client.with_access(access)), expiry)
                                        })
                                })
                                .await
                        }
                        Err(_) => Err(StreamError::Protocol(
                            "the live connection could not be saved",
                        )),
                    }
                }
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                this.native.busy = false;
                match result {
                    Ok((client, expiry)) => this.install_native(client, expiry, window, cx),
                    Err(error) => {
                        this.native.error = Some(error.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn install_native(
        &mut self,
        client: Arc<NativeClient>,
        expiry: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(binding) = &self.binding else {
            return;
        };
        let target = binding
            .manager
            .thread_id
            .clone()
            .zip(binding.manager.project_id.clone());
        if self.threads.enabled {
            self.set_threads_active(false, window, cx);
        }
        self.native.pause();
        self.native.client = Some(client);
        self.native.error = None;
        self.native.stream_error = None;
        self.native.expires_at = Some(expiry);
        self.native.open = false;
        if self.native.retained.is_none() {
            if let Some((thread, project)) = target {
                self.native.retained = Some(Arc::new(Mutex::new(Retained {
                    model: Projection::new(&thread, &project),
                    cursor: None,
                    valid: true,
                    epoch: 0,
                })));
            }
        }
        self.native_load_actions(window, cx);
        self.start_thread_directory(window, cx);
        self.rebind_selected_native_thread(window, cx);
        self.native_start(window, cx);
        cx.notify();
    }
    pub(super) fn native_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.active || self.native.worker.is_some() || self.threads.selected.is_some() {
            return;
        }
        let Some(client) = self.native.client.clone() else {
            return;
        };
        if !client.access_allowed() {
            return;
        }
        let Some(retained) = self.native.retained.clone() else {
            return;
        };
        let Some(thread) = self.chat.as_ref().map(|chat| chat.thread_id.clone()) else {
            return;
        };
        let epoch = {
            let mut r = retained.lock().unwrap();
            r.epoch += 1;
            r.epoch
        };
        self.native.worker = Some(cx.spawn_in(window, async move |this, cx| {
            let mut socket: Option<ThreadSubscription> = None;
            let mut failures = 0u32;
            loop {
                if socket.is_none() {
                    let cursor = {
                        let r = retained.lock().unwrap();
                        if r.epoch != epoch {
                            return;
                        }
                        r.valid.then(|| r.cursor.clone()).flatten()
                    };
                    let connect_client = client.clone();
                    let connect_thread = thread.clone();
                    match cx
                        .background_executor()
                        .spawn(async move { connect_client.subscribe(&connect_thread, cursor) })
                        .await
                    {
                        Ok(stream) => {
                            let cancel = stream.cancellation().ok();
                            if this
                                .update_in(cx, |this, _, _| {
                                    this.native.cancel = cancel;
                                })
                                .is_err()
                            {
                                return;
                            }
                            socket = Some(stream);
                        }
                        Err(error) => {
                            let terminal = terminal(&error);
                            let message = notice(&error);
                            let _ = this.update_in(cx, |this, _, cx| {
                                if this.native.stream_error.as_ref() != Some(&message) {
                                    this.native.stream_error = Some(message);
                                    cx.notify();
                                }
                            });
                            if terminal {
                                return;
                            }
                            failures = failures.saturating_add(1);
                            cx.background_executor().timer(delay(failures)).await;
                            continue;
                        }
                    }
                }
                let mut stream = socket.take().unwrap();
                let model = retained.clone();
                let (stream, result) = cx
                    .background_executor()
                    .spawn(async move {
                        let result = stream.receive(|updates| {
                            let mut r = model.lock().unwrap();
                            if r.epoch != epoch {
                                return Err(StreamError::Closed);
                            }
                            r.model.apply(updates)?;
                            r.valid = false;
                            Ok(())
                        });
                        {
                            let mut r = model.lock().unwrap();
                            if r.epoch == epoch {
                                r.cursor = stream.cursor();
                                r.valid = true;
                            }
                        }
                        (stream, result)
                    })
                    .await;
                let terminal_error = result.as_ref().err().is_some_and(terminal);
                let message = result.as_ref().err().map(notice);
                let changed = this
                    .update_in(cx, |this, window, cx| {
                        let mut r = retained.lock().unwrap();
                        if r.epoch != epoch {
                            return false;
                        }
                        let mut changes = r.model.take_changes();
                        let controls_changed = changes.actions.is_some();
                        if let Some(state) = changes.actions.take() {
                            this.native
                                .actions
                                .update(cx, |a, cx| a.set_state(state, window, cx));
                        }
                        let metadata = changes.metadata.clone();
                        let changed = changes.changed();
                        if changed {
                            this.feed.update(cx, |feed, cx| {
                                feed.apply_native(changes, &this.records, cx)
                            });
                        }
                        let ready = result.is_ok();
                        let mut notify = controls_changed
                            || this.native.stream_error != message
                            || this.native.stream_ready != ready;
                        this.native.stream_ready = ready;
                        this.native.stream_error = message;
                        if let Some(metadata) = metadata {
                            notify |= this.native.metadata != metadata;
                            this.native.metadata = metadata;
                        }
                        if notify {
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !changed || terminal_error {
                    return;
                }
                if result.is_ok() {
                    failures = 0;
                    socket = Some(stream);
                } else {
                    failures = failures.saturating_add(1);
                    drop(stream);
                    cx.background_executor().timer(delay(failures)).await;
                }
            }
        }));
    }
    fn native_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.native.history_busy {
            return;
        }
        let Some(client) = self.native.client.clone() else {
            return;
        };
        let Some(retained) = self.native.retained.clone() else {
            return;
        };
        let Some(request) = retained.lock().unwrap().model.history_request() else {
            return;
        };
        let Some(thread) = self.chat.as_ref().map(|chat| chat.thread_id.clone()) else {
            return;
        };
        self.native.history_busy = true;
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let model = retained.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let page = client.history(&thread, request.cursor())?;
                    model.lock().unwrap().model.apply_history(&request, page)
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.generation != generation {
                    return;
                }
                this.native.history_busy = false;
                match result {
                    Ok(()) => {
                        let mut r = retained.lock().unwrap();
                        let changes = r.model.take_changes();
                        if let Some(metadata) = &changes.metadata {
                            this.native.metadata = metadata.clone();
                        }
                        this.feed
                            .update(cx, |feed, cx| feed.apply_native(changes, &this.records, cx));
                    }
                    Err(error) => this.native.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn render_native(&self, cx: &mut Context<Self>) -> AnyElement {
        let compact = !self.connection_details;
        let enabled = self.native.client_can_operate() && self.native.stream_error.is_none();
        self.native
            .actions
            .update(cx, |a, cx| a.set_enabled(enabled, cx));
        let mut controls = h_flex().flex_wrap().gap_2().child(
            Button::new("project-live-connect")
                .ghost()
                .label(if self.native.active() {
                    "Reconnect live updates"
                } else {
                    "Enable live updates"
                })
                .debug_selector(|| "project-live-connect".into())
                .disabled(self.native.busy || self.environment.is_none())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.native.pause();
                    this.native_restore(true, window, cx);
                })),
        );
        if self.native.active() {
            controls = controls.child(
                Button::new("project-live-pair-again")
                    .ghost()
                    .label("Pair again")
                    .disabled(self.native.busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.native.open = true;
                        cx.notify();
                    })),
            );
        }
        let mut view = v_flex().w_full().min_w_0().gap_1();
        if !compact {
            view = view.child(controls);
        }
        if self.native.metadata.has_more_history && self.native.metadata.history_cursor.is_some() {
            view = view.child(
                Button::new("project-live-history")
                    .ghost()
                    .label("Load earlier activity")
                    .disabled(self.native.history_busy)
                    .on_click(cx.listener(|this, _, window, cx| this.native_history(window, cx))),
            );
        }
        if self.native.open && !compact {
            view=view.child(div().w_full().min_w_0().whitespace_normal()
                .child("Live updates need a separate T3 client pairing with permission to read threads and control agents. Paste its one-time pairing token; the saved connection is reused until it expires."))
                .child(div().w_full().min_w_0().debug_selector(||"project-live-token-row".into()).child(Input::new(&self.native.input).w_full()))
                .child(Button::new("project-live-pair").ghost().label("Pair").disabled(self.native.busy)
                    .on_click(cx.listener(|this,_,window,cx|this.native_pair(window,cx))));
            if let Some(origin) = self.environment.as_ref().map(Environment::origin) {
                view = view.child(
                    Button::new("project-live-open-browser")
                        .ghost()
                        .label("Open T3")
                        .on_click(move |_, _, cx| cx.open_url(&origin)),
                );
            }
        }
        if let Some(expiry) = self.native.expires_at.filter(|_| !compact) {
            let renew = self.native.renew_due();
            view = view.child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .text_color(brand::palette(cx).text_muted)
                    .child(format!(
                        "Live {}{}",
                        super::expiry_note(expiry).trim_start_matches("Connection "),
                        if renew {
                            " — pair again to renew"
                        } else {
                            ""
                        }
                    )),
            );
        }
        if self.native.metadata.deleted {
            view = view.child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .debug_selector(|| "project-live-deleted".into())
                    .child("This manager thread was deleted. Open T3 to choose another manager."),
            );
        }
        if self.native.metadata.payload_budget_exceeded {
            view =
                view.child(div().w_full().min_w_0().whitespace_normal().child(
                    "The latest activity window is incomplete. Open T3 for the full thread.",
                ));
        }
        if let Some(error) = self
            .native
            .error
            .clone()
            .or_else(|| self.native.stream_error.clone())
        {
            view = view.child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .debug_selector(|| "project-live-error".into())
                    .text_color(cx.theme().danger)
                    .child(if compact {
                        "Connection interrupted. Open Diagnostics to reconnect.".to_owned()
                    } else {
                        error
                    }),
            );
        }
        view.child(self.native.actions.clone()).into_any_element()
    }
}
fn notice(error: &StreamError) -> String {
    match error {
        StreamError::Closed | StreamError::Unreachable => {
            "Connection interrupted. Reconnecting live updates…".into()
        }
        StreamError::PairingRequired | StreamError::Expired => {
            "This live connection is no longer signed in. Pair again to continue.".into()
        }
        StreamError::Protocol(_) | StreamError::IncompatibleProtocol => {
            "Live updates are unavailable with this T3 server. Open T3 to continue.".into()
        }
        _ => error.to_string(),
    }
}

fn terminal(error: &StreamError) -> bool {
    match error {
        StreamError::Closed | StreamError::Unreachable => false,
        StreamError::Http(status) => !matches!(*status, 408 | 429 | 500..=599),
        _ => true,
    }
}

fn delay(attempt: u32) -> Duration {
    Duration::from_secs((1u64 << attempt.saturating_sub(1).min(4)).min(15))
}
#[cfg(test)]
mod tests;
