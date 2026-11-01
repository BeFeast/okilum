//! Definition-selected snapshots in the existing LogView; no second row renderer.
use super::*;
use gpui::size;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use okilum_core::log::virtual_log::{Candidate, Definition, Reason};
#[path = "reader_log_virtual/model.rs"]
mod model;
#[path = "reader_log_virtual/worker.rs"]
mod worker;

#[derive(Default)]
struct Windows(std::collections::BTreeMap<String, WindowHandle<Root>>);
impl Global for Windows {}
pub(crate) fn show(vault: PathBuf, cx: &mut App) {
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
        let panel = cx.new(|cx| Panel::new(vault, window, cx));
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
struct Panel {
    vault: PathBuf,
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
        if self.active {
            return;
        }
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        self.loading = true;
        self.error = false;
        self.message = "Loading definition…".into();
        let vault = self.vault.clone();
        self.load = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { model::load(&vault, &path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(loaded) => this.pending_load = Some(loaded),
                    Err(error) => {
                        this.error = true;
                        this.message = error.to_string();
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn configure(&mut self, loaded: model::Loaded, window: &mut Window, cx: &mut Context<Self>) {
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
        self.loaded = Some(loaded.path);
        self.time = fields.time;
        self.vault_root = fields.vault_root;
        self.error = false;
        self.message = "Definition loaded. Criteria edits apply to this session; the saved file stays unchanged.".into();
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
    fn start(&mut self, cx: &mut Context<Self>) {
        if self.active || self.loading {
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
        self.run_name = definition.name.clone();
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let (worker, events) =
            worker::Worker::start(self.vault.clone(), path, definition, self.watch);
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
        if self.stopping && !matches!(&frame, worker::Frame::Progress { .. }) {
            self.message = "Stopped; loaded snapshot retained.".into();
            self.active = false;
            self.stopping = false;
            self.worker = None;
            cx.notify();
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
                    if watching {
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
            self.stopping = true;
            self.message = "Stopping after the current bounded selection pass…".into();
            cx.notify();
        }
    }
    fn install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let mut files = pending.files.into_iter();
        let Some(first) = files.next() else {
            self.view = None;
            return;
        };
        let name = self.applied_name.clone();
        self.view = Some(cx.new(|cx| {
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
                let owner = view.source_count();
                view.extra.push(ExtraSource {
                    data: SourceData::File(Arc::new(file)),
                    enabled: true,
                    owner,
                    seq: 0,
                });
            }
            view.rebuild_merge();
            view.refilter(cx);
            view
        }));
    }
}

fn reason(reason: &Reason) -> String {
    match reason {
        Reason::Matched => "Selected",
        Reason::NameMismatch => "Glob does not match",
        Reason::RotationMismatch => "Different rotation family",
        Reason::OutsideRoot => "Outside source root",
        Reason::DuplicateSource => "Already selected by canonical path",
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
        if let Some(loaded) = self.pending_load.take() {
            self.configure(loaded, window, cx);
        }
        self.install(window, cx);
        let disabled = self.active || self.loading;
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
                .child(Button::new("virtual-load").small().label("Load definition").disabled(disabled).on_click(cx.listener(|this, _, _, cx| { let path = PathBuf::from(this.path.read(cx).value().as_str()); this.load_definition(path, cx); })))
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
                .child(Button::new("virtual-watch").small().ghost().label("Watch changes").selected(self.watch).disabled(disabled).tooltip("Keep the snapshot and request an explicit Rescan when files change").on_click(cx.listener(|this, _, _, cx| { this.watch = !this.watch; cx.notify(); })))
                .child(Button::new("virtual-copy").small().ghost().label("Copy definition").disabled(self.loaded.is_none() || disabled).on_click(cx.listener(|this, _, _, cx| {
                    match this.definition(cx).and_then(|definition| Ok(serde_json::to_string_pretty(&definition)?)) {
                        Ok(source) => cx.write_to_clipboard(ClipboardItem::new_string(source)), Err(error) => { this.error = true; this.message = error.to_string(); cx.notify(); }
                    }
                })))
                .child(div().flex_1())
                .child(Button::new("virtual-start").debug_selector(|| "virtual-start".into()).small().primary().label(if self.view.is_some() { "Rescan" } else { "Start" }).disabled(disabled || self.loaded.is_none()).on_click(cx.listener(|this, _, _, cx| this.start(cx))))
                .child(Button::new("virtual-stop").debug_selector(|| "virtual-stop".into()).small().label("Stop").disabled(!self.active || self.stopping).on_click(cx.listener(|this, _, _, cx| this.stop(cx))))
                .child(Button::new("virtual-details").small().ghost().label(format!("Sources ({})", self.candidates.len())).selected(self.details).on_click(cx.listener(|this, _, _, cx| { this.details = !this.details; cx.notify(); }))))
            .child(div().id("virtual-status").max_h(px(64.)).overflow_y_scroll().text_sm().text_color(if self.error { brand::palette(cx).danger } else { cx.theme().muted_foreground }).child(self.message.clone()))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Criteria select whole files; all records of selected files are merged. Session edits do not rewrite the saved definition. Changes require an explicit Rescan; automatic rotation/membership Follow is not enabled."));
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
            .child(TitleBar::new().child("Virtual logs"))
            .child(controls);
        body = if let Some(view) = &self.view {
            body.child(
                div()
                    .text_sm()
                    .px_3()
                    .child(format!("Snapshot · {}", self.applied_name)),
            )
            .child(div().flex_1().min_h_0().child(view.clone()))
        } else {
            body.child(div().flex_1().p_4().child("No selected log rows. Open a saved .okilog definition and Start; source files are read-only."))
        };
        body
    }
}

#[cfg(test)]
#[path = "reader_log_virtual/tests.rs"]
mod tests;
