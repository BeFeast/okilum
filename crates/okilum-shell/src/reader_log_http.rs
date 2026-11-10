//! Explicit, vault-scoped HTTPS file sessions over the ordinary LogView.
use crate::*;
use anyhow::Result;
use gpui_component::menu::DropdownMenu as _;
#[path = "reader_log_http/model.rs"]
mod model;
#[path = "reader_log_http/worker.rs"]
mod worker;
use model::Source;

#[derive(Default)]
struct Windows(std::collections::BTreeMap<String, WindowHandle<Root>>);
impl Global for Windows {}

pub(crate) fn show(vault: PathBuf, cx: &mut App) {
    let scope = okilum_core::remote_log::vault_scope(&vault);
    if let Some(handle) = cx
        .try_global::<Windows>()
        .and_then(|w| w.0.get(&scope))
        .copied()
    {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
    }
    let state = match reader_ui_state::state_directory(cx) {
        Ok(state) => state,
        Err(error) => {
            diagnostic_error!("Cannot open HTTPS logs: {error}");
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
        window.set_window_title("HTTPS log files");
        let panel = cx.new(|cx| Panel::new(vault, state, window, cx));
        cx.new(|cx| Root::new(panel, window, cx))
    }) {
        if cx.try_global::<Windows>().is_none() {
            cx.set_global(Windows::default());
        }
        cx.global_mut::<Windows>().0.insert(scope, handle);
    }
}
struct Panel {
    vault: PathBuf,
    state: PathBuf,
    name: Entity<InputState>,
    url: Entity<InputState>,
    sources: Vec<Source>,
    loaded: bool,
    saving: bool,
    active: bool,
    active_source: Option<Source>,
    generation: u64,
    message: String,
    notice: Option<String>,
    data: Option<Arc<okilum_core::log::stream::Snapshot>>,
    view: Option<Entity<reader_log::LogView>>,
    factory: worker::Factory,
    // A mounted scheduler test can hand Start a real worker whose first frame
    // is already queued, with subsequent polling held until Stop.
    #[cfg(test)]
    ready_worker: Option<(worker::Worker, async_channel::Receiver<worker::Frame>)>,
    worker: Option<worker::Worker>,
    events: Option<Task<()>>,
    _load: Task<()>,
    _save: Option<Task<()>>,
}
impl Panel {
    fn new(vault: PathBuf, state: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Source name"));
        let url = cx
            .new(|cx| InputState::new(window, cx).placeholder("https://host/path/application.log"));
        let root = vault.clone();
        let directory = state.clone();
        let load = cx.spawn(async move |this, cx| {
            let sources = cx
                .background_executor()
                .spawn(async move { model::load(&directory, &root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loaded = true;
                match sources {
                    Ok(sources) => this.sources = sources,
                    Err(error) => {
                        this.notice = Some(format!("Cannot load saved sources: {error:#}"))
                    }
                }
                cx.notify();
            });
        });
        Self {
            vault,
            state,
            name,
            url,
            sources: Vec::new(),
            loaded: false,
            saving: false,
            active: false,
            active_source: None,
            generation: 0,
            message: "Choose a source and press Start. Rows stay in this session only.".into(),
            notice: None,
            data: None,
            view: None,
            factory: worker::factory(),
            #[cfg(test)]
            ready_worker: None,
            worker: None,
            events: None,
            _load: load,
            _save: None,
        }
    }
    fn candidate(&self, cx: &App) -> Result<Source> {
        let source = Source {
            name: self.name.read(cx).value().trim().into(),
            url: self.url.read(cx).value().trim().into(),
        };
        source.validate()?;
        Ok(source)
    }
    fn stop(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.events = None;
        self.worker = None;
        self.active = false;
    }
    fn start(&mut self, cx: &mut Context<Self>) {
        let source = match self.candidate(cx) {
            Ok(source) => source,
            Err(error) => {
                self.notice = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.stop();
        self.notice = None;
        self.data = None;
        self.view = None;
        self.active_source = Some(source.clone());
        #[cfg(test)]
        let started = self
            .ready_worker
            .take()
            .map(Ok)
            .unwrap_or_else(|| worker::Worker::start(source, self.factory.clone()));
        #[cfg(not(test))]
        let started = worker::Worker::start(source, self.factory.clone());
        let (worker, events) = match started {
            Ok(pair) => pair,
            Err(error) => {
                self.message = format!("Cannot start: {error}");
                cx.notify();
                return;
            }
        };
        self.worker = Some(worker);
        self.active = true;
        self.message = "Connecting…".into();
        let generation = self.generation;
        self.events = Some(cx.spawn(async move |this, cx| {
            while let Ok(frame) = events.recv().await {
                if this
                    .update(cx, |this, cx| this.receive(generation, frame, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        cx.notify();
    }
    fn receive(&mut self, generation: u64, frame: worker::Frame, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        self.active = frame.active;
        self.message = frame.message;
        if let Some(data) = frame.data {
            if let Some(view) = &self.view {
                view.update(cx, |view, cx| view.replace_stream(data.clone(), cx));
            }
            self.data = Some(data);
        }
        if !self.active {
            self.worker = None;
        }
        cx.notify();
    }
    fn choose(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        self.stop();
        self.notice = None;
        self.data = None;
        self.view = None;
        self.active_source = None;
        self.name
            .update(cx, |i, cx| i.set_value(source.name, window, cx));
        self.url
            .update(cx, |i, cx| i.set_value(source.url, window, cx));
        self.message = "Source selected; press Start to connect.".into();
        cx.notify();
    }
    fn save(&mut self, cx: &mut Context<Self>) {
        let source = match self.candidate(cx) {
            Ok(source) => source,
            Err(error) => {
                self.notice = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let vault = self.vault.clone();
        let state = self.state.clone();
        self.saving = true;
        self._save = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { model::save(&state, &vault, &source) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(sources) => {
                        this.sources = sources;
                        if !this.active {
                            this.notice = Some("Source saved; press Start to connect.".into());
                        }
                    }
                    Err(error) => this.notice = Some(format!("Cannot save source: {error:#}")),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}
impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.view.is_none() {
            if let Some(data) = &self.data {
                self.view =
                    Some(cx.new(|cx| reader_log::LogView::stream(data.clone(), window, cx)));
            }
        }
        let entries = self.sources.clone();
        let entity = cx.entity().downgrade();
        let saved = Button::new("https-log-saved")
            .small()
            .label("Saved sources")
            .disabled(!self.loaded || entries.is_empty())
            .dropdown_menu(move |mut menu, _, _| {
                for source in &entries {
                    let source = source.clone();
                    let entity = entity.clone();
                    let label = source.name.clone();
                    menu = menu.item(gpui_component::menu::PopupMenuItem::new(label).on_click(
                        move |_, window, cx| {
                            let _ = entity
                                .update(cx, |this, cx| this.choose(source.clone(), window, cx));
                        },
                    ));
                }
                menu
            });
        v_flex().size_full().min_h_0()
            .child(TitleBar::new().child("HTTPS log files"))
            .child(v_flex().p_3().gap_2().flex_none()
                .child(h_flex().gap_2().child(div().w(px(180.)).child(Input::new(&self.name).small())).child(div().flex_1().child(Input::new(&self.url).small())).child(saved))
                .child(h_flex().gap_2()
                    .child(Button::new("https-log-save").small().label("Save source").disabled(!self.loaded || self.saving).on_click(cx.listener(|this, _, _, cx| this.save(cx))))
                    .child(Button::new("https-log-start").debug_selector(|| "https-log-start".into()).small().primary().label("Start").disabled(self.active).on_click(cx.listener(|this, _, _, cx| this.start(cx))))
                    .child(Button::new("https-log-stop").debug_selector(|| "https-log-stop".into()).small().label("Stop").disabled(!self.active).on_click(cx.listener(|this, _, _, cx| { this.stop(); this.message = "Stopped; loaded rows retained. An in-flight request may finish within 10 seconds.".into(); cx.notify(); }))))
                .child(div().text_xs().child("Append-only HTTPS files with byte Range support. Credentials/query tokens are not supported. Same-size rewrites cannot be detected."))
                .child(div().debug_selector(|| "https-log-status".into()).text_sm().child(format!("{}{}", self.active_source.as_ref().map(|s| format!("{} · ", s.name)).unwrap_or_default(), self.message))))
            .children(self.notice.as_ref().map(|notice| div().px_3().text_sm().child(notice.clone())))
            .child(div().flex_1().min_h_0().children(self.view.clone()))
    }
}
#[cfg(test)]
#[path = "reader_log_http/tests.rs"]
mod tests;
