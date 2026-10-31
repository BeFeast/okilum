//! Kibana controls around the existing LogView, not a second event renderer.
use crate::*;
use anyhow::Context as _;
use okilum_core::remote_log::live::{Mode, Status, Update, Worker};
use okilum_core::remote_log::{self, discover::TimeRange, transport::ApiKey, EventWindow, Source};
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
            eprintln!("Cannot open remote logs: {error}");
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
}
impl Panel {
    fn new(vault: PathBuf, state: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &'static str, cx: &mut Context<Self>, window: &mut Window| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let name = input("Source name", cx, window);
        let url = input("Paste a complete Discover URL", cx, window);
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Encoded API key → OS credential storage")
                .masked(true)
        });
        let kql = input("Server KQL", cx, window);
        let filters = input("Discover filter objects as JSON", cx, window);
        let from = input("From: now-15m or RFC3339", cx, window);
        let to = input("To: now or RFC3339", cx, window);
        let (worker, events) = Worker::start();
        let _events = cx.spawn(async move |this, cx| {
            while let Ok(reply) = events.recv().await {
                if this
                    .update(cx, |this, cx| {
                        if reply.generation != this.generation || this.credential_pending {
                            return;
                        }
                        this.receive(reply, cx);
                        cx.notify();
                    })
                    .is_err()
                {
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
        Self{generation:0,following:false,credential_pending:false,proposal:None,pending_link:None,sources_loaded:false,vault,state,name,url,key,kql,filters,from,to,sources:Vec::new(),draft:None,applied:None,range:None,view:None,data:None,busy:false,more:false,exporting:false,message:"Import Discover, review the query and filters, then Apply to load history. Start never runs automatically.".into(),error:None,worker,_events,_load,_credential:None}
    }
    fn choose(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        self.stop();
        self.key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Encoded API key → OS credential storage")
                .masked(true)
        });
        self.name
            .update(cx, |i, cx| i.set_value(&source.name, window, cx));
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
        self.error = None;
        cx.notify();
    }
    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = Source::import(&self.name.read(cx).value(), &self.url.read(cx).value());
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
        source.name = self.name.read(cx).value().trim().to_owned();
        source.discover.query.query = self.kql.read(cx).value().to_string();
        source.discover.filters = serde_json::from_str(&self.filters.read(cx).value())
            .context("Filters must be a JSON array of complete Discover filter objects")?;
        source.discover.time = TimeRange {
            from: self.from.read(cx).value().to_string(),
            to: self.to.read(cx).value().to_string(),
        };
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
        let mut secret = self.key.read(cx).value().as_bytes().to_vec();
        // Drop the entire input/undo history after handing bytes to the keychain task.
        self.key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Encoded API key → OS credential storage")
                .masked(true)
        });
        let credential = source.credential_url(&self.vault);
        let store = if secret.is_empty() {
            None
        } else {
            Some(cx.write_credentials(&credential, "ApiKey", &secret))
        };
        secret.fill(0);
        let root = self.vault.clone();
        let directory = self.state.clone();
        self.busy = true;
        self.credential_pending = true;
        self.error = None;
        self.message =
            "Loading history… previous applied rows are retained until the request succeeds".into();
        self._credential = Some(cx.spawn(async move |this, cx| {
            let result: Result<ApiKey> = async {
                if let Some(store) = store {
                    store.await.context(
                        "OS credential storage could not save the key; no file fallback is used",
                    )?;
                }
                let read = cx.update(|cx| cx.read_credentials(&credential));
                let (_, bytes) = read
                    .await
                    .context("OS credential storage is unavailable")?
                    .context("No OS-stored key for this vault/source; enter an encoded API key")?;
                ApiKey::new(bytes)
            }
            .await;
            let result = match result {
                Ok(key) => {
                    let saved = source.clone();
                    let stored = cx
                        .background_executor()
                        .spawn(async move { remote_log::save_source(&directory, &root, &saved) })
                        .await;
                    stored.map(|_| key)
                }
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation != requested_generation {
                    return;
                }
                this.credential_pending = false;
                match result {
                    Ok(key) => {
                        if let Some(old) = this.sources.iter_mut().find(|s| s.id == source.id) {
                            *old = source.clone();
                        } else {
                            this.sources.push(source.clone());
                        }
                        this.draft = Some(source.clone());
                        this.generation = this.worker.apply(source, key);
                    }
                    Err(error) => {
                        this.busy = false;
                        this.message =
                            "History request failed; previous applied results are retained.".into();
                        this.error = Some(format!("{error:#}"));
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
        if self.following || self.busy {
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
    fn begin_live(&mut self, mode: Mode) {
        let Some(applied) = &self.applied else {
            return;
        };
        self.generation = self.worker.start_live(mode, applied);
        self.following = true;
        self.busy = true;
        self.error = None;
        self.message = "Catching up — reconciling every page of the selected window".into();
    }
    fn receive(&mut self, update: Update, cx: &mut Context<Self>) {
        self.following = update.following;
        self.more = false;
        self.busy = matches!(update.status, Status::CatchingUp { .. });
        if let Some(loaded) = update.loaded {
            let changed = self
                .data
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, &loaded.window));
            if changed {
                if self.applied.as_ref().is_none_or(|previous| {
                    previous.id != loaded.source.id
                        || previous.discover.columns != loaded.source.discover.columns
                }) {
                    reader_ui_state::set_log_columns(
                        &loaded.window.path.to_string_lossy(),
                        loaded.source.discover.columns.clone(),
                        cx,
                    );
                }
                self.applied = Some(loaded.source.clone());
                self.range = Some(loaded.range.clone());
                self.data = Some(loaded.window.clone());
                if let Some(view) = &self.view {
                    view.update(cx, |view, cx| {
                        view.replace_remote(loaded.window.clone(), cx)
                    });
                }
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
            } => {
                self.error = warning;
                self.message = format!(
                    "History loaded{}{}",
                    if complete { " · End of snapshot" } else { "" },
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
            } => {
                self.error = None;
                self.message = format!("Window snapshot complete · {pages} pages in {seconds:.2}s · cycle began {began} · {} → {}. Later searchable arrivals await a new cycle; no five-second guarantee.", range.from, range.to);
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
            Status::Error(error) => {
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

impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
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
                        self.choose(source, window, cx);
                        self.message="Linked query and fixed time range loaded. Apply re-queries the server; this is not an immutable snapshot.".into();
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
        }
        if self.view.is_none() {
            if let Some(data) = self.data.clone() {
                self.view = Some(cx.new(|cx| reader_log::LogView::remote(data, window, cx)));
            }
        }
        let sources = self.sources.clone();
        let weak = cx.entity().downgrade();
        let saved = Button::new("remote-saved")
            .small()
            .label("Saved sources")
            .disabled(self.busy)
            .dropdown_menu(move |mut menu, _, _| {
                for source in &sources {
                    let weak = weak.clone();
                    let source = source.clone();
                    menu = menu.item(PopupMenuItem::new(source.name.clone()).on_click(
                        move |_, window, cx| {
                            let _ =
                                weak.update(cx, |this, cx| this.choose(source.clone(), window, cx));
                        },
                    ));
                }
                menu
            });
        let mut body=v_flex().size_full().min_h_0().child(TitleBar::new().child("Kibana logs"))
            .child(v_flex().p_3().gap_2().flex_none()
                .child(h_flex().gap_2().child(div().w(px(180.)).child(Input::new(&self.name).small())).child(div().flex_1().child(Input::new(&self.url).small())).child(Button::new("remote-import").small().label("Import Discover URL").disabled(self.busy).on_click(cx.listener(|this,_,window,cx|this.import(window,cx)))).child(saved))
                .child(Input::new(&self.key).small())
                .child(h_flex().gap_2().child(div().flex_1().child(Input::new(&self.kql).small())).child(Button::new("remote-apply").small().label("Apply query and filters").on_click(cx.listener(|this,_,window,cx|this.apply(window,cx)))))
                .child(div().text_xs().child("Filters JSON — includes disabled, negated and pinned filters; edits apply together with KQL"))
                .child(Input::new(&self.filters).small())
                .child(h_flex().gap_2().child(div().flex_1().child(Input::new(&self.from).small())).child(div().flex_1().child(Input::new(&self.to).small()))
                    .child(Button::new("remote-reverse").small().label("Reverse sort (Apply)").disabled(self.busy).on_click(cx.listener(|this,_,_,cx|{if let Some(draft)=this.draft.as_mut(){for sort in &mut draft.discover.sort{sort.order=if sort.order=="asc"{"desc"}else{"asc"}.into();}cx.notify();}}))))
                .child(h_flex().gap_2().child(Button::new("remote-more").small().label(if self.applied.as_ref().is_some_and(|s|s.discover.sort[0].order=="asc"){ "Load later" }else{"Load older"}).disabled(self.busy||!self.more).on_click(cx.listener(|this,_,_,cx|{this.busy=true;this.generation=this.worker.more();cx.notify();})))
                    .child(Button::new("remote-export").small().label("Export loaded JSONL").disabled(self.data.is_none()||self.exporting).on_click(cx.listener(|this,_,_,cx|this.export(cx))))
                    .child(Button::new("remote-start").small().label("Start applied query").disabled(self.applied.is_none()||self.following||self.busy).on_click(cx.listener(|this,_,_,cx|this.propose_start(cx))))
                    .child(Button::new("remote-stop").small().label("Stop").disabled(!self.following&&!self.busy).on_click(cx.listener(|this,_,_,cx|{this.stop();cx.notify();}))))
                .when(self.proposal.is_some(),|body|body.child(h_flex().gap_2()
                    .child(div().flex_1().text_sm().child("Choose a rolling window of the same duration or keep this fixed historical range. Following starts only after you choose."))
                    .child(Button::new("remote-confirm-rolling").small().label("Follow rolling window").on_click(cx.listener(|this,_,_,cx|{if let Some(mode)=this.proposal.take(){this.begin_live(mode);cx.notify();}})))
                    .child(Button::new("remote-confirm-fixed").small().label("Follow fixed range").on_click(cx.listener(|this,_,_,cx|{this.proposal=None;if let Some(range)=this.range.clone(){this.begin_live(Mode::Fixed(range));cx.notify();}})))))
                .child(div().text_xs().child(remote_log::LIVE_LIMITATION))
                .child(div().text_xs().child(remote_log::KQL_LIMITATION))
                .child(div().text_sm().child(self.message.clone()))
                .children(self.applied.as_ref().map(|source|div().text_xs().child(format!("Applied source: {} · Start follows the applied query; draft edits require Apply",source.name))))
                .children(self.range.as_ref().map(|range|div().text_xs().child(format!("Applied absolute range: {} → {} · Limit 10,000 events / 16 MiB original JSON; loaded-only export",range.from,range.to))))
                .children(self.error.as_ref().map(|error|div().text_sm().text_color(brand::palette(cx).danger).child(error.clone()))));
        body = if let Some(view) = &self.view {
            body.child(div().flex_1().min_h_0().child(view.clone()))
        } else {
            body.child(div().flex_1().p_4().child("No history loaded. Events stay in memory until this window closes; credentials stay in the OS credential store."))
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
