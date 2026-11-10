//! Kibana controls around the existing LogView, not a second event renderer.
use crate::*;
#[path = "reader_log_remote/investigation.rs"]
mod investigation;
use anyhow::Context as _;
use okilum_core::remote_log::live::{Mode, Status, Update, Worker};
use okilum_core::remote_log::{self, discover::TimeRange, transport::ApiKey, EventWindow, Source};
#[path = "reader_log_remote/guided.rs"]
mod guided;
#[path = "reader_log_remote/server_filters.rs"]
mod server_filters;
use guided::Guided;
#[path = "reader_log_remote/credentials.rs"]
mod credentials;
#[derive(Default)]
struct Windows(std::collections::BTreeMap<String, WindowHandle<Root>>);
impl Global for Windows {}

pub(crate) fn show(vault: PathBuf, cx: &mut App) {
    show_inner(vault, None, cx);
}

/// The existing router must first resolve `link.vault_id` to this exact root,
/// without guessing a name or writing/enrolling an identity (#1414 owner).
#[allow(dead_code)]
pub(crate) fn show_link(vault: PathBuf, link: remote_log::link::RemoteLink, cx: &mut App) {
    show_inner(vault, Some(link), cx);
}

fn show_inner(vault: PathBuf, link: Option<remote_log::link::RemoteLink>, cx: &mut App) {
    let scope = remote_log::vault_scope(&vault);
    if let Some(handle) = cx
        .try_global::<Windows>()
        .and_then(|w| w.0.get(&scope))
        .copied()
    {
        let pending = link.clone();
        if handle
            .update(cx, move |root, window, cx| {
                if let Some(link) = pending {
                    if let Ok(panel) = root.view().clone().downcast::<Panel>() {
                        panel.update(cx, |panel, cx| {
                            panel.pending_link = Some(link);
                            cx.notify();
                        });
                    }
                }
                window.activate_window();
            })
            .is_ok()
        {
            return;
        }
    }
    let state = match reader_ui_state::state_directory(cx) {
        Ok(state) => state,
        Err(error) => {
            diagnostic_error!("Cannot open remote logs: {error}");
            return;
        }
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(window_state::default_bounds(
            size(px(1200.), px(800.)),
            cx,
        ))),
        window_min_size: Some(size(px(900.), px(600.))),
        ..TitleBar::window_options()
    };
    if let Ok(handle) = cx.open_window(options, move |window, cx| {
        window.set_window_title(&format!(
            "Kibana logs — {}",
            vault.file_name().unwrap_or_default().to_string_lossy()
        ));
        let panel = cx.new(|cx| {
            let mut panel = Panel::new(vault, state, window, cx);
            panel.pending_link = link;
            panel
        });
        cx.new(|cx| Root::new(panel, window, cx))
    }) {
        if cx.try_global::<Windows>().is_none() {
            cx.set_global(Windows::default());
        }
        cx.global_mut::<Windows>().0.insert(scope, handle);
    }
}

struct Panel {
    credentials: credentials::Credentials,
    guided: Guided,
    _chips: Option<Subscription>,
    pending_link: Option<remote_log::link::RemoteLink>,
    generation: u64,
    following: bool,
    credential_pending: bool,
    proposal: Option<Mode>,
    sources_loaded: bool,
    vault: PathBuf,
    state: PathBuf,
    name: Entity<InputState>,
    url: Entity<InputState>,
    pattern: Entity<InputState>,
    time_field: Entity<InputState>,
    requested: Option<Source>,
    requested_definition: Option<Source>,
    applied_definition: Option<Source>,
    auth_error: bool,
    quality: Option<remote_log::result::ResultStatus>,
    key: Entity<InputState>,
    kql: Entity<InputState>,
    filters: Entity<InputState>,
    from: Entity<InputState>,
    to: Entity<InputState>,
    sources: Vec<Source>,
    draft: Option<Source>,
    applied: Option<Source>,
    range: Option<TimeRange>,
    view: Option<Entity<reader_log::LogView>>,
    data: Option<Arc<EventWindow>>,
    busy: bool,
    more: bool,
    exporting: bool,
    message: String,
    error: Option<String>,
    worker: Worker,
    _events: Task<()>,
    _load: Task<()>,
    _credential: Option<Task<()>>,
    _navigation: Option<Subscription>,
}
impl Panel {
    fn new(vault: PathBuf, state: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &'static str, cx: &mut Context<Self>, window: &mut Window| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let name = input("Source name", cx, window);
        let url = input("HTTPS endpoint or complete Discover URL", cx, window);
        let pattern = input("Index pattern (required for a direct endpoint)", cx, window);
        let time_field = input("Time field (default @timestamp)", cx, window);
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Encoded API key")
                .masked(true)
        });
        let kql = input("Server KQL", cx, window);
        let filters = input("Discover filter objects as JSON", cx, window);
        let from = input("From: now-15m or RFC3339", cx, window);
        let to = input("To: now or RFC3339", cx, window);
        let (worker, events) = Worker::start();
        let _events = cx.spawn(async move |this, cx| {
            while let Ok(reply) = events.recv().await {
                if this.update(cx, |this, cx| this.receive(reply, cx)).is_err() {
                    break;
                }
            }
        });
        let directory = state.clone();
        let root = vault.clone();
        let _load = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { remote_log::load_sources(&directory, &root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(sources) => {
                        this.sources = sources;
                        this.sources_loaded = true;
                    }
                    Err(error) => {
                        this.error = Some(format!("Cannot load saved sources: {error:#}"))
                    }
                }
                cx.notify();
            });
        });
        let guided = Guided::new(window, cx);
        Self{credentials:Default::default(),_navigation:None,guided,_chips:None,generation:0,following:false,credential_pending:false,proposal:None,pending_link:None,sources_loaded:false,vault,state,name,url,pattern,time_field,requested:None,requested_definition:None,applied_definition:None,auth_error:false,quality:None,key,kql,filters,from,to,sources:Vec::new(),draft:None,applied:None,range:None,view:None,data:None,busy:false,more:false,exporting:false,message:"Add a source or import Discover, review the query and filters, then Apply to load history. Start never runs automatically.".into(),error:None,worker,_events,_load,_credential:None}
    }
    fn choose(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        self.stop();
        self.credentials.select(source.credential_url(&self.vault));
        if self
            .draft
            .as_ref()
            .is_none_or(|old| old.id != source.id || old.endpoint != source.endpoint)
        {
            self.guided.scope.clear();
            self.guided.chips.clear();
            if let Some(view) = &self.view {
                view.update(cx, |view, cx| view.reset_remote_filters(window, cx));
            }
        }
        self.guided.open_scope = None;
        self.guided.custom = false;
        self.guided.key_needed = false;
        self.guided.invalidate_counts();
        self.configure_server_counts(cx);
        self.key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Encoded API key")
                .masked(true)
        });
        self.name
            .update(cx, |i, cx| i.set_value(&source.name, window, cx));
        self.pattern.update(cx, |i, cx| {
            i.set_value(source.index_pattern.as_deref().unwrap_or(""), window, cx)
        });
        self.time_field
            .update(cx, |i, cx| i.set_value(&source.time_field, window, cx));
        self.auth_error = false;
        self.requested = None;
        self.kql.update(cx, |i, cx| {
            i.set_value(&source.discover.query.query, window, cx)
        });
        self.filters.update(cx, |i, cx| {
            i.set_value(
                serde_json::to_string(&source.discover.filters).unwrap_or_default(),
                window,
                cx,
            )
        });
        self.from.update(cx, |i, cx| {
            i.set_value(&source.discover.time.from, window, cx)
        });
        self.to.update(cx, |i, cx| {
            i.set_value(&source.discover.time.to, window, cx)
        });
        self.message = format!(
            "{} · Data view {} · Imported refresh {} · Review, then Apply",
            source.endpoint,
            source.discover.data_view,
            if source.discover.refresh_paused {
                "paused"
            } else {
                "requested; not started"
            }
        );
        self.draft = Some(source);
        self.guided.adding = false;
        self.error = None;
        cx.notify();
    }
    /// Recover the last successful inputs without changing the retained rows or
    /// issuing Apply. Saved definitions may already contain a failed candidate.
    fn restore_applied(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.credential_pending {
            return;
        }
        let Some(source) = self
            .applied_definition
            .clone()
            .or_else(|| self.applied.clone())
        else {
            return;
        };
        self.choose(source, window, cx);
        self.url
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.message = "Last applied advanced inputs restored; scope and chips remain selected. Loaded snapshot and its absolute range are unchanged; Apply explicitly to re-query, or Start the applied query.".into();
        cx.notify();
    }
    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.url.read(cx).value();
        let result = if input.contains("/app/discover") {
            Source::import(&self.name.read(cx).value(), &input)
        } else {
            Source::direct(
                &self.name.read(cx).value(),
                &input,
                &self.pattern.read(cx).value(),
                &self.time_field.read(cx).value(),
            )
        };
        match result {
            Ok(source) => {
                self.url.update(cx, |i, cx| i.set_value("", window, cx));
                self.choose(source, window, cx);
            }
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
            }
        }
    }
    fn candidate(&self, cx: &App) -> Result<Source> {
        let mut source = self
            .draft
            .clone()
            .context("Import or select a saved source first")?;
        let pattern = self.pattern.read(cx).value();
        if !pattern.trim().is_empty() {
            source.index_pattern = Some(pattern.trim().into());
        }
        let field = self.time_field.read(cx).value();
        if !field.trim().is_empty() && source.time_field != field.trim() {
            if let Some(sort) = source.discover.sort.first_mut() {
                if sort.field == source.time_field {
                    sort.field = field.trim().into();
                }
            }
            source.time_field = field.trim().into();
        }
        source.name = self.name.read(cx).value().trim().to_owned();
        source.discover.query.query = self.kql.read(cx).value().to_string();
        source.discover.filters = serde_json::from_str(&self.filters.read(cx).value())
            .context("Filters must be a JSON array of complete Discover filter objects")?;
        source.discover.time = TimeRange {
            from: self.from.read(cx).value().to_string(),
            to: self.to.read(cx).value().to_string(),
        };
        self.guided_candidate(&mut source, cx)?;
        source.validate()?;
        Ok(source)
    }
    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop();
        let requested_generation = self.generation;
        let source = match self.candidate(cx) {
            Ok(source) => source,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.submit_query(source, false, false, requested_generation, window, cx);
    }
    fn replace_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_key_with_storage(false, window, cx);
    }
    fn replace_key_with_storage(
        &mut self,
        save: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.auth_error || self.busy || self.credential_pending {
            return;
        }
        let Some(source) = self.requested.clone() else {
            return;
        };
        if !self.key.read(cx).value().is_empty()
            && self
                .draft
                .as_ref()
                .is_some_and(|draft| draft.id != source.id || draft.endpoint != source.endpoint)
        {
            self.error =
                Some("Select the failed source before entering its replacement API key".into());
            cx.notify();
            return;
        }
        self.credentials.retry_save = save;
        self.stop();
        self.submit_query(source, true, false, self.generation, window, cx);
    }
    fn submit_query(
        &mut self,
        source: Source,
        replacement: bool,
        credential_retry: bool,
        requested_generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.requested = Some(source.clone());
        let definition = if replacement {
            None
        } else if credential_retry {
            self.requested_definition.clone()
        } else {
            let mut definition = source.clone();
            definition.discover.filters = serde_json::from_str(&self.filters.read(cx).value())
                .expect("candidate validated the base filter array");
            self.requested_definition = Some(definition.clone());
            Some(definition)
        };
        if !replacement {
            self.auth_error = false;
        }
        self.guided.invalidate_counts();
        self.configure_server_counts(cx);
        self.guided.open_scope = None;
        self.guided.custom = false;
        self.guided.connection = "Connecting…".into();
        self.worker.begin_attempt();
        let diagnostics = self.worker.diagnostics();
        if replacement {
            self.guided.catalog_cache.clear();
        }
        let count_cancel = self.guided.cancel.clone();
        #[cfg(test)]
        let test_key = self.guided.test_key.clone();
        let credential = source.credential_url(&self.vault);
        self.credentials.select(credential.clone());
        self.credentials.replacement = replacement;
        self.credentials.failure = None;
        // The explicit save choice belongs to this attempt, including invalid input.
        let retry_save = std::mem::take(&mut self.credentials.retry_save);
        let input = self.key.read(cx).value();
        if !input.is_empty() {
            match ApiKey::new(input.as_bytes().to_vec()) {
                Ok(key) => self.credentials.key = Some(key),
                Err(error) => {
                    self.error = Some(error.to_string());
                    self.busy = false;
                    cx.notify();
                    return;
                }
            }
        }
        // Drop the entire input/undo history; retained retry bytes have a zeroizing owner.
        self.key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Encoded API key")
                .masked(true)
        });
        let session_only = self.credentials.session_only && !retry_save;
        let memory_key = self.credentials.key.clone();
        let store = if session_only {
            None
        } else {
            memory_key
                .as_ref()
                .map(|key| self.write_key(&credential, key, cx))
        };
        let root = self.vault.clone();
        let directory = self.state.clone();
        self.busy = true;
        self.credential_pending = true;
        self.error = None;
        self.message =
            "Loading history… previous applied rows are retained until the request succeeds".into();
        self._credential = Some(cx.spawn(async move |this, cx| {
            let result: Result<(ApiKey, remote_log::transport::Kibana)> = async {
                #[cfg(test)]
                if let Some(bytes) = test_key {
                    let key = ApiKey::new(bytes.clone())?;
                    let selected = source.clone();
                    let client = cx
                        .background_executor()
                        .spawn(async move {
                            remote_log::transport::Kibana::new(
                                &selected,
                                ApiKey::new(bytes)?,
                                count_cancel,
                            )
                        })
                        .await?;
                    return Ok((key, client));
                }
                let key = if session_only {
                    memory_key.context("Enter an encoded API key for this session")?
                } else {
                    if let Some(store) = store {
                        store.await.map_err(credentials::StorageError::from)?;
                    }
                    let read = this.update(cx, |this, cx| this.read_key(&credential, cx))?;
                    let (_, bytes) = read
                        .await
                        .map_err(credentials::StorageError::from)?
                        .ok_or_else(|| {
                            credentials::StorageError(
                                "No OS-stored key for this vault/source; enter an encoded API key"
                                    .into(),
                            )
                        })?;
                    ApiKey::new(bytes)?
                };
                let client_key = key.clone();
                let selected = source.clone();
                let client = cx
                    .background_executor()
                    .spawn(async move {
                        remote_log::transport::Kibana::new(&selected, client_key, count_cancel)
                    })
                    .await?;
                Ok((key, client))
            }
            .await;
            let result = match result {
                Ok(key) => {
                    if let Some(saved) = definition.clone() {
                        cx.background_executor()
                            .spawn(
                                async move { remote_log::save_source(&directory, &root, &saved) },
                            )
                            .await
                            .map(|_| key)
                    } else {
                        Ok(key)
                    }
                }
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation != requested_generation {
                    return;
                }
                this.credential_pending = false;
                match result {
                    Ok((key, client)) => {
                        this.credentials.failure = None;
                        this.credentials.session_only = session_only;
                        if !session_only {
                            this.credentials.key = None;
                        }
                        this.guided.key_needed = false;
                        let client = client.with_diagnostics(diagnostics);
                        this.guided.count_client = Some(client.clone());
                        this.load_catalog(client, source.clone(), cx);
                        if let Some(definition) = definition {
                            if let Some(old) = this
                                .sources
                                .iter_mut()
                                .find(|saved| saved.id == definition.id)
                            {
                                *old = definition.clone();
                            } else {
                                this.sources.push(definition.clone());
                            }
                            if !replacement {
                                this.draft = Some(definition);
                            }
                        }
                        this.generation = if replacement {
                            this.worker.replace_key(key)
                        } else {
                            this.worker.apply(source, key)
                        };
                    }
                    Err(error) => {
                        this.busy = false;
                        this.guided.key_needed = true;
                        this.guided.connection = "Connection failed".into();
                        this.message =
                            "History request failed; previous applied results are retained.".into();
                        if let Some(storage) = error.downcast_ref::<credentials::StorageError>() {
                            this.credentials.failure = Some(storage.0.clone());
                            this.error = Some(credentials::recovery_message().into());
                            if storage.0.starts_with("No OS-stored key") {
                                this.error = Some(storage.0.clone());
                                if this.credentials.key.is_none() {
                                    this.credentials.failure = None;
                                }
                            }
                        } else {
                            this.error = Some(format!("{error:#}"));
                        }
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn stop(&mut self) {
        self._credential = None;
        self.credential_pending = false;
        self.generation = self.worker.stop();
        self.following = false;
        self.busy = false;
        self.more = false;
        self.proposal = None;
    }
    fn propose_start(&mut self, cx: &mut Context<Self>) {
        if self.following || self.busy || self.credential_pending {
            return;
        }
        let result = self
            .range
            .as_ref()
            .context("Apply history first")
            .and_then(Mode::rolling_same_duration);
        match result {
            Ok(mode)
                if self
                    .applied
                    .as_ref()
                    .is_some_and(|s| s.discover.time.absolute()) =>
            {
                self.proposal = Some(mode)
            }
            Ok(mode) => self.begin_live(mode),
            Err(error) => self.error = Some(error.to_string()),
        }
        cx.notify();
    }
    fn confirm_start(&mut self, rolling: bool, cx: &mut Context<Self>) {
        // A queued click from a dismissed/replaced disclosure is not consent
        // for a new session. Both choices consume the same pending proposal.
        if self.following || self.busy || self.credential_pending {
            return;
        }
        let Some(proposed) = self.proposal.take() else {
            return;
        };
        let mode = if rolling {
            proposed
        } else if let Some(range) = self.range.clone() {
            Mode::Fixed(range)
        } else {
            cx.notify();
            return;
        };
        self.begin_live(mode);
        cx.notify();
    }
    fn cancel_start(&mut self, cx: &mut Context<Self>) {
        // Dismissing consent must not close the history traversal or change the
        // draft, loaded snapshot, coverage status or worker generation.
        self.proposal = None;
        cx.notify();
    }
    fn begin_live(&mut self, mode: Mode) {
        let Some(applied) = &self.applied else {
            return;
        };
        self.requested = Some(applied.clone());
        self.requested_definition = self.applied_definition.clone();
        self.auth_error = false;
        self.generation = self.worker.start_live(mode, applied);
        self.following = true;
        self.busy = true;
        self.error = None;
        self.message = "Catching up — reconciling every page of the selected window".into();
    }
    fn receive(&mut self, update: Update, cx: &mut Context<Self>) {
        if update.generation != self.generation || self.credential_pending {
            return;
        }
        self.following = update.following;
        if let Some(view) = &self.view {
            view.update(cx, |view, cx| {
                view.investigation.live = update.following;
                cx.notify();
            });
        }
        self.more = false;
        self.busy = matches!(update.status, Status::CatchingUp { .. });
        if let Some(loaded) = update.loaded {
            if let Some(view) = &self.view {
                view.update(cx, |view, cx| {
                    view.remote_context(
                        loaded.clone(),
                        update.following,
                        matches!(update.status, Status::Complete { .. }),
                        cx,
                    )
                });
            }
            self.quality = Some(loaded.quality.clone());
            let changed = self
                .data
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, &loaded.window));
            if changed {
                // A new displayed window invalidates scope-count results, even
                // when following a fixed range in changing server data.
                self.guided.counts.clear();
                self.guided.open_scope = None;
                if self.applied.as_ref().is_none_or(|previous| {
                    previous.id != loaded.source.id
                        || previous.discover.data_view != loaded.source.discover.data_view
                }) {
                    reader_ui_state::set_log_columns(
                        &loaded.window.path.to_string_lossy(),
                        loaded.source.discover.columns.clone(),
                        cx,
                    );
                }
                let mut definition = loaded.source.clone();
                if self.requested.as_ref().is_some_and(|requested| {
                    requested.id == loaded.source.id
                        && requested.endpoint == loaded.source.endpoint
                        && requested.discover.filters == loaded.source.discover.filters
                }) {
                    if let Some(base) = &self.requested_definition {
                        definition.discover.filters = base.discover.filters.clone();
                        definition.discover.time = base.discover.time.clone();
                    }
                }
                self.applied_definition = Some(definition);
                self.applied = Some(loaded.source.clone());
                self.range = Some(loaded.range.clone());
                self.data = Some(loaded.window.clone());
                if let Some(view) = &self.view {
                    view.update(cx, |view, cx| {
                        view.replace_remote(loaded.window.clone(), cx)
                    });
                }
            }
            if let Some(view) = &self.view {
                let guided = self.guided_context();
                view.update(cx, |view, _| view.investigation.guided = guided);
            }
            if matches!(update.status, Status::History { .. }) {
                self.more = loaded.more;
            }
        }
        match update.status {
            Status::History {
                complete,
                limited,
                warning,
                quality,
            } => {
                self.auth_error = self.guided.auxiliary_auth_error;
                self.guided.adding = false;
                self.guided.connection = quality
                    .version
                    .as_ref()
                    .map_or("Connected".into(), |v| format!("Connected · Kibana {v}"));
                if self.auth_error {
                    self.guided.connection = "Auth failed → Replace key".into();
                }
                self.quality = Some(quality);
                self.error = warning;
                self.message = format!(
                    "History loaded{}{}",
                    if complete { " · End of result" } else { "" },
                    if limited {
                        " · Loaded limit reached; older pages not covered"
                    } else {
                        ""
                    }
                );
            }
            Status::CatchingUp {
                range,
                pages,
                scanned,
            } => {
                self.error = None;
                self.message = format!("Catching up · {pages} pages / {scanned} events scanned · {} → {} · Previous snapshot shown until the whole cycle completes", range.from, range.to);
            }
            Status::Complete {
                range,
                began,
                seconds,
                pages,
                quality,
            } => {
                self.auth_error = self.guided.auxiliary_auth_error;
                self.guided.connection = quality
                    .version
                    .as_ref()
                    .map_or("Connected".into(), |v| format!("Connected · Kibana {v}"));
                if self.auth_error {
                    self.guided.connection = "Auth failed → Replace key".into();
                }
                self.quality = Some(quality);
                self.error = None;
                self.message = format!("Window traversal complete · {pages} pages in {seconds:.2}s · cycle began {began} · {} → {}. Later searchable arrivals await a new cycle; no five-second guarantee.", range.from, range.to);
            }
            Status::Reconnecting {
                attempt,
                after_seconds,
                error,
            } => {
                self.error = Some(error);
                self.message = format!("Reconnecting · attempt {attempt}, retry in {after_seconds}s · Next cycle catches up every page in the current selected window. Offline periods outside that window are not recovered.");
            }
            Status::Incomplete {
                range,
                scanned,
                reason,
            } => {
                self.error = Some(reason);
                self.message = format!("Incomplete · {scanned} scanned · Following stopped; previous snapshot retained{}", range.map(|r|format!(" · {} → {}",r.from,r.to)).unwrap_or_default());
            }
            Status::TimedOut => {
                self.guided.connection = "Search timeout".into();
                self.error = Some(remote_log::result::SearchTimeout.to_string());
                self.message = "Search timeout · previous query and rows retained".into();
            }
            Status::AuthError(error) => {
                self.auth_error = true;
                self.guided.connection = "Auth failed → Replace key".into();
                self.error = Some(if self.credentials.session_only {
                    "Kibana rejected the key used for this session. Enter a new key".into()
                } else {
                    error.to_string()
                });
                self.message = "Enter a new API key and choose how to use it to re-run the same query. Loaded rows and settings are retained.".into();
            }
            Status::IndexPatternRequired => {
                self.error = Some(remote_log::result::IndexPatternRequired.to_string());
                self.message = "Enter the index pattern above and Apply again.".into();
            }
            Status::Error(error) => {
                self.guided.connection = "Unreachable or request failed".into();
                self.error = Some(error);
                self.message = "Request failed; previous applied query and rows retained. Correct the source/query and Apply.".into();
            }
            Status::Stopped { warning } => {
                if warning.is_some() {
                    self.error = warning;
                }
                self.message = "Stopped · loaded rows retained; Apply to page a new history snapshot, or Start explicitly.".into();
            }
        }
        self.configure_server_counts(cx);
        cx.notify();
    }
    fn export(&mut self, cx: &mut Context<Self>) {
        if self.exporting {
            return;
        }
        let Some(data) = self.data.clone() else {
            return;
        };
        self.exporting = true;
        let destination = cx.prompt_for_new_path(&self.vault, Some("loaded-kibana-events.jsonl"));
        cx.spawn(async move |this, cx| {
            let selected = match destination.await {
                Ok(Ok(path)) => Ok(path),
                Ok(Err(_)) => Err("The export save dialog could not be opened"),
                Err(_) => Err("The export save dialog closed unexpectedly"),
            };
            let path = match selected {
                Ok(Some(path)) => path,
                result => {
                    let _ = this.update(cx, |this, cx| {
                        this.exporting = false;
                        if let Err(error) = result {
                            this.error = Some(error.into());
                        }
                        cx.notify();
                    });
                    return;
                }
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    let _admission = okilum_write_authority::operation_for_path(&path)?;
                    let mut temp = tempfile::NamedTempFile::new_in(
                        path.parent().context("Export folder missing")?,
                    )?;
                    data.export(temp.as_file_mut())?;
                    temp.as_file().sync_all()?;
                    temp.persist(&path).map_err(|e| e.error)?;
                    Ok::<_, anyhow::Error>(())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.exporting = false;
                match result {
                    Ok(()) => this.message =
                        "Exported loaded events only, with original fields and document identities"
                            .into(),
                    Err(error) => {
                        this.error = Some(format!("Cannot export loaded events: {error:#}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Panel {
    // Bound debug stack use by materializing one native section at a time.
    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut header = v_flex()
            .id("remote-header-scroll")
            .p_3()
            .gap_2()
            .min_w_0()
            .max_h(relative(0.75))
            .overflow_y_scroll()
            .flex_shrink_0()
            .child(self.render_guided(cx));
        header = header.child(self.render_actions(cx));
        if self.guided.advanced {
            header = header.child(self.render_advanced_controls(cx));
        }
        if self.proposal.is_some() {
            header = header.child(self.render_start_confirmation(cx));
        }
        header = header.child(self.render_status(cx));
        if self.guided.details_open {
            let text = self.worker.diagnostics().lock().unwrap().copy_text();
            header = header.child(
                v_flex()
                    .debug_selector(|| "remote-connection-section".into())
                    .gap_2()
                    .child(
                        Button::new("remote-copy-details")
                            .debug_selector(|| "remote-copy-details".into())
                            .small()
                            .label("Copy connection details")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let text = this.worker.diagnostics().lock().unwrap().copy_text();
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            })),
                    )
                    .child(
                        div()
                            .id("remote-details-text")
                            .max_h(px(180.))
                            .overflow_y_scroll()
                            .children(
                                text.lines()
                                    .map(|line| div().text_xs().child(line.to_owned())),
                            ),
                    ),
            );
        }
        header.into_any_element()
    }
    fn render_source_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut header = v_flex().gap_2();
        if self.guided.adding {
            header = header
                .child(
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(div().w(px(180.)).child(Input::new(&self.name).small()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&self.url).small()),
                        )
                        .child(
                            Button::new("remote-import")
                                .debug_selector(|| "remote-import".into())
                                .small()
                                .label("Add source / import")
                                .disabled(self.busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.import(window, cx)),
                                ),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&self.pattern).small()))
                        .child(
                            div()
                                .w(px(240.))
                                .child(Input::new(&self.time_field).small()),
                        ),
                );
        }
        header.into_any_element()
    }
    fn render_key_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut header = v_flex().gap_2();
        if self.auth_error
            || self.guided.key_needed
            || self.draft.as_ref().is_some_and(|draft| {
                self.applied.as_ref().is_none_or(|applied| {
                    applied.id != draft.id || applied.endpoint != draft.endpoint
                })
            })
        {
            header = header.child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.key).small()))
                    .when(self.auth_error, |row| {
                        row.child(
                            Button::new("remote-replace-key")
                                .debug_selector(|| "remote-replace-key".into())
                                .small()
                                .label(if self.credentials.session_only {
                                    "Replace key (session only)"
                                } else {
                                    "Replace API key"
                                })
                                .disabled(self.busy || self.credential_pending)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.replace_key(window, cx)),
                                ),
                        )
                        .when(self.credentials.session_only, |row| {
                            row.child(
                                Button::new("remote-replace-save-key")
                                    .debug_selector(|| "remote-replace-save-key".into())
                                    .small()
                                    .label("Save to keychain")
                                    .disabled(self.busy || self.credential_pending)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.replace_key_with_storage(true, window, cx)
                                    })),
                            )
                        })
                    }),
            );
        }
        header.into_any_element()
    }
    fn render_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut header = v_flex().gap_2();
        header = header.child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    Button::new("remote-refresh")
                        .debug_selector(|| "remote-refresh".into())
                        .small()
                        .label("Refresh")
                        .disabled(self.draft.is_none() || self.busy)
                        .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx))),
                )
                .child(
                    Button::new("remote-restore-applied")
                        .debug_selector(|| "remote-restore-applied".into())
                        .small()
                        .ghost()
                        .label("Restore applied advanced inputs")
                        .disabled(self.applied.is_none() || self.busy || self.credential_pending)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.restore_applied(window, cx)),
                        ),
                )
                .child(
                    Button::new("remote-more")
                        .small()
                        .label(
                            if self
                                .applied
                                .as_ref()
                                .is_some_and(|s| s.discover.sort[0].order == "asc")
                            {
                                "Load later"
                            } else {
                                "Load older"
                            },
                        )
                        .disabled(self.busy || !self.more)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.busy = true;
                            this.generation = this.worker.more();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("remote-start")
                        .debug_selector(|| "remote-start".into())
                        .small()
                        .label("Start applied query")
                        .disabled(
                            self.applied.is_none()
                                || self.following
                                || self.busy
                                || self.credential_pending
                                || self.proposal.is_some(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.propose_start(cx))),
                )
                .child(
                    Button::new("remote-stop")
                        .debug_selector(|| "remote-stop".into())
                        .small()
                        .label("Stop")
                        .disabled(!self.following && !self.busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.stop();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("remote-connection-details")
                        .debug_selector(|| "remote-connection-details".into())
                        .small()
                        .ghost()
                        .label("Connection details")
                        .selected(self.guided.details_open)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.guided.details_open = !this.guided.details_open;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("remote-advanced")
                        .debug_selector(|| "remote-advanced".into())
                        .small()
                        .ghost()
                        .label("Advanced")
                        .selected(self.guided.advanced)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.guided.advanced = !this.guided.advanced;
                            cx.notify();
                        })),
                ),
        );
        header.into_any_element()
    }
    fn render_advanced_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut header = v_flex().gap_2();
        if self.guided.advanced {
            header=header
                .child(div().text_xs().child("Advanced · Discover import, server KQL, complete filter objects and exact timestamps"))
                .child(h_flex().gap_2().child(div().flex_1().min_w_0().child(Input::new(&self.url).small()))
                    .child(Button::new("remote-advanced-import").small().label("Import Discover").on_click(cx.listener(|this,_,window,cx|this.import(window,cx)))))
                .child(h_flex().gap_2().child(div().flex_1().child(Input::new(&self.pattern).small())).child(div().flex_1().child(Input::new(&self.time_field).small())))
                .child(Input::new(&self.kql).small())
                .child(Input::new(&self.filters).small())
                .child(h_flex().gap_2().child(div().flex_1().child(Input::new(&self.from).small())).child(div().flex_1().child(Input::new(&self.to).small()))
                    .child(Button::new("remote-apply").small().label("Apply query and filters").on_click(cx.listener(|this,_,window,cx|this.apply(window,cx)))))
                .child(h_flex().gap_2()
                    .child(Button::new("remote-reverse").small().label("Reverse sort (Apply)").disabled(self.busy).on_click(cx.listener(|this,_,_,cx|{if let Some(draft)=this.draft.as_mut(){for sort in &mut draft.discover.sort{sort.order=if sort.order=="asc"{"desc"}else{"asc"}.into();}cx.notify();}})))
                    .child(Button::new("remote-export").small().label("Export loaded JSONL").disabled(self.data.is_none()||self.exporting).on_click(cx.listener(|this,_,_,cx|this.export(cx)))))
                .child(div().text_xs().child(remote_log::LIVE_LIMITATION))
                .child(div().text_xs().child(remote_log::KQL_LIMITATION));
        }
        header.into_any_element()
    }
    fn render_start_confirmation(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut header = v_flex().gap_2();
        if self.proposal.is_some() {
            header=header.child(h_flex().gap_2()
                .child(div().flex_1().text_sm().child("Choose a rolling window of the same duration or keep this fixed historical range. Following starts only after you choose."))
                .child(Button::new("remote-confirm-rolling").debug_selector(||"remote-confirm-rolling".into()).small().label("Follow rolling window").disabled(self.following||self.busy||self.credential_pending).on_click(cx.listener(|this,_,_,cx|this.confirm_start(true,cx))))
                .child(Button::new("remote-confirm-fixed").debug_selector(||"remote-confirm-fixed".into()).small().label("Follow fixed range").disabled(self.following||self.busy||self.credential_pending).on_click(cx.listener(|this,_,_,cx|this.confirm_start(false,cx))))
                .child(Button::new("remote-cancel-start").debug_selector(||"remote-cancel-start".into()).small().ghost().label("Cancel").on_click(cx.listener(|this,_,_,cx|this.cancel_start(cx)))));
        }
        header.into_any_element()
    }
    fn environment_name(&self) -> &str {
        self.draft
            .as_ref()
            .or(self.requested.as_ref())
            .or(self.applied.as_ref())
            .map_or("No environment", |source| source.name.as_str())
    }
    fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut header = v_flex().gap_2();
        let environment = self.environment_name();
        let retained_environment = self
            .applied
            .as_ref()
            .filter(|applied| {
                self.draft.as_ref().is_some_and(|draft| {
                    draft.id != applied.id || draft.endpoint != applied.endpoint
                })
            })
            .map(|source| {
                format!(
                    "Loaded rows remain from {} until Refresh succeeds",
                    source.name
                )
            });
        let scope = self
            .applied
            .as_ref()
            .map(|source| {
                source
                    .discover
                    .filters
                    .iter()
                    .filter(|filter| {
                        filter
                            .pointer("/meta/type")
                            .and_then(serde_json::Value::as_str)
                            == Some("scope")
                    })
                    .filter_map(|filter| {
                        Some(format!(
                            "{} = {}",
                            filter.pointer("/meta/key")?.as_str()?,
                            filter.pointer("/meta/value")?.as_str()?
                        ))
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        header = header
            .child(
                div()
                    .debug_selector(|| "remote-status".into())
                    .text_sm()
                    .child(format!(
                        "{environment} · {}{} · Scope: {}",
                        self.guided.connection,
                        self.credential_status(),
                        if scope.is_empty() {
                            "All values"
                        } else {
                            &scope
                        }
                    )),
            )
            .children(retained_environment.map(|label| div().text_xs().child(label)))
            .children(self.range.as_ref().map(|range| {
                div().text_xs().child(format!(
                    "Applied window · {} → {} · UTC · bounded changing data",
                    guided::utc_label(&range.from), guided::utc_label(&range.to)
                ))
            }))
            .children(self.quality.as_ref().map(|quality| {
                div()
                    .text_sm()
                    .child(quality.label(self.data.as_ref().map_or(0, |data| data.events.len())))
                    .when(quality.incomplete(),|line|line.child(div().text_xs().child(
                        "Incomplete window: partial responses, failed shards/clusters or a deadline can omit events. A cap stops at 10,000 rows / 16 MiB; narrow the range and Refresh. Unknown metadata cannot certify completion."
                    )))
            }))
            .children(self.error.as_ref().filter(|_| self.credentials.failure.is_none()).map(|error| {
                div()
                    .text_sm()
                    .text_color(brand::palette(cx).danger)
                    .child(error.clone())
            }))
            .child(div().text_xs().child(self.message.clone()));
        header.into_any_element()
    }
}

impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.sources_loaded && !self.busy {
            if let Some(link) = self.pending_link.take() {
                let result = link.validate().and_then(|_| {
                    let mut source = self
                        .sources
                        .iter()
                        .find(|s| s.id == link.source_id)
                        .cloned()
                        .context(
                            "This source is not saved in the selected vault; import it explicitly",
                        )?;
                    anyhow::ensure!(
                        source.discover.data_view == link.state.data_view,
                        "The linked data view differs from this saved source"
                    );
                    source.discover = link.state;
                    Ok(source)
                });
                match result {
                    Ok(source) => {
                        self.guided.scope.clear();
                        self.guided.chips.clear();
                        if let Some(view) = &self.view {
                            view.update(cx, |view, cx| view.reset_remote_filters(window, cx));
                        }
                        self.choose(source, window, cx);
                        self.message="Linked query and fixed time range loaded. Apply re-queries the server; this is not an immutable snapshot.".into();
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
        }
        if self.view.is_none() {
            if let Some(data) = self.data.clone() {
                let view = cx.new(|cx| reader_log::LogView::remote(data, window, cx));
                view.update(cx, |view, cx| {
                    view.enable_server_filters(cx);
                    view.set_count_provider(
                        Arc::new(server_filters::CountsUnavailable(
                            "Kibana field catalog is loading; reopen this picker after connecting"
                                .into(),
                        )),
                        cx,
                    );
                });
                self._chips = Some(cx.subscribe_in(
                    &view,
                    window,
                    |this, _, event: &reader_log::filters::FilterChipsChanged, window, cx| {
                        this.chips_changed(&event.0, window, cx);
                    },
                ));
                self.install_investigation(&view, window, cx);
                self.view = Some(view);
                self.configure_server_counts(cx);
            }
        }
        let header = self.render_header(cx);
        let mut body = v_flex()
            .size_full()
            .min_h_0()
            .child(TitleBar::new().child("Kibana logs"))
            .child(header);
        body = if let Some(view) = &self.view {
            body.child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(view.clone())
                    .when(self.guided.open_scope.is_some(), |body| {
                        body.child(gpui::deferred(
                            div()
                                .id("remote-scope-overlay-scroll")
                                .debug_selector(|| "remote-scope-overlay".into())
                                .absolute()
                                .top_0()
                                .left_0()
                                .w(px(480.))
                                .max_w_full()
                                .max_h_full()
                                .overflow_y_scroll()
                                .p_3()
                                .bg(cx.theme().popover)
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded_md()
                                .shadow_md()
                                .occlude()
                                .child(self.render_scope_values(cx)),
                        ))
                    }),
            )
        } else {
            body.child(div().flex_1().p_4().child("No history loaded. Events stay in memory until this window closes. Keys use OS storage unless you explicitly choose this session only."))
        };
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn source() -> Source {
        Source::import("Work", "https://logs.example/app/discover#/?_a=(index:events,query:(language:kuery,query:''),columns:!(description),sort:!(!('@timestamp',desc)))&_g=(time:(from:'2026-10-20T12:00:00Z',to:'2026-10-20T12:15:00Z'),refreshInterval:(pause:!t,value:0))").unwrap()
    }
    #[gpui::test]
    async fn invalid_apply_and_missing_keychain_preserve_applied_history_without_file_fallback(
        cx: &mut TestAppContext,
    ) {
        // The real session worker wakes this test from an OS thread, just like
        // PDF and file-watcher tests; it is not driven by GPUI's test scheduler.
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        let state = dir.path().join("state");
        std::fs::create_dir_all(&root).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_ui_state::install(&state, cx);
        });
        let mut panel = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| Panel::new(root.clone(), state.clone(), window, cx));
            panel = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let panel = panel.unwrap();
        visual.run_until_parked();
        let applied = source();
        let id = applied.id;
        let data=Arc::new(EventWindow::new(vec![Arc::new(serde_json::json!({"_index":"events","_id":"kept","_source":{"@timestamp":"2026-10-20T12:01:00.123456789Z","level":"INFO","description":"previous result","nested":{"all":[1,2]}}}))],&applied).unwrap());
        panel.update_in(visual, |panel, window, cx| {
            panel.choose(applied.clone(), window, cx);
            panel.applied = Some(applied);
            panel.data = Some(data.clone());
            panel.filters.update(cx, |input, cx| {
                input.set_value("[{\"script\":{}}]", window, cx)
            });
            panel.apply(window, cx);
            assert!(!panel.busy);
            assert!(panel.error.as_ref().unwrap().contains("Unsupported filter"));
            assert_eq!(panel.applied.as_ref().unwrap().id, id);
            assert!(Arc::ptr_eq(panel.data.as_ref().unwrap(), &data));
            panel
                .filters
                .update(cx, |input, cx| input.set_value("[]", window, cx));
            panel.key.update(cx, |input, cx| {
                input.set_value("sentinel-secret", window, cx)
            });
            assert!(panel.key.read(cx).presentation().is_masked());
            panel.apply(window, cx);
            assert!(
                panel.busy,
                "positive control: valid draft reached OS credential lookup"
            );
            assert!(
                panel.key.read(cx).value().is_empty(),
                "key input and undo entity replaced"
            );
        });
        visual.run_until_parked();
        panel.read_with(visual, |panel, _| {
            // GPUI TestPlatform deliberately returns no OS credential. This
            // proves fail-closed wiring, not native keychain integration.
            assert!(panel.error.as_ref().unwrap().contains("No OS-stored key"));
            assert!(!panel.busy);
            assert_eq!(panel.applied.as_ref().unwrap().id, id);
            assert!(Arc::ptr_eq(panel.data.as_ref().unwrap(), &data));
        });
        assert!(
            remote_log::load_sources(&state, &root).unwrap().is_empty(),
            "no credential-file or source fallback after keychain refusal"
        );
        let mut exported = Vec::new();
        data.export(&mut exported).unwrap();
        let row: serde_json::Value = serde_json::from_slice(&exported).unwrap();
        assert_eq!(row["_id"], "kept");
        assert_eq!(row["_source"]["nested"]["all"], serde_json::json!([1, 2]));

        // Exercise the platform dialog receiver's nested result rather than
        // only the serializer. Cancel retains the session; selection writes
        // the same original all-fields event, without credential input.
        panel.update(visual, |panel, cx| panel.export(cx));
        assert!(
            visual.did_prompt_for_new_path(),
            "export reached the dialog"
        );
        visual.simulate_new_path_selection(|_| None);
        visual.run_until_parked();
        panel.read_with(visual, |panel, _| {
            assert!(!panel.exporting);
            assert!(Arc::ptr_eq(panel.data.as_ref().unwrap(), &data));
        });
        let output = dir.path().join("loaded.jsonl");
        let selected = output.clone();
        panel.update(visual, |panel, cx| panel.export(cx));
        assert!(visual.did_prompt_for_new_path());
        visual.simulate_new_path_selection(move |_| Some(selected));
        visual.run_until_parked();
        assert_eq!(std::fs::read(output).unwrap(), exported);
        panel.read_with(visual, |panel, _| assert!(!panel.exporting));
        panel.update(visual, |panel, cx| {
            panel.range = Some(source().discover.time);
            let before = panel.worker.generation();
            panel.propose_start(cx);
            assert!(panel.proposal.is_some());
            assert!(!panel.following);
            assert_eq!(
                panel.worker.generation(),
                before,
                "historical proposal sends no live command before a choice"
            );
            panel.message = "Waiting for the real worker Stop acknowledgement".into();
            panel.stop();
        });
        visual
            .condition(&panel, |panel, _| panel.message.starts_with("Stopped ·"))
            .await;
        panel.read_with(visual, |panel, _| {
            assert!(!panel.following && !panel.busy);
            assert!(Arc::ptr_eq(panel.data.as_ref().unwrap(), &data));
        });
        cx.quit();
    }
    #[gpui::test]
    fn reconciled_windows_keep_selection_marks_and_old_snapshot_by_document_identity(
        cx: &mut TestAppContext,
    ) {
        let source = source();
        let event = |id: &str| {
            Arc::new(
                serde_json::json!({"_index":"events","_id":id,"_source":{"@timestamp":"2026-10-20T12:01:00.123456789Z","description":id}}),
            )
        };
        let initial = Arc::new(EventWindow::new(vec![event("a"), event("b")], &source).unwrap());
        cx.update(crate::test_init);
        let mut view = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| reader_log::LogView::remote(initial.clone(), window, cx));
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        visual.run_until_parked();
        let old = view.update(visual, |view, _| {
            view.selected = Some(0);
            view.anchor = Some(0);
            view.marks.insert((0, 0));
            view.snapshot_of(0).unwrap()
        });
        let updated = Arc::new(
            EventWindow::new(vec![event("b"), event("new"), event("a")], &source).unwrap(),
        );
        view.update(visual, |view, cx| view.replace_remote(updated, cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(view.selected, Some(2));
            assert_eq!(view.anchor, Some(2));
            assert!(view.marks.contains(&(0, 2)));
        });
        let reader_log::Snapshot::Remote(old) = old else {
            panic!("expected an immutable remote snapshot")
        };
        assert!(Arc::ptr_eq(&old, &initial));
        assert_eq!(old.events[0]["_id"], "a");
        let mut other = source;
        other.endpoint = "https://other.example".into();
        let updated = Arc::new(EventWindow::new(vec![event("a")], &other).unwrap());
        view.update(visual, |view, cx| view.replace_remote(updated, cx));
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert!(view.selected.is_none() && view.marks.is_empty())
        });
        cx.quit();
    }
}

#[cfg(test)]
#[path = "reader_log_remote/recovery_tests.rs"]
mod recovery_tests;

#[cfg(test)]
#[path = "reader_log_remote/start_tests.rs"]
mod start_tests;

#[cfg(test)]
#[path = "reader_log_remote/protocol_tests.rs"]
mod protocol_tests;

#[cfg(test)]
#[path = "reader_log_remote/investigation_tests.rs"]
mod investigation_tests;

#[cfg(test)]
#[path = "reader_log_remote/guided_tests.rs"]
mod guided_tests;

#[cfg(test)]
#[path = "reader_log_remote/session_key_tests.rs"]
mod session_key_tests;

#[cfg(test)]
#[path = "reader_log_remote/polish_tests.rs"]
mod polish_tests;
