//! The area's remote manager in the existing Reader window (#1418).
//! Network work stays off the UI thread; questionnaires remain in T3.
use super::*;
use anyhow::Context as _;
use gpui_component::input::{Textarea, TextareaState};
use okilum_t3::{
    chat::{Conversation, MessageJournal, MessageRecord},
    manifest::AreaBinding,
    Environment, GatewayStatus, ManagerGateway, McpGateway, ServerIdentity, StoredToken,
};
use std::time::Duration;

#[cfg(target_os = "linux")]
mod composer;
mod connections;
mod executors;
#[cfg(target_os = "linux")]
mod feed;
mod launch;
mod models;
#[cfg(target_os = "linux")]
mod native;

pub(super) fn save_browser_environment(
    state: &Path,
    environment: &Environment,
) -> anyhow::Result<()> {
    // This is the shared app default, not a project/environment override.
    // Named registry entries belong to explicit project connections.
    connections::save_last_origin(state, environment)
}

pub(super) fn prefill_app_browser_origin(state: Option<&Path>) -> anyhow::Result<Option<String>> {
    connections::last_origin(state)
}

pub(super) fn project_browser_origin(
    area: &Path,
    alias: &str,
    state: Option<&Path>,
) -> anyhow::Result<Option<String>> {
    connections::project_origin(area, alias, state)
}

pub(super) fn prefill_browser_origin(
    area: &Path,
    alias: &str,
    state: Option<&Path>,
) -> anyhow::Result<Option<String>> {
    connections::prefill_origin(area, alias, state)
}

pub(crate) struct ProjectView {
    binding: Option<AreaBinding>,
    area: Option<PathBuf>,
    context_path: String,
    state: Option<PathBuf>,
    environment: Option<Environment>,
    gateway: Option<Arc<dyn ManagerGateway>>,
    identity: Option<ServerIdentity>,
    chat: Option<Conversation>,
    records: Vec<MessageRecord>,
    executors_open: bool,
    executors: Option<okilum_t3::executors::Snapshot>,
    executors_error: Option<String>,
    model_target: Option<okilum_t3::models::Target>,
    models: Option<okilum_t3::models::Snapshot>,
    model_candidate: Option<okilum_t3::ModelSelection>,
    model_error: Option<String>,
    model_busy: bool,
    launch: launch::Form,
    origin: Entity<InputState>,
    compose: Entity<TextareaState>,
    reason: Option<String>,
    error: Option<String>,
    busy: bool,
    active: bool,
    polling: bool,
    generation: u64,
    _input: Subscription,
    last_poll_status: Option<GatewayStatus>,
    #[cfg(target_os = "linux")]
    feed: Entity<feed::Feed>,
    #[cfg(target_os = "linux")]
    composer: Entity<composer::Composer>,
    #[cfg(target_os = "linux")]
    native: native::Native,
}

pub(super) struct ManagerStarted;
impl EventEmitter<ManagerStarted> for ProjectView {}

impl ProjectView {
    pub(crate) fn new(
        binding: Option<AreaBinding>,
        state: Option<PathBuf>,
        area: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::empty(state.clone(), window, cx);
        view.area = Some(area);
        let state_copy = state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let env = match (&binding, &state_copy) {
                        (Some(binding), Some(state)) => connections::load(state, binding)?,
                        _ => None,
                    };
                    Ok::<_, anyhow::Error>((binding, env))
                })
                .await;
            let env = this
                .update_in(cx, |this, window, cx| match result {
                    Ok((binding, env)) => {
                        this.reason = match &binding {
                            None => Some("This area is not connected to T3.".into()),
                            Some(binding) if binding.manager.thread_id.is_none() => {
                                Some("This area has no linked manager yet.".into())
                            }
                            _ => None,
                        };
                        this.binding = binding;
                        if let Some(env) = &env {
                            this.origin
                                .update(cx, |input, cx| input.set_value(env.origin(), window, cx));
                        }
                        cx.notify();
                        env
                    }
                    Err(error) => {
                        this.reason = None;
                        this.error = Some(format!("{error:#}"));
                        cx.notify();
                        None
                    }
                })
                .ok()
                .flatten();
            let Some(env) = env else {
                return;
            };
            let url = env.credential_url();
            let task = match this.update_in(cx, |_, _, cx| cx.read_credentials(&url)) {
                Ok(task) => task,
                Err(_) => return,
            };
            let token = match task.await {
                Ok(Some((_, bytes))) => StoredToken::from_secret(&bytes).map(Some),
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
            let result = match token {
                Ok(token) => {
                    cx.background_executor()
                        .spawn(async move {
                            McpGateway::new(env.clone(), token)
                                .map(|gateway| (env, Arc::new(gateway) as Arc<dyn ManagerGateway>))
                        })
                        .await
                }
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_connection(result, window, cx)
            });
        })
        .detach();
        view
    }

    fn empty(state: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let origin =
            cx.new(|cx| InputState::new(window, cx).placeholder("T3 server address (https://…)"));
        let compose = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder("Message your manager…")
        });
        #[cfg(not(target_os = "linux"))]
        let input = cx.subscribe(&compose, |_, _, _: &InputEvent, cx| cx.notify());
        #[cfg(target_os = "linux")]
        let parent = cx.entity().downgrade();
        #[cfg(target_os = "linux")]
        let feed = cx.new(|cx| feed::Feed::new(parent, cx));
        #[cfg(target_os = "linux")]
        let composer = cx.new(|cx| composer::Composer::new(compose.clone(), cx));
        #[cfg(target_os = "linux")]
        let input = cx.subscribe_in(
            &composer,
            window,
            |this, _, _: &composer::SendRequested, window, cx| {
                this.send(None, window, cx);
            },
        );
        Self {
            binding: None,
            area: None,
            context_path: String::new(),
            state,
            environment: None,
            gateway: None,
            identity: None,
            chat: None,
            records: Vec::new(),
            executors_open: false,
            executors: None,
            executors_error: None,
            model_target: None,
            models: None,
            model_candidate: None,
            model_error: None,
            model_busy: false,
            launch: launch::Form::new(window, cx),
            origin,
            compose,
            reason: Some("Loading project…".into()),
            error: None,
            busy: false,
            active: true,
            polling: false,
            generation: 0,
            _input: input,
            last_poll_status: None,
            #[cfg(target_os = "linux")]
            feed,
            #[cfg(target_os = "linux")]
            composer,
            #[cfg(target_os = "linux")]
            native: native::Native::new(window, cx),
        }
    }

    pub(crate) fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        #[cfg(target_os = "linux")]
        if active {
            self.native_start(window, cx);
        } else {
            self.native.pause();
        }
        if active {
            self.start_polling(window, cx);
        }
    }

    fn finish_connection(
        &mut self,
        result: anyhow::Result<(Environment, Arc<dyn ManagerGateway>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        match result {
            Ok((env, gateway)) => {
                #[cfg(target_os = "linux")]
                self.native.clear();
                self.environment = Some(env);
                self.gateway = Some(gateway);
                self.chat = self
                    .binding
                    .as_ref()
                    .and_then(|b| b.manager.thread_id.as_ref())
                    .map(Conversation::new);
                #[cfg(target_os = "linux")]
                self.feed.update(cx, |feed, cx| feed.reset(cx));
                self.last_poll_status = None;
                self.identity = None;
                self.model_target = None;
                self.models = None;
                self.model_candidate = None;
                self.model_error = None;
                self.error = None;
                self.generation += 1;
                self.start_polling(window, cx);
                #[cfg(target_os = "linux")]
                self.native_restore(false, window, cx);
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    fn connection_environment(&self, cx: &App) -> anyhow::Result<Environment> {
        let binding = self
            .binding
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("This area is not connected to T3."))?;
        Environment::from_input(&binding.environment, &self.origin.read(cx).value())
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.connect_mode(false, window, cx);
    }

    fn connect_mode(&mut self, sign_in_again: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.model_busy {
            return;
        }
        if self.binding.is_none() {
            return;
        }
        let Some(state) = self.state.clone() else {
            self.error = Some("Application state is unavailable.".into());
            cx.notify();
            return;
        };
        let env = match self.connection_environment(cx) {
            Ok(env) => env,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.error = None;
        let (send_url, receive_url) = async_channel::bounded::<String>(1);
        let worker_env = env.clone();
        cx.spawn_in(window, async move |this, cx| {
            let key = env.credential_url();
            let saved = if sign_in_again {
                Ok(None)
            } else {
                let task = match this.update_in(cx, |_, _, cx| cx.read_credentials(&key)) {
                    Ok(task) => task,
                    Err(_) => return,
                };
                match task.await {
                    Ok(Some((_, bytes))) => {
                        StoredToken::from_secret(&bytes)
                            .context("Saved T3 credentials cannot be read. Choose Sign in again to replace them after authorization.")
                            .map(|token| Some((key, token)))
                    }
                    Ok(None) => Ok(None),
                    Err(error) => Err(error),
                }
            };
            let saved = match saved {
                Ok(saved) => saved,
                Err(error) => {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.finish_connection(Err(error), window, cx)
                    });
                    return;
                }
            };
            let job = cx.background_executor().spawn(async move {
                okilum_t3::reconnect::connect(&worker_env, saved, sign_in_again, || {
                    okilum_t3::oauth::connect(
                        &worker_env,
                        "Okilum",
                        move |url| {
                            send_url
                                .send_blocking(url.to_string())
                                .map_err(|_| anyhow::anyhow!("The connection window closed"))
                        },
                        Duration::from_secs(180),
                    )
                })
            });
            // A reused token never invokes authorization: its unused URL sender
            // closes, so this does not wait for a browser or ask for pairing.
            if let Ok(url) = receive_url.recv().await {
                let _ = this.update_in(cx, |_, _, cx| cx.open_url(&url));
            }
            let connected = match job.await {
                Ok(connected) => connected,
                Err(error) => {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.finish_connection(Err(error), window, cx)
                    });
                    return;
                }
            };
            if let Some(token) = &connected.issued_token {
                let task = match this.update_in(cx, |_, _, cx| {
                    cx.write_credentials(&env.credential_url(), &env.alias, &token.to_secret())
                }) {
                    Ok(task) => task,
                    Err(_) => return,
                };
                if let Err(error) = task.await {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.finish_connection(Err(error), window, cx)
                    });
                    return;
                }
            }
            let result = cx
                .background_executor()
                .spawn(async move {
                    connections::save(&state, &env)?;
                    Ok((env, Arc::new(connected.gateway) as Arc<dyn ManagerGateway>))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_connection(result, window, cx)
            });
        })
        .detach();
        cx.notify();
    }

    fn start_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let (Some(area), Some(state), Some(gateway), Some(environment), Some(project_id)) = (
            self.area.clone(),
            self.state.clone(),
            self.gateway.clone(),
            self.environment.clone(),
            self.binding
                .as_ref()
                .and_then(|b| b.manager.project_id.clone()),
        ) else {
            self.error =
                Some("Connect to the configured T3 project before starting a manager.".into());
            cx.notify();
            return;
        };
        let context = self.context_path.clone();
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::reader_manager_start::start(
                        &area,
                        &context,
                        &state,
                        gateway.as_ref(),
                        &environment,
                        &project_id,
                    )
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(thread) => {
                        if let Some(binding) = &mut this.binding {
                            binding.manager.thread_id = Some(thread.clone());
                        }
                        this.chat = Some(Conversation::new(thread));
                        #[cfg(target_os = "linux")]
                        this.feed.update(cx, |feed, cx| feed.reset(cx));
                        this.reason = None;
                        this.start_polling(window, cx);
                        cx.emit(ManagerStarted);
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn can_send(&self) -> bool {
        #[cfg(target_os = "linux")]
        if self.native.active() {
            return !self.native.deleted() && self.native.client_can_operate();
        }

        self.identity.is_some()
            && self.gateway.as_ref().is_some_and(|gateway| {
                matches!(
                    gateway.status(),
                    GatewayStatus::Connected { .. } | GatewayStatus::ExpiresSoon { .. }
                )
            })
    }

    fn journal(&self) -> Option<MessageJournal> {
        Some(MessageJournal::new(
            self.state.as_ref()?,
            self.environment.as_ref()?,
            &self.chat.as_ref()?.thread_id,
        ))
    }

    fn start_polling(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.polling || !self.active || self.gateway.is_none() || self.chat.is_none() {
            return;
        }
        self.polling = true;
        cx.spawn_in(window, async move |this, cx| loop {
            let job = this
                .update_in(cx, |this, window, cx| {
                    if !this.active {
                        this.polling = false;
                        return None;
                    }
                    #[cfg(target_os = "linux")]
                    if this.native.active() && this.identity.is_some() && !this.executors_open {
                        this.polling = false;
                        return None;
                    }
                    let gateway = this.gateway.clone()?;
                    if matches!(
                        gateway.status(),
                        GatewayStatus::NotConnected
                            | GatewayStatus::Expired
                            | GatewayStatus::Revoked
                    ) {
                        this.polling = false;
                        cx.notify();
                        return None;
                    }
                    Some((
                        gateway,
                        this.chat.as_ref()?.poll_request(),
                        this.journal(),
                        this.generation,
                        window.is_window_active(),
                        this.binding.as_ref()?.manager.project_id.clone()?,
                        this.executors_open
                            .then(|| this.area.clone().zip(this.binding.clone()))
                            .flatten(),
                    ))
                })
                .ok()
                .flatten();
            let Some((
                gateway,
                chat,
                journal,
                generation,
                focused,
                expected_project,
                executor_binding,
            )) = job
            else {
                break;
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    let identity = gateway.server_identity()?;
                    let chat = chat.fetch(gateway.as_ref())?;
                    anyhow::ensure!(
                        chat.project_id == expected_project,
                        "The linked manager belongs to another T3 project. Check the area binding."
                    );
                    let records = journal
                        .map(|journal| journal.records())
                        .transpose()?
                        .unwrap_or_default();
                    let executors = executor_binding.map(|(area, binding)| {
                        okilum_t3::executors::Snapshot::load(gateway.as_ref(), &area, &binding)
                    });
                    Ok::<_, anyhow::Error>((chat, identity, records, executors))
                })
                .await;
            let next = this
                .update_in(cx, |this, _, cx| {
                    if generation != this.generation {
                        return 0;
                    }
                    let mut delay = if focused { 2_000 } else { 15_000 };
                    let status = this.gateway.as_ref().map(|gateway| gateway.status());
                    let mut changed = this.last_poll_status != status;
                    this.last_poll_status = status;
                    match result {
                        Ok((chat, identity, records, executors)) => {
                            if chat.has_more {
                                delay = 100;
                            }
                            let Some(current) = this.chat.as_mut() else {
                                return 0;
                            };
                            let changes = match current.apply_poll(chat) {
                                Ok(changes) => changes,
                                Err(error) => {
                                    let error = Some(error.to_string());
                                    if this.error != error {
                                        this.error = error;
                                        cx.notify();
                                    }
                                    return 15_000;
                                }
                            };
                            let records_changed = this.records != records;
                            changed |= changes.changed()
                                || this.identity.as_ref() != Some(&identity)
                                || records_changed
                                || this.error.is_some();
                            this.identity = Some(identity);
                            if records_changed {
                                this.records = records;
                            }
                            this.error = None;
                            #[cfg(target_os = "linux")]
                            this.feed.update(cx, |feed, cx| {
                                feed.apply(
                                    changes
                                        .positions
                                        .iter()
                                        .filter_map(|position| current.items.get(position)),
                                    records_changed.then_some(this.records.as_slice()),
                                    cx,
                                )
                            });
                            if let Some(result) = executors {
                                match result {
                                    Ok(snapshot) => {
                                        changed |= this.executors.as_ref() != Some(&snapshot)
                                            || this.executors_error.is_some();
                                        this.executors = Some(snapshot);
                                        this.executors_error = None;
                                    }
                                    Err(error) => {
                                        let error = Some(format!("{error:#}"));
                                        changed |= this.executors_error != error;
                                        this.executors_error = error;
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            let error = Some(format!("{error:#}"));
                            changed |= this.identity.is_some() || this.error != error;
                            this.identity = None;
                            this.error = error;
                            delay = 15_000;
                        }
                    }
                    if changed {
                        cx.notify();
                    }
                    delay
                })
                .unwrap_or(0);
            if next == 0 {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.polling = false;
                    this.start_polling(window, cx);
                });
                break;
            }
            cx.background_executor()
                .timer(Duration::from_millis(next))
                .await;
        })
        .detach();
    }

    fn send(&mut self, retry: Option<MessageRecord>, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(target_os = "linux")]
        if retry.is_none() && self.native.active() {
            self.native_send(window, cx);
            return;
        }
        if self.busy || !self.can_send() {
            return;
        }
        let Some(gateway) = self.gateway.clone() else {
            return;
        };
        let Some(journal) = self.journal() else {
            return;
        };
        let draft = self.compose.read(cx).value().to_string();
        if retry.is_none() && draft.trim().is_empty() {
            return;
        }
        let is_new = retry.is_none();
        self.busy = true;
        self.error = None;
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let sent_text = draft.clone();
            let (result, records, retained_id) = cx
                .background_executor()
                .spawn(async move {
                    let mut retained_id = None;
                    let result = (|| {
                        let record = match retry {
                            Some(record) => record,
                            None => journal.prepare(&draft)?,
                        };
                        retained_id = Some(record.message.client_request_id.clone());
                        journal.send(gateway.as_ref(), &record)
                    })();
                    (result, journal.records(), retained_id)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if generation != this.generation {
                    cx.notify();
                    return;
                }
                let retained = records.as_ref().is_ok_and(|records| {
                    records.iter().any(|record| {
                        Some(&record.message.client_request_id) == retained_id.as_ref()
                    })
                });
                if is_new && retained && this.compose.read(cx).value() == sent_text {
                    this.compose
                        .update(cx, |input, cx| input.set_value("", window, cx));
                }
                match records {
                    Ok(records) => {
                        if this.records != records {
                            this.records = records;
                            #[cfg(target_os = "linux")]
                            this.feed.update(cx, |feed, cx| {
                                feed.apply(std::iter::empty(), Some(&this.records), cx)
                            });
                        }
                    }
                    Err(error) => this.error = Some(format!("{error:#}")),
                }
                if let Err(error) = result {
                    this.error = Some(format!("{error:#}"));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn expiry_note(expires_at: u64) -> String {
    i64::try_from(expires_at)
        .ok()
        .and_then(|seconds| chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0))
        .map(|date| {
            format!(
                "Connection token expires {} UTC",
                date.format("%Y-%m-%d %H:%M")
            )
        })
        .unwrap_or_else(|| "Connection expiry unavailable".into())
}

fn status_note(status: &GatewayStatus) -> Option<&'static str> {
    match status {
        GatewayStatus::Connected { .. } => None,
        GatewayStatus::NotConnected => Some("Connect to T3 to talk to this manager."),
        GatewayStatus::ExpiresSoon { .. } => {
            Some("Your T3 connection expires within three days. Choose Renew to sign in again.")
        }
        GatewayStatus::Expired => Some("Your T3 connection has expired. Reconnect to continue."),
        GatewayStatus::Revoked => Some("Your T3 connection was revoked. Reconnect to continue."),
        GatewayStatus::Unreachable { .. } => {
            Some("T3 is unreachable. Your history and retained messages are kept.")
        }
    }
}

impl Render for ProjectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self
            .gateway
            .as_ref()
            .map(|g| g.status())
            .unwrap_or(GatewayStatus::NotConnected);
        let enabled = self.can_send();
        let note = status_note(&status);
        let expiry = self
            .gateway
            .as_ref()
            .and_then(|gateway| gateway.token_expires_at())
            .map(expiry_note);
        let renew = matches!(
            status,
            GatewayStatus::ExpiresSoon { .. } | GatewayStatus::Expired
        );
        let browser_help = self.busy
            || matches!(
                status,
                GatewayStatus::NotConnected | GatewayStatus::Expired | GatewayStatus::Revoked
            );
        let browser_origin = self
            .connection_environment(cx)
            .ok()
            .map(|environment| environment.origin());

        let pending = self
            .chat
            .as_ref()
            .filter(|chat| chat.pending_question_error.is_none())
            .map(|chat| chat.pending_questions)
            .unwrap_or(0);
        let open = self
            .identity
            .as_ref()
            .zip(self.chat.as_ref())
            .map(|(identity, chat)| identity.thread_url(&chat.thread_id));
        #[cfg(target_os = "linux")]
        let open = open.or_else(|| {
            self.native
                .active()
                .then(|| self.environment.as_ref().map(Environment::origin))
                .flatten()
        });
        #[cfg(target_os = "linux")]
        let history = {
            self.feed
                .update(cx, |feed, cx| feed.set_controls(enabled, self.busy, cx));
            let native_ready = !self.native.active()
                || (self.native.actions.read(cx).state.ready
                    && self.native.stream_error_is_clear());
            let can_steer = self.native.actions.read(cx).state.can_steer;
            self.composer.update(cx, |composer, cx| {
                composer.set_native(self.native.active(), can_steer, cx);
                composer.set_controls(enabled && native_ready, self.busy, cx)
            });
            div().flex_1().min_h_0().child(
                self.feed
                    .clone()
                    .cached(StyleRefinement::default().size_full()),
            )
        };
        #[cfg(not(target_os = "linux"))]
        let history = {
            let mut history = v_flex()
                .id("project-messages")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .gap_4();
            if let Some(chat) = &self.chat {
                for item in chat.items.values() {
                    if !matches!(item.kind.as_str(), "user_message" | "assistant_message") {
                        continue;
                    }
                    let Some(text) = &item.text else {
                        continue;
                    };
                    history = history.child(
                        v_flex()
                            .gap_1()
                            .child(div().text_color(brand::palette(cx).text_muted).child(
                                if item.kind == "user_message" {
                                    "You"
                                } else {
                                    "Manager"
                                },
                            ))
                            .child(
                                TextView::markdown(
                                    ("project-message", item.position as usize),
                                    text.clone(),
                                )
                                .selectable(true)
                                .style(reader_text_style(cx.theme())),
                            ),
                    );
                }
            }
            for (index, record) in self.records.iter().enumerate() {
                let projected = record.receipt.as_ref().is_some_and(|receipt| {
                    self.chat.as_ref().is_some_and(|chat| {
                        chat.items
                            .values()
                            .any(|item| item.message_id.as_ref() == Some(&receipt.message_id))
                    })
                });
                if projected {
                    continue;
                }
                let retry = record.clone();
                history = history.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_color(brand::palette(cx).text_muted).child(
                            if record.receipt.is_some() {
                                "You · queued"
                            } else {
                                "Message delivery unconfirmed"
                            },
                        ))
                        .when(record.receipt.is_some(), |row| {
                            row.child(
                                TextView::markdown(
                                    ("project-retained", index),
                                    record.message.text.clone(),
                                )
                                .selectable(true)
                                .style(reader_text_style(cx.theme())),
                            )
                        })
                        .when(record.receipt.is_none(), |row| {
                            row.child(
                                Button::new(("project-retry", index))
                                    .debug_selector(move || format!("project-retry-{index}"))
                                    .ghost()
                                    .icon(IconName::RotateCw)
                                    .tooltip(format!(
                                        "Retry retained message: {}",
                                        record.message.text
                                    ))
                                    .disabled(self.busy || !enabled)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.send(Some(retry.clone()), window, cx)
                                    })),
                            )
                        }),
                );
            }
            history
        };
        v_flex()
            .id("project-tab")
            .debug_selector(|| "project-tab".into())
            .size_full()
            .min_h_0()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        gpui_component::button::ButtonGroup::new("project-segments")
                            .child(
                                Button::new("project-manager")
                                    .ghost()
                                    .label("Manager")
                                    .selected(!self.executors_open)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.executors_open = false;
                                        cx.notify();
                                    })),
                            )
                            .when(
                                self.binding
                                    .as_ref()
                                    .is_some_and(|b| !b.executor.project_id.is_empty()),
                                |group| {
                                    group.child(
                                        Button::new("project-executors")
                                            .ghost()
                                            .label("Executors")
                                            .debug_selector(|| "project-executors".into())
                                            .selected(self.executors_open)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.executors_open = true;
                                                this.start_polling(window, cx);
                                                cx.notify();
                                            })),
                                    )
                                },
                            ),
                    )
                    .child(div().flex_1())
                    .when(
                        self.binding
                            .as_ref()
                            .is_some_and(|b| !b.executor.project_id.is_empty()),
                        |row| {
                            row.child(
                                Button::new("project-launch")
                                    .debug_selector(|| "project-launch".into())
                                    .ghost()
                                    .label("Request executor")
                                    .disabled(self.busy || self.model_busy || !enabled)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_launch(window, cx)
                                    })),
                            )
                        },
                    )
                    .when_some(self.manager_model_target(), |row, target| {
                        row.child(
                            Button::new("project-manager-model")
                                .debug_selector(|| "project-manager-model".into())
                                .ghost()
                                .label("Model")
                                .disabled(self.busy || self.model_busy || !enabled)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_models(target.clone(), window, cx);
                                })),
                        )
                    })
                    .when(pending > 0, |row| {
                        row.child(
                            div()
                                .text_color(brand::palette(cx).text_muted)
                                .child("Question in T3"),
                        )
                    })
                    .when_some(open, |row, url| {
                        row.child(
                            Button::new("project-open-t3")
                                .ghost()
                                .icon(IconName::ExternalLink)
                                .tooltip("Open in T3")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    }),
            )
            .when(
                self.binding
                    .as_ref()
                    .is_some_and(|b| b.manager.thread_id.is_none()),
                |view| {
                    view.child(div().child("Manager не запущен")).child(
                        Button::new("start-manager")
                            .label("Start manager")
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.start_manager(window, cx)),
                            ),
                    )
                },
            )
            .when(self.binding.is_some(),|view| {
                #[cfg(target_os="linux")] let view=view.child(self.render_native(cx));
                view
            })
            .when_some(self.reason.clone(), |view, reason| {
                view.child(
                    div()
                        .text_color(brand::palette(cx).text_muted)
                        .child(reason),
                )
            })
            .when_some(note, |view, note| {
                view.child(
                    div()
                        .debug_selector(|| "project-connection-status".into())
                        .text_color(brand::palette(cx).text_muted)
                        .child(note),
                )
            })
            .when(self.binding.as_ref().is_some(), |view| {
                view.child(
                    h_flex()
                        .gap_2()
                        .child(Input::new(&self.origin).flex_1())
                        .child(
                            Button::new("project-connect")
                                .ghost()
                                .icon(IconName::RotateCw)
                                .tooltip("Reconnect using the saved T3 token")
                                .disabled(self.busy || self.model_busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.connect(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("project-sign-in-again")
                                .debug_selector(move || if renew { "project-renew".into() } else { "project-sign-in-again".into() })
                                .ghost()
                                .label(if renew { "Renew" } else { "Sign in again" })
                                .tooltip("Authorize a new 30-day connection in your browser")
                                .disabled(self.busy || self.model_busy)
                                .on_click(cx.listener(|this, _, window, cx| this.connect_mode(true, window, cx))),
                        ),
                )
            })
            .when_some(expiry, |view, expiry| view.child(div().w_full().min_w_0().whitespace_normal().debug_selector(|| "project-token-expiry".into()).text_color(brand::palette(cx).text_muted).child(expiry)))
            .when(self.binding.is_some() && browser_help, |view| {
                view.child(v_flex().gap_1()
                    .child(div().w_full().min_w_0().whitespace_normal().child("If your browser is signed out, sign in to T3 once (30 days), then approve this connection."))
                    .child(Button::new("project-browser-sign-in").debug_selector(|| "project-browser-sign-in".into()).ghost().label("Open T3 in browser")
                        .disabled(browser_origin.is_none())
                        .on_click(move |_, _, cx| { if let Some(origin) = &browser_origin { cx.open_url(origin); } })))
            })
            .when_some(
                self.chat
                    .as_ref()
                    .and_then(|chat| chat.pending_question_error.clone()),
                |view, error| {
                    view.child(
                        div()
                            .debug_selector(|| "project-questions-unavailable".into())
                            .text_color(cx.theme().danger)
                            .child(format!(
                                "Questions unavailable: {error}. Open T3 to view questions."
                            )),
                    )
                },
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .debug_selector(|| "project-error".into())
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .when(self.model_target.is_some(), |view| {
                view.child(self.render_models(cx))
            })
            .when(self.launch.open, |view| view.child(self.render_launch(cx)))
            .child(if self.executors_open {
                self.render_executors(cx)
            } else {
                history.into_any_element()
            })
            .when(!self.executors_open, |view| {
                #[cfg(target_os = "linux")]
                let composer = self.composer.clone().into_any_element();
                #[cfg(not(target_os = "linux"))]
                let composer = h_flex()
                        .items_end()
                        .gap_2()
                        .child(Textarea::new(&self.compose).flex_1())
                        .child(
                            Button::new("project-send")
                                .debug_selector(|| "project-send".into())
                                .ghost()
                                .icon(IconName::ArrowUp)
                                .tooltip("Send to manager")
                                .disabled(
                                    self.busy
                                        || !enabled
                                        || self.compose.read(cx).value().trim().is_empty(),
                                )
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.send(None, window, cx)),
                                ),
                        ).into_any_element();
                view.child(composer)
            })
    }
}

impl Reader {
    pub(super) fn close_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.project_request = self.project_request.wrapping_add(1);
        if let Some(project) = self.project.as_ref().filter(|_| self.project_open) {
            project.update(cx, |project, cx| project.set_active(false, window, cx));
        }
        self.project_open = false;
    }

    #[cfg(test)]
    pub(super) fn toggle_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_open {
            self.close_project(window, cx);
            cx.notify();
            return;
        }
        let note = self.vault_root.join(&self.current_rel);
        let area = connections::area_for(&self.vault_root, &note)
            .unwrap_or_else(|_| self.vault_root.clone());
        self.open_project_area(area, window, cx);
    }

    pub(super) fn open_project_area(
        &mut self,
        area: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(previous) = &self.project {
            previous.update(cx, |view, cx| view.set_active(false, window, cx));
        }
        self.project_open = true;
        self.project = None;
        self.project_request = self.project_request.wrapping_add(1);
        let request = self.project_request;
        let unavailable_manager = self
            .project_navigation
            .statuses
            .get(&area)
            .filter(|status| status.restartable())
            .and_then(|_| self.project_navigation.status_managers.get(&area))
            .cloned();
        cx.spawn_in(window, async move |this, cx| {
            // Re-read the explicit pointer on every activation. A rotation gets
            // its own view; returning to an existing binding retains its draft.
            let binding = cx
                .background_executor()
                .spawn(async move {
                    let project = okilum_t3::projects::Project::read(&area)?;
                    let mut binding = project.native_binding()?;
                    if unavailable_manager.as_ref() == Some(&project.manager) {
                        if let Some(binding) = &mut binding {
                            binding.manager.thread_id = None;
                        }
                    }
                    let key = (area, serde_json::to_string(&binding)?);
                    Ok::<_, anyhow::Error>((key, binding))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.project_request != request || !this.project_open {
                    return;
                }
                let project = match binding {
                    Ok((key, binding)) => match this.project_views.get(&key).cloned() {
                        Some(project) => {
                            project.update(cx, |project, cx| project.set_active(true, window, cx));
                            project
                        }
                        None => {
                            let state = this.session_directory.clone();
                            let project = cx.new(|cx| {
                                let mut view =
                                    ProjectView::new(binding, state, key.0.clone(), window, cx);
                                view.context_path = key
                                    .0
                                    .strip_prefix(&this.vault_root)
                                    .unwrap_or(&key.0)
                                    .to_string_lossy()
                                    .replace('\\', "/");
                                view
                            });
                            cx.subscribe_in(
                                &project,
                                window,
                                |this, _, _: &ManagerStarted, window, cx| {
                                    if let Some(active) = this.project_navigation.active.clone() {
                                        this.enter_vault_project(active, window, cx);
                                    }
                                },
                            )
                            .detach();
                            this.project_views.insert(key, project.clone());
                            project
                        }
                    },
                    Err(error) => cx.new(|cx| {
                        let mut view =
                            ProjectView::empty(this.session_directory.clone(), window, cx);
                        view.reason = None;
                        view.error = Some(format!("{error:#}"));
                        view
                    }),
                };
                this.project = Some(project);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(any(test, feature = "project-ui-harness"))]
mod fixture;
#[cfg(test)]
mod tests;

#[cfg(feature = "project-ui-harness")]
pub(crate) fn open_harness(cx: &mut App) -> bool {
    fixture::open(cx)
}
