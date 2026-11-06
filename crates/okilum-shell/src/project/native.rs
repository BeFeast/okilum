//! Native live history. The background task owns the socket; the retained model
//! and cursor are paired under a mutex, including during concurrent history loads.
use super::*;
pub(super) mod actions;
mod onboarding;
pub(super) mod recovery;
mod recovery_state;
use okilum_t3::{
    projection::{Metadata, Projection},
    stream::{
        CancelSubscription, NativeClient, NativeToken, ResumeCursor, StreamError,
        ThreadSubscription, ThreadUpdate,
    },
};
use recovery::{ConnectionAttempt, ConnectionPhase, ConnectionSource};
use recovery_state::{ConnectionContext, ConnectionFailure};
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
    request_lease: Arc<()>,
    busy: bool,
    flow_blocked: bool,
    setup_error: Option<String>,
    setup_cause: Option<StreamError>,
    history_error: Option<String>,
    failure: Option<ConnectionFailure>,
    phase: ConnectionPhase,
    pub(super) connection_epoch: u64,
    retry_epoch: u64,
    ever_ready: bool,
    stream_ready: bool,
    expires_at: Option<u64>,
    client: Option<Arc<NativeClient>>,
    retained: Option<Arc<Mutex<Retained>>>,
    cancel: Option<CancelSubscription>,
    worker: Option<Task<()>>,
    history_busy: bool,
    history_epoch: u64,
    metadata: Metadata,
    pub(super) model_revision: u64,
    pub(super) catalog: Option<Entity<models::catalog::Catalog>>,
    pub(super) catalog_observer: Option<Subscription>,
    actions_visible: bool,
    _actions_visibility: Subscription,
}
impl Native {
    #[cfg(test)]
    pub(super) fn set_detail_test_expiry(&mut self, expiry: u64) {
        self.expires_at = Some(expiry);
    }
    #[cfg(test)]
    pub(super) fn set_detail_test_client(&mut self, client: Arc<NativeClient>) {
        self.client = Some(client);
    }
    pub(super) fn new(window: &mut Window, cx: &mut Context<ProjectView>) -> Self {
        let parent = cx.entity().downgrade();
        let actions = cx.new(|_| actions::Actions::new(parent));
        let visibility = cx.observe(&actions, |this, actions, cx| {
            let visible = actions.read(cx).has_visible_content(cx);
            if this.native.actions_visible != visible {
                this.native.actions_visible = visible;
                cx.notify();
            }
        });
        Self {
            actions,
            request_lease: Arc::new(()),
            input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Paste a T3 pair link or pairing code")
                    .masked(true)
            }),
            busy: false,
            flow_blocked: false,
            setup_error: None,
            setup_cause: None,
            history_error: None,
            failure: None,
            phase: ConnectionPhase::Paused,
            connection_epoch: 0,
            retry_epoch: 0,
            ever_ready: false,
            stream_ready: false,
            expires_at: None,
            client: None,
            retained: None,
            cancel: None,
            worker: None,
            history_busy: false,
            history_epoch: 0,
            metadata: Metadata::default(),
            model_revision: 0,
            catalog: None,
            catalog_observer: None,
            actions_visible: false,
            _actions_visibility: visibility,
        }
    }
    pub(super) fn stream_error_is_clear(&self) -> bool {
        !self.busy && self.stream_ready && self.failure.is_none()
    }
    pub(super) fn client_can_operate(&self) -> bool {
        !self.busy
            && !self.flow_blocked
            && self.stream_ready
            && self.client.as_ref().is_some_and(|c| c.can_operate())
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
    pub(super) fn current_model(&self, thread: &str) -> Option<&str> {
        (self.metadata.thread_id == thread && !self.metadata.model.is_empty())
            .then_some(self.metadata.model.as_str())
    }
    pub(super) fn current_selection(&self, thread: &str) -> Option<&okilum_t3::ModelSelection> {
        (self.metadata.thread_id == thread)
            .then_some(self.metadata.model_selection.as_ref())
            .flatten()
    }
    pub(super) fn configuration(&self, thread: &str) -> Option<okilum_t3::ThreadConfiguration> {
        Some(okilum_t3::ThreadConfiguration {
            thread_id: thread.into(),
            model_selection: self.current_selection(thread)?.clone(),
            runtime_mode: self.metadata.runtime_mode.clone()?,
            interaction_mode: self.metadata.interaction_mode.clone()?,
        })
    }
    pub(super) fn retain_metadata(&mut self, metadata: Metadata) {
        self.metadata = metadata;
    }
    pub(super) fn title(&self) -> &str {
        &self.metadata.title
    }
    pub(super) fn status_label(&self, cx: &App) -> &'static str {
        if self.busy {
            return "Connecting";
        }
        let state = &self.actions.read(cx).state;
        if !self.stream_ready {
            match self.phase {
                ConnectionPhase::Connecting => "Connecting",
                ConnectionPhase::Reconnecting => "Reconnecting",
                ConnectionPhase::Terminal | ConnectionPhase::Paused => "Offline",
                ConnectionPhase::Connected => "Connecting",
            }
        } else if !state.requests.is_empty() {
            "Waiting"
        } else if state.active_run.is_some() {
            "Working"
        } else {
            "Connected"
        }
    }
    pub(super) fn pause(&mut self) {
        self.stream_ready = false;
        self.catalog_observer.take();
        self.catalog.take();
        // This epoch owns credential setup as well as the live worker. A stale
        // completion cannot clear a newer attempt's busy flag.
        self.busy = false;
        self.history_busy = false;
        self.history_epoch = self.history_epoch.wrapping_add(1);
        self.phase = ConnectionPhase::Paused;
        self.connection_epoch = self.connection_epoch.wrapping_add(1);
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
        self.request_lease = Arc::new(());
        self.client = None;
        self.retained = None;
        self.setup_error = None;
        self.setup_cause = None;
        self.flow_blocked = false;
        self.history_error = None;
        self.failure = None;
        self.ever_ready = false;
        self.metadata = Metadata::default();
        self.busy = false;
        self.history_busy = false;
        self.expires_at = None;
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        self.pause();
    }
}
impl ProjectView {
    #[cfg(test)]
    pub(super) fn native_pairing_busy(&self) -> bool {
        self.native.busy
    }
    pub(super) fn pair_manager(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let environment = self
            .environment
            .clone()
            .or_else(|| self.connection_environment(cx).ok());
        if let Some(environment) = environment {
            self.native.pause();
            self.environment = Some(environment.clone());
            self.native.setup_error = None;
            self.native.setup_cause = None;
            reader_t3_settings::manager::begin(&environment, cx);
        } else {
            reader_settings::show_t3(cx);
        }
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
        self.freeze_native_actions(window, cx);
        self.native.phase = ConnectionPhase::Connecting;
        let attempt = ConnectionAttempt::begin(self, environment.origin());
        cx.spawn_in(window, async move |this, cx| {
            let task = match this.update_in(cx, |this, _, cx| {
                attempt.current(this).then(|| {
                    this.credentials
                        .read(&NativeToken::credential_key(&environment), cx)
                })
            }) {
                Ok(Some(task)) => task,
                _ => return,
            };
            let mut saved_expiry = None;
            let result = match task.await {
                Ok(Some((_, bytes))) => {
                    saved_expiry = NativeToken::saved_expiry(&environment, &bytes);
                    NativeToken::from_secret(&environment, &bytes).map(Some)
                }
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
                if !attempt.current(this) {
                    return;
                }
                this.native.busy = false;
                this.native.expires_at = saved_expiry;
                match result {
                    Ok(Some((client, expiry))) => this.install_native(client, expiry, window, cx),
                    Ok(None) => {
                        this.native.phase = ConnectionPhase::Paused;
                        this.draft_restore(window, cx);
                        if ask {
                            this.pair_manager(window, cx);
                        }
                        cx.notify();
                    }
                    Err(error) => {
                        this.native.phase = ConnectionPhase::Terminal;
                        this.native.setup_error = Some(error.to_string());
                        this.native.setup_cause = Some(error);
                        this.draft_restore(window, cx);
                        if ask {
                            this.pair_manager(window, cx);
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
        let gateway = match client.project_gateway(self.gateway.clone()) {
            Ok(gateway) => gateway,
            Err(error) => {
                self.native.setup_error = Some(error.to_string());
                cx.notify();
                return;
            }
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
        self.gateway = Some(gateway);
        self.native.client = Some(client);
        self.native.flow_blocked = false;
        self.attachments.connection_changed();
        self.native.setup_error = None;
        self.native.setup_cause = None;
        self.native.failure = None;
        self.native.expires_at = Some(expiry);
        self.sync_native_action_surface(cx);
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
        self.draft_restore(window, cx);
        self.native_load_actions(window, cx);
        self.start_thread_directory(window, cx);
        self.native_start(window, cx);
        cx.emit(NativeConnected);
        cx.notify();
    }
    pub(super) fn native_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.active || self.native.busy || self.native.worker.is_some() {
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
        if self.model_busy {
            self.model_busy = false;
            self.model_error = Some(
                "The connection changed before the model change was confirmed. No retry was sent."
                    .into(),
            );
        }
        self.native.connection_epoch = self.native.connection_epoch.wrapping_add(1);
        self.native.phase = if self.native.ever_ready {
            ConnectionPhase::Reconnecting
        } else {
            ConnectionPhase::Connecting
        };
        self.freeze_native_actions(window, cx);
        let source = ConnectionSource::new(
            self,
            client.clone(),
            retained.clone(),
            thread.clone(),
            epoch,
        );
        self.native.worker = Some(cx.spawn_in(window, async move |this, cx| {
            let mut socket: Option<ThreadSubscription> = None;
            let mut failures = 0u32;
            let mut synchronized = false;
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
                    match recovery::stream_io(cx.background_executor(), move || {
                        connect_client.subscribe(&connect_thread, cursor)
                    })
                    .await
                    {
                        Ok(stream) => {
                            let phases = stream.phases();
                            let cancel = stream.cancellation().ok();
                            if !this
                                .update_in(cx, |this, _, cx| {
                                    if !source.current(this) {
                                        return false;
                                    }
                                    this.native.cancel = cancel;
                                    this.ttfm.socket_connected(phases);
                                    // The ticket/upgrade authenticated this native
                                    // connection. Warm config alongside history.
                                    this.ensure_native_catalog(cx);
                                    true
                                })
                                .unwrap_or(false)
                            {
                                return;
                            }
                            socket = Some(stream);
                            synchronized = false;
                        }
                        Err(error) => {
                            let terminal = terminal(&error);
                            let current = this
                                .update_in(cx, |this, window, cx| {
                                    if !source.current(this) {
                                        return false;
                                    }
                                    this.native_failed(
                                        error,
                                        ConnectionContext::Connect,
                                        window,
                                        cx,
                                    );
                                    cx.notify();
                                    true
                                })
                                .unwrap_or(false);
                            if !current {
                                return;
                            }
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
                let (stream, result, accepted_sync, snapshot_at) =
                    recovery::stream_io(cx.background_executor(), move || {
                        let mut snapshot_at = None;
                        let result = stream.receive(|updates| {
                            let mut r = model.lock().unwrap();
                            if r.epoch != epoch {
                                return Err(StreamError::Closed);
                            }
                            r.model.apply(updates)?;
                            if updates
                                .iter()
                                .any(|update| matches!(update, ThreadUpdate::Snapshot { .. }))
                            {
                                snapshot_at = Some(std::time::Instant::now());
                            }
                            synchronized |= updates
                                .iter()
                                .any(|update| matches!(update, ThreadUpdate::Synchronized));
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
                        (stream, result, synchronized, snapshot_at)
                    })
                    .await;
                synchronized = accepted_sync;
                let terminal_error = result.as_ref().err().is_some_and(terminal);
                let failure = result
                    .as_ref()
                    .err()
                    .cloned()
                    .map(|cause| ConnectionFailure {
                        cause,
                        context: ConnectionContext::Receive,
                    });
                let changed = this
                    .update_in(cx, |this, window, cx| {
                        let mut r = retained.lock().unwrap();
                        if !source.matches(this) || r.epoch != epoch {
                            return false;
                        }
                        let mut changes = r.model.take_changes();
                        // An accepted resume may contain no new entities. Restore
                        // authority only from that same retained model, never a
                        // synthetic ready state while the socket is reconnecting.
                        let ready = result.is_ok() && synchronized;
                        if let Some(at) = snapshot_at {
                            this.ttfm.mark(super::ttfm::Phase::Thread, at);
                        }
                        if ready && !this.native.stream_ready && changes.actions.is_none() {
                            changes.actions = Some(r.model.action_state());
                        }
                        if let Some(state) = &mut changes.actions {
                            state.ready &= ready;
                        }
                        let controls_changed = changes.actions.is_some();
                        if let Some(state) = changes.actions.take() {
                            let timeline_runs =
                                r.model.timeline_runs().cloned().collect::<Vec<_>>();
                            if let Some(thread) =
                                this.chat.as_ref().map(|chat| chat.thread_id.clone())
                            {
                                this.feed.update(cx, |feed, cx| {
                                    feed.set_timeline_runs(
                                        &thread,
                                        &timeline_runs,
                                        state.active_run.clone(),
                                        cx,
                                    )
                                });
                            }
                            let unsettled = r
                                .model
                                .timeline_runs()
                                .filter(|run| {
                                    !matches!(
                                        run["status"].as_str(),
                                        Some(
                                            "completed"
                                                | "interrupted"
                                                | "failed"
                                                | "cancelled"
                                                | "rolled_back"
                                        )
                                    )
                                })
                                .filter_map(|run| run["id"].as_str().map(str::to_owned))
                                .collect::<Vec<_>>();
                            this.feed
                                .update(cx, |feed, cx| feed.set_copy_active_runs(unsettled, cx));
                            this.sync_native_action_surface(cx);
                            this.native
                                .actions
                                .update(cx, |a, cx| a.set_state(state, window, cx));
                            this.feed.update(cx, |feed, cx| feed.requests_changed(cx));
                        }
                        let metadata = changes.metadata.clone();
                        let changed = changes.changed() || controls_changed;
                        if changed {
                            this.feed.update(cx, |feed, cx| {
                                feed.apply_native(changes, &this.records, cx)
                            });
                        }
                        let mut notify = controls_changed
                            || this.native.failure != failure
                            || this.native.stream_ready != ready;
                        this.native.stream_ready = ready;
                        if let Some(failure) = failure {
                            this.native_failed(failure.cause, failure.context, window, cx);
                        } else if ready {
                            this.ensure_native_catalog(cx);
                            // The independent catalog stream stays subscribed and
                            // re-snapshots on its own reconnect. Thread replay
                            // must not start a second catalog refresh.
                            this.native.failure = None;
                            this.native.phase = ConnectionPhase::Connected;
                            this.native.ever_ready = true;
                            if let Some(env) = &this.environment {
                                reader_t3_settings::manager::transport(
                                    env,
                                    client.clone(),
                                    this.native.expires_at.unwrap_or(0),
                                    reader_t3_settings::manager::Stage::Connected,
                                    None,
                                    cx,
                                );
                            }
                        }
                        if let Some(metadata) = metadata {
                            notify |= this.native.metadata != metadata;
                            this.receive_live_model(&metadata);
                            this.native.retain_metadata(metadata);
                            this.sync_native_models(cx);
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
        cx.notify();
    }
    pub(super) fn prefetch_native_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A failed page remains an explicit retry; painting must not start a
        // retry chain. Existing loaded history remains usable.
        if self.native.history_error.is_none() && self.native.metadata.has_more_history {
            self.native_history(window, cx);
        }
    }
    fn native_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.active || !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
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
        self.native.history_epoch = self.native.history_epoch.wrapping_add(1);
        let history_epoch = self.native.history_epoch;
        self.native.history_error = None;
        let epoch = retained.lock().unwrap().epoch;
        let source = ConnectionSource::new(
            self,
            client.clone(),
            retained.clone(),
            thread.clone(),
            epoch,
        );
        cx.spawn_in(window, async move |this, cx| {
            let model = retained.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let page = client.history(&thread, request.cursor())?;
                    let mut retained = model.lock().unwrap();
                    if retained.epoch != epoch {
                        return Err(StreamError::Closed);
                    }
                    retained.model.apply_history(&request, page)
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if !source.same_source(this) || this.native.history_epoch != history_epoch {
                    return;
                }
                this.native.history_busy = false;
                match result {
                    Ok(()) => {
                        let mut r = retained.lock().unwrap();
                        let changes = r.model.take_changes();
                        if let Some(metadata) = &changes.metadata {
                            this.receive_live_model(metadata);
                            this.native.retain_metadata(metadata.clone());
                            this.sync_native_models(cx);
                        }
                        this.feed
                            .update(cx, |feed, cx| feed.apply_native(changes, &this.records, cx));
                    }
                    Err(error) => this.native.history_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn render_native(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let enabled = self.native.client_can_operate() && self.native.failure.is_none();
        self.native
            .actions
            .update(cx, |a, cx| a.set_enabled(enabled, cx));
        let mut view = v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(self.render_manager_onboarding(window, cx));
        if self.native.metadata.has_more_history && self.native.metadata.history_cursor.is_some() {
            view = view.child(
                Button::new("project-live-history")
                    .debug_selector(|| "project-live-history".into())
                    .ghost()
                    .label(if self.native.history_busy {
                        "Loading earlier messages…"
                    } else if self.native.history_error.is_some() {
                        "Retry earlier messages"
                    } else {
                        "Load earlier messages"
                    })
                    .disabled(self.native.history_busy)
                    .on_click(cx.listener(|this, _, window, cx| this.native_history(window, cx))),
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
        view = view.child(self.render_native_recovery(window, cx));
        if self.executors_open {
            view.child(self.native.actions.clone()).into_any_element()
        } else {
            view.into_any_element()
        }
    }
}
fn terminal(error: &StreamError) -> bool {
    !recovery_state::is_transient(error)
}

pub(super) fn delay(attempt: u32) -> Duration {
    Duration::from_secs((1u64 << attempt.saturating_sub(1).min(4)).min(15))
}
#[cfg(test)]
mod tests;
