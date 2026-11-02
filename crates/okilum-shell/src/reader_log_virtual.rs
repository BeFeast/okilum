//! Definition-selected snapshots in the existing LogView; no second row renderer.
use super::*;
use gpui::size;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use okilum_core::log::virtual_log::{Candidate, Definition, Reason};
#[path = "reader_log_virtual/follow.rs"]
mod follow;
#[path = "reader_log_virtual/model.rs"]
mod model;
#[path = "reader_log_virtual/save.rs"]
mod save;
#[path = "reader_log_virtual/worker.rs"]
mod worker;

#[derive(Default)]
struct Windows(std::collections::BTreeMap<String, WindowHandle<Root>>);
impl Global for Windows {}

pub(crate) fn eligible(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "okilog")
}

/// The ordinary file route owns this entity. Replacing/closing the preview drops
/// its selection/watcher worker just as closing the standalone source window does.
pub(crate) fn embedded(
    vault: PathBuf,
    path: PathBuf,
    state: Option<PathBuf>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<Panel> {
    cx.new(|cx| {
        let mut panel = Panel::new(vault, window, cx);
        panel.embedded = true;
        panel.state = state;
        panel.load_definition(path, cx);
        panel
    })
}

pub(crate) fn show(vault: PathBuf, state: Option<PathBuf>, cx: &mut App) {
    let scope = okilum_core::remote_log::vault_scope(&vault);
    if let Some(handle) = cx
        .try_global::<Windows>()
        .and_then(|windows| windows.0.get(&scope))
        .copied()
    {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(window_state::default_bounds(
            size(px(1200.), px(850.)),
            cx,
        ))),
        window_min_size: Some(size(px(800.), px(600.))),
        ..TitleBar::window_options()
    };
    if let Ok(handle) = cx.open_window(options, move |window, cx| {
        window.set_window_title("Virtual logs");
        let panel = cx.new(|cx| {
            let mut panel = Panel::new(vault, window, cx);
            panel.state = state;
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

struct Pending {
    files: Vec<LogFile>,
    elapsed: Duration,
}
pub(crate) struct Panel {
    vault: PathBuf,
    embedded: bool,
    state: Option<PathBuf>,
    session: Option<save::Session>,
    save_error: Option<String>,
    saving: bool,
    save_task: Option<Task<()>>,
    loaded: Option<PathBuf>,
    path: Entity<InputState>,
    name: Entity<InputState>,
    glob: Entity<InputState>,
    query: Entity<InputState>,
    rotation: Entity<InputState>,
    seconds: Entity<InputState>,
    from: Entity<InputState>,
    to: Entity<InputState>,
    time: model::TimeMode,
    vault_root: bool,
    watch: bool,
    following: bool,
    details: bool,
    loading: bool,
    active: bool,
    stopping: bool,
    generation: u64,
    load_generation: u64,
    run_name: String,
    applied_name: String,
    message: String,
    error: bool,
    candidates: Vec<Candidate>,
    pending: Option<Pending>,
    pending_follow: Option<Vec<follow::Source>>,
    pending_path: Option<String>,
    pending_load: Option<model::Loaded>,
    view: Option<Entity<LogView>>,
    worker: Option<worker::Worker>,
    events: Option<Task<()>>,
    load: Option<Task<()>>,
}
impl Panel {
    fn new(vault: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &'static str, cx: &mut Context<Self>, window: &mut Window| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        Self {
            vault,
            embedded: false,
            state: None,
            session: None,
            save_error: None,
            saving: false,
            save_task: None,
            loaded: None,
            path: input("Saved .okilog path, relative to this vault", cx, window),
            name: input("Name", cx, window),
            glob: input("logs/**", cx, window),
            query: input("Optional file-level query", cx, window),
            rotation: input("Optional base log filename", cx, window),
            seconds: input("3600", cx, window),
            from: input("From · RFC3339 with timezone", cx, window),
            to: input("To · RFC3339 with timezone", cx, window),
            time: model::TimeMode::All,
            vault_root: false,
            watch: true,
            following: false,
            details: false,
            loading: false,
            active: false,
            stopping: false,
            generation: 0,
            load_generation: 0,
            run_name: String::new(),
            applied_name: String::new(),
            message: "Open a saved .okilog definition, review its criteria, then Start.".into(),
            error: false,
            candidates: Vec::new(),
            pending: None,
            pending_follow: None,
            pending_path: None,
            pending_load: None,
            view: None,
            worker: None,
            events: None,
            load: None,
        }
    }
    fn choose(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open virtual log definition (.okilog)".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |this, cx| this.load_definition(path, cx));
                }
            }
        })
        .detach();
    }
    fn load_definition(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.active || self.saving || self.loading {
            return;
        }
        // Keep the original editor lease until the replacement parses. Return
        // it on failure so a bad path/definition cannot strand the current draft.
        let previous_session = self.session.take();
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        self.loading = true;
        self.pending_load = None;
        self.pending_path = Some(path.to_string_lossy().into_owned());
        self.error = false;
        self.message = "Loading definition…".into();
        let vault = self.vault.clone();
        let state = self.state.clone();
        self.load = Some(cx.spawn(async move |this, cx| {
            let (result, previous_session) = cx
                .background_executor()
                .spawn(async move {
                    let mut loaded = match model::load(&vault, &path) {
                        Ok(loaded) => loaded,
                        Err(error) => return (Err(error), previous_session),
                    };
                    // Reloading the same file needs its old lease released
                    // before opening the fresh revision-aware Save session.
                    drop(previous_session);
                    match state
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("Reader recovery storage is unavailable"))
                        .and_then(|state| save::Session::open(&loaded, state))
                    {
                        Ok(session) => loaded.session = Some(session),
                        Err(error) => loaded.save_error = Some(format!("{error:#}")),
                    }
                    (Ok(loaded), None)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                match result {
                    Ok(loaded) => this.pending_load = Some(loaded),
                    Err(error) => {
                        this.loading = false;
                        this.session = previous_session;
                        this.error = true;
                        this.message = format!(
                            "Cannot load definition: {error:#}{}",
                            this.loaded
                                .as_ref()
                                .map(|path| format!(
                                    " · Still using {}. Criteria and results retained.",
                                    path.display()
                                ))
                                .unwrap_or_default()
                        );
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn configure(
        &mut self,
        mut loaded: model::Loaded,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = false;
        let fields = model::Fields::from_definition(&loaded.definition);
        for (input, value) in [
            (&self.name, fields.name),
            (&self.glob, fields.glob),
            (&self.query, fields.query),
            (&self.rotation, fields.rotation),
            (&self.seconds, fields.seconds),
            (&self.from, fields.from),
            (&self.to, fields.to),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.path.update(cx, |input, cx| {
            input.set_value(loaded.path.to_string_lossy().into_owned(), window, cx)
        });
        self.session = loaded.session.take();
        self.save_error = loaded.save_error.take();
        self.loaded = Some(loaded.path);
        self.time = fields.time;
        self.vault_root = fields.vault_root;
        self.error = false;
        self.message = "Definition loaded. Start applies session criteria; Save updates this original definition.".into();
    }
    fn definition(&self, cx: &App) -> anyhow::Result<Definition> {
        let text = |input: &Entity<InputState>| input.read(cx).value().to_string();
        model::Fields {
            name: text(&self.name),
            glob: text(&self.glob),
            query: text(&self.query),
            rotation: text(&self.rotation),
            seconds: text(&self.seconds),
            from: text(&self.from),
            to: text(&self.to),
            vault_root: self.vault_root,
            time: self.time,
        }
        .definition()
    }
    pub(crate) fn release_for_source(&mut self, cx: &mut Context<Self>) -> bool {
        if self.loading || self.saving || self.active {
            return false;
        }
        self.session = None;
        self.save_error =
            Some("Source owns editing; return to Read or Load to reopen criteria saving".into());
        cx.notify();
        true
    }
    fn save(&mut self, cx: &mut Context<Self>) {
        if self.active || self.loading || self.saving {
            return;
        }
        let definition = match self.definition(cx) {
            Ok(definition) => definition,
            Err(error) => {
                self.error = true;
                self.message = format!("{error:#}");
                cx.notify();
                return;
            }
        };
        let Some(mut session) = self.session.take() else {
            return;
        };
        let generation = self.load_generation;
        let vault = self.vault.clone();
        self.saving = true;
        self.save_task = Some(cx.spawn(async move |this, cx| {
            let (session, result) = cx.background_executor().spawn(async move {
                let result = session.save(&vault, &definition);
                (session, result)
            }).await;
            let _ = this.update(cx, |this, cx| {
                if this.load_generation != generation { return; }
                this.session = Some(session);
                this.saving = false;
                this.error = !matches!(&result, Ok(save::Outcome::Saved | save::Outcome::Unchanged));
                this.message = match result {
                    Ok(save::Outcome::Saved) => "Original definition saved. Snapshot retained; Start/Rescan is still explicit.".into(),
                    Ok(save::Outcome::Unchanged) => "No criteria changes; original bytes preserved.".into(),
                    Ok(save::Outcome::Conflict) => "Original definition changed externally. Criteria draft retained; review the file and recovery in Source before reloading.".into(),
                    Err(error) => format!("{error:#}. Criteria retained; no automatic retry."),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn start(&mut self, cx: &mut Context<Self>) {
        if self.active || self.loading || self.saving {
            return;
        }
        let Some(path) = self.loaded.clone() else {
            return;
        };
        let definition = match self.definition(cx) {
            Ok(definition) => definition,
            Err(error) => {
                self.error = true;
                self.message = error.to_string();
                cx.notify();
                return;
            }
        };
        // A prior successful batch may be queued for paint when a new run
        // starts. It cannot be applied over the new generation's initial result.
        self.pending = None;
        self.pending_follow = None;
        self.run_name = definition.name.clone();
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let (worker, events) = worker::Worker::start_mode(
            self.vault.clone(),
            path,
            definition,
            self.watch,
            self.following,
        );
        self.worker = Some(worker);
        self.active = true;
        self.stopping = false;
        self.error = false;
        self.message = "Selecting source files…".into();
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
        if self.stopping {
            // Ready/update frames may already be queued when Stop is clicked.
            // Discard all of them until the worker acknowledges a terminal turn;
            // the first queued batch is not proof of worker exit.
            if matches!(
                &frame,
                worker::Frame::Stopped
                    | worker::Frame::Failed(_)
                    | worker::Frame::Invalidated(_, _)
            ) {
                self.message = "Stopped; loaded snapshot retained.".into();
                self.active = false;
                self.stopping = false;
                self.worker = None;
                self.generation = self.generation.wrapping_add(1);
                cx.notify();
            }
            return;
        }
        match frame {
            worker::Frame::Progress { checked, bytes } => {
                if !self.stopping {
                    self.message = format!(
                        "Selecting · {checked} candidates checked · {} bytes read",
                        grouped(bytes)
                    );
                }
            }
            worker::Frame::Ready {
                selection,
                elapsed,
                watching,
            } => {
                let partial = selection.incomplete
                    || selection.candidates.iter().any(|candidate| {
                        matches!(
                            candidate.reason,
                            Reason::SizeLimit
                                | Reason::FileLimit
                                | Reason::IncompleteWindow
                                | Reason::Unreadable(_)
                        )
                    });
                self.message = format!(
                    "{}{} files selected · {} checked · {} bytes read{}",
                    if partial {
                        "Incomplete selection · "
                    } else {
                        ""
                    },
                    selection.files.len(),
                    selection.checked,
                    grouped(selection.bytes_checked),
                    if watching && self.following {
                        " · Following observed appends/rotations; unobserved gaps are not ruled out"
                    } else if watching {
                        " · Watching for source changes"
                    } else {
                        " · Snapshot"
                    }
                );
                self.candidates = selection.candidates;
                self.pending = Some(Pending {
                    files: selection.files,
                    elapsed,
                });
                self.applied_name = self.run_name.clone();
                self.active = watching;
                if !watching {
                    self.worker = None;
                }
            }
            worker::Frame::Updated(update) => {
                let pending = self.pending_follow.get_or_insert_with(Vec::new);
                for source in update.sources {
                    if let Some(old) = pending.iter_mut().find(|old| old.slot == source.slot) {
                        *old = source;
                    } else {
                        pending.push(source);
                    }
                }
                if let Some(candidates) = update.candidates {
                    self.candidates = candidates;
                }
                self.message = update.notice.unwrap_or_else(|| "Following observed appends/rotations · Frozen run criteria · Unobserved gaps are not ruled out".into());
            }
            worker::Frame::Invalidated(message, error) => {
                self.message = message;
                self.error = error;
                self.active = false;
                self.worker = None;
            }
            worker::Frame::Failed(message) => {
                self.message = message;
                self.error = true;
                self.active = false;
                self.worker = None;
            }
            worker::Frame::Stopped => {
                self.message = "Stopped; loaded snapshot retained.".into();
                self.active = false;
                self.worker = None;
            }
        }
        if !self.active {
            self.stopping = false;
        }
        cx.notify();
    }
    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(worker) = &self.worker {
            worker.cancel();
            self.pending = None;
            self.pending_follow = None;
            self.stopping = true;
            self.message = "Stopping after the current bounded selection pass…".into();
            cx.notify();
        }
    }
    fn install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pending) = self.pending.take() {
            let filters = self
                .view
                .as_ref()
                .map(|view| view.read(cx).filter_view_state());
            let mut files = pending.files.into_iter();
            self.view = files.next().map(|first| {
                let name = self.applied_name.clone();
                cx.new(|cx| {
                    let mut view = LogView::new(
                        State::Ready(LogDocument {
                            rel: name,
                            file: Arc::new(first),
                            elapsed: pending.elapsed,
                        }),
                        window,
                        cx,
                    );
                    view.source_locked = true;
                    for file in files {
                        let slot = view.source_count();
                        view.extra.push(ExtraSource {
                            data: SourceData::File(Arc::new(file)),
                            enabled: true,
                            owner: slot,
                            seq: 0,
                        });
                    }
                    view.rebuild_merge();
                    if let Some(filters) = filters {
                        view.restore_filter_view(filters, window, cx);
                    } else {
                        view.refilter(cx);
                    }
                    view
                })
            });
        }
        let Some(mut sources) = self
            .pending_follow
            .take()
            .filter(|sources| !sources.is_empty())
        else {
            return;
        };
        sources.sort_by_key(|source| source.slot);
        if self.view.is_none() {
            let first = sources.remove(0);
            self.view = Some(cx.new(|cx| {
                let mut view = LogView::new(
                    State::Held(Arc::new(HeldSource {
                        path: first.path,
                        snapshot: first.snapshot,
                        label: first.label,
                    })),
                    window,
                    cx,
                );
                view.source_locked = true;
                view
            }));
        }
        self.view.as_ref().unwrap().update(cx, |view, cx| {
            let anchor = view.anchor.and_then(|id| view.locate(id));
            let filtered = view.rows.as_ref().map(|rows| {
                rows.iter()
                    .filter_map(|id| view.locate(*id as usize))
                    .collect::<Vec<_>>()
            });
            for source in sources {
                let held = Arc::new(HeldSource {
                    path: source.path,
                    snapshot: source.snapshot,
                    label: source.label,
                });
                if source.slot == 0 {
                    view.state = State::Held(held);
                } else if let Some(extra) = view.extra.get_mut(source.slot - 1) {
                    extra.data = SourceData::Held(held);
                } else {
                    view.extra.push(ExtraSource {
                        data: SourceData::Held(held),
                        enabled: true,
                        owner: source.slot,
                        seq: 0,
                    });
                }
            }
            view.window_revision = view.window_revision.wrapping_add(1);
            view.histogram_for = None;
            view.rebuild_merge();
            view.anchor = anchor.and_then(|(source, row)| view.id_of(source, row));
            view.rows = filtered.map(|rows| {
                Arc::new(
                    rows.into_iter()
                        .filter_map(|(source, row)| view.id_of(source, row).map(|id| id as u32))
                        .collect(),
                )
            });
            view.refilter(cx);
            cx.notify();
        });
    }
}

fn reason(reason: &Reason) -> String {
    match reason {
        Reason::Matched => "Selected",
        Reason::NameMismatch => "Glob does not match",
        Reason::RotationMismatch => "Different rotation family",
        Reason::OutsideRoot => "Outside source root",
        Reason::DuplicateSource => "Already represented source",
        Reason::NoTimestamps => "No timestamps for this time criterion",
        Reason::OutsideTimeRange => "Outside time window",
        Reason::NoMatchingRecord => "Query did not match this file",
        Reason::SizeLimit => "Input/decoded byte limit",
        Reason::FileLimit => "Selected file limit",
        Reason::IncompleteWindow => "Only a partial file window was available",
        Reason::Unreadable(error) => return format!("Cannot read: {error}"),
    }
    .into()
}

fn field(label: &'static str, input: &Entity<InputState>, disabled: bool) -> Div {
    v_flex()
        .gap_1()
        .child(div().text_xs().child(label))
        .child(Input::new(input).small().disabled(disabled))
}

impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(path) = self.pending_path.take() {
            self.path
                .update(cx, |input, cx| input.set_value(path, window, cx));
        }
        if let Some(loaded) = self.pending_load.take() {
            self.configure(loaded, window, cx);
        }
        self.install(window, cx);
        let disabled = self.active || self.loading || self.saving;
        let owner = cx.entity().downgrade();
        let time = self.time;
        let time_menu = Button::new("virtual-time")
            .small()
            .label(format!("Time: {}", time.label()))
            .disabled(disabled)
            .dropdown_menu(move |mut menu, _, _| {
                for mode in [
                    model::TimeMode::All,
                    model::TimeMode::Last,
                    model::TimeMode::Absolute,
                ] {
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::new(mode.label())
                            .checked(mode == time)
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |this, cx| {
                                    this.time = mode;
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            });
        let mut controls = v_flex().p_3().gap_2().flex_none()
            .child(h_flex().gap_2().child(div().flex_1().min_w_0().child(Input::new(&self.path).small().disabled(disabled)))
                .child(Button::new("virtual-load").debug_selector(|| "virtual-load".into()).small().label("Load definition").disabled(disabled).on_click(cx.listener(|this, _, _, cx| { let path = PathBuf::from(this.path.read(cx).value().as_str()); this.load_definition(path, cx); })))
                .child(Button::new("virtual-browse").small().label("Browse…").disabled(disabled).on_click(cx.listener(|this, _, _, cx| this.choose(cx)))))
            .child(h_flex().gap_2()
                .child(div().w(px(220.)).child(field("Name", &self.name, disabled || self.loaded.is_none())))
                .child(div().flex_1().min_w_0().child(field("File glob", &self.glob, disabled || self.loaded.is_none())))
                .child(Button::new("virtual-root").small().ghost().label(if self.vault_root { "Vault root" } else { "Definition folder" }).disabled(disabled || self.loaded.is_none()).tooltip("Root used for the relative glob").on_click(cx.listener(|this, _, _, cx| { this.vault_root = !this.vault_root; cx.notify(); }))))
            .child(h_flex().gap_2().child(div().flex_1().min_w_0().child(field("File-level query", &self.query, disabled || self.loaded.is_none())))
                .child(div().w(px(220.)).child(field("Rotation family", &self.rotation, disabled || self.loaded.is_none()))))
            .child(h_flex().gap_2().items_center().child(time_menu)
                .when(self.time == model::TimeMode::Last, |row| row.child(div().w(px(140.)).child(Input::new(&self.seconds).small().disabled(disabled))))
                .when(self.time == model::TimeMode::Absolute, |row| row.child(div().flex_1().child(Input::new(&self.from).small().disabled(disabled))).child(div().flex_1().child(Input::new(&self.to).small().disabled(disabled)))))
            .child(h_flex().gap_2().items_center()
                .child(Button::new("virtual-watch").small().ghost().label("Watch changes").selected(self.watch).disabled(disabled).tooltip("Keep the snapshot and request an explicit Rescan when files change").on_click(cx.listener(|this, _, _, cx| { this.watch = !this.watch; if this.watch { this.following = false; } cx.notify(); })))
                .child(Button::new("virtual-follow").debug_selector(|| "virtual-follow".into()).small().ghost().label("Follow").selected(self.following).disabled(disabled).tooltip("Opt in, then Start. Preserve observed uncompressed rotation/truncation segments and marks; wait on missing paths. Ambiguous identities, record-window changes or lost coverage pause with history retained. Gap-free continuity is not established.").on_click(cx.listener(|this, _, _, cx| { this.following = !this.following; if this.following { this.watch = false; } cx.notify(); })))
                .child(Button::new("virtual-copy").small().ghost().label("Copy definition").disabled(self.loaded.is_none() || disabled).on_click(cx.listener(|this, _, _, cx| {
                    match this.definition(cx).and_then(|definition| Ok(serde_json::to_string_pretty(&definition)?)) {
                        Ok(source) => cx.write_to_clipboard(ClipboardItem::new_string(source)), Err(error) => { this.error = true; this.message = error.to_string(); cx.notify(); }
                    }
                })))
                .child(Button::new("virtual-save").debug_selector(|| "virtual-save".into()).small().label(if self.saving { "Saving…" } else { "Save" }).disabled(disabled || self.session.is_none()).tooltip("Save criteria to the original .okilog with conflict checks and History; does not rescan").on_click(cx.listener(|this, _, _, cx| this.save(cx))))
                .child(div().flex_1())
                .child(Button::new("virtual-start").debug_selector(|| "virtual-start".into()).small().primary().label(if self.view.is_some() { "Rescan" } else { "Start" }).disabled(disabled || self.loaded.is_none()).on_click(cx.listener(|this, _, _, cx| this.start(cx))))
                .child(Button::new("virtual-stop").debug_selector(|| "virtual-stop".into()).small().label("Stop").disabled(!self.active || self.stopping).on_click(cx.listener(|this, _, _, cx| this.stop(cx))))
                .child(Button::new("virtual-details").small().ghost().label(format!("Sources ({})", self.candidates.len())).selected(self.details).on_click(cx.listener(|this, _, _, cx| { this.details = !this.details; cx.notify(); }))))
            .child(div().id("virtual-status").max_h(px(64.)).overflow_y_scroll().text_sm().text_color(if self.error { brand::palette(cx).danger } else { cx.theme().muted_foreground }).child(self.message.clone()))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Criteria select whole files; all records of selected files are merged. Save explicitly updates the original definition with revision checks and History. Changes require an explicit Rescan; Follow is explicit opt-in. Observed rotation/truncation history is retained; unobserved gaps are not ruled out. Ambiguity, changed record windows or lost coverage require explicit Rescan."));
        if let Some(error) = &self.save_error {
            controls = controls.child(div().text_sm().text_color(brand::palette(cx).danger).child(
                format!("Save unavailable: {error}. Session Start remains available. To review Source, use the original document in Reader (close a standalone Virtual logs window first)."),
            ));
        }
        if self.details {
            controls = controls.child(
                div()
                    .h(px(ROW_HEIGHT * self.candidates.len().min(6) as f32))
                    .child(
                        uniform_list(
                            "virtual-source-reasons",
                            self.candidates.len(),
                            cx.processor(|this, range: Range<usize>, _, _| {
                                range
                                    .map(|i| {
                                        let candidate = &this.candidates[i];
                                        let path = candidate
                                            .path
                                            .strip_prefix(&this.vault)
                                            .unwrap_or(&candidate.path)
                                            .to_string_lossy()
                                            .into_owned();
                                        let message = reason(&candidate.reason);
                                        let tooltip =
                                            format!("{} · {}", candidate.path.display(), message);
                                        h_flex()
                                            .id(("virtual-candidate", i))
                                            .h(px(ROW_HEIGHT))
                                            .gap_2()
                                            .items_center()
                                            .text_sm()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_ellipsis()
                                                    .child(path),
                                            )
                                            .child(
                                                div()
                                                    .w(px(320.))
                                                    .min_w_0()
                                                    .text_ellipsis()
                                                    .child(message),
                                            )
                                            .tooltip(move |window, cx| {
                                                gpui_component::tooltip::Tooltip::new(
                                                    tooltip.clone(),
                                                )
                                                .build(window, cx)
                                            })
                                            .into_any_element()
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .size_full(),
                    ),
            );
        }
        let mut body = v_flex()
            .size_full()
            .min_h_0()
            .debug_selector(|| "virtual-definition-view".into())
            .when(!self.embedded, |body| {
                body.child(TitleBar::new().child("Virtual logs"))
            })
            .child(controls);
        body = if let Some(view) = &self.view {
            body.child(div().text_sm().px_3().child(format!(
                "{} · {}",
                if self.following {
                    "Observed log"
                } else {
                    "Snapshot"
                },
                self.applied_name
            )))
            .child(div().flex_1().min_h_0().child(view.clone()))
        } else {
            body.child(div().flex_1().p_4().child(if self.active && self.following { "No matching sources yet. Follow is waiting for files that meet the frozen criteria." } else { "No selected log rows. Open a saved .okilog definition and Start; source files are read-only." }))
        };
        body
    }
}

#[cfg(test)]
#[path = "reader_log_virtual/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "reader_log_virtual/load_tests.rs"]
mod load_tests;
