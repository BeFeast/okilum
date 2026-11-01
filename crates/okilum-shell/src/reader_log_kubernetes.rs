//! Explicit read-only Kubernetes target, rendered by the existing LogView.
use crate::*;
use okilum_core::log::kubernetes::Request;

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
            size(px(1100.), px(760.)),
            cx,
        ))),
        window_min_size: Some(size(px(720.), px(480.))),
        ..TitleBar::window_options()
    };
    if let Ok(handle) = cx.open_window(options, move |window, cx| {
        window.set_window_title("Kubernetes logs");
        let panel = cx.new(|cx| Panel::new(scope, window, cx));
        cx.new(|cx| Root::new(panel, window, cx))
    }) {
        if cx.try_global::<Windows>().is_none() {
            cx.set_global(Windows::default());
        }
        let scope = okilum_core::remote_log::vault_scope(&vault);
        cx.global_mut::<Windows>().0.insert(scope, handle);
    }
}

struct Panel {
    scope: String,
    context: Entity<InputState>,
    namespace: Entity<InputState>,
    pod: Entity<InputState>,
    container: Entity<InputState>,
    tail: Entity<InputState>,
    previous: bool,
    follow: bool,
    generation: u64,
    running: bool,
    stopping: bool,
    message: String,
    diagnostic: String,
    error: Option<String>,
    view: Option<Entity<reader_log::LogView>>,
    pending: Option<Arc<okilum_core::log::stream::Snapshot>>,
    worker: Option<worker::Worker>,
    events: Option<Task<()>>,
}
impl Panel {
    fn new(scope: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &'static str, cx: &mut Context<Self>, window: &mut Window| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let tail = input("Initial lines (0–100000)", cx, window);
        tail.update(cx, |input, cx| input.set_value("1000", window, cx));
        Self {
            scope,
            context: input("Current context (or exact context name)", cx, window),
            namespace: input("Namespace", cx, window),
            pod: input("Pod", cx, window),
            container: input("Container", cx, window),
            tail,
            previous: false,
            follow: true,
            generation: 0,
            running: false,
            stopping: false,
            message:
                "Choose an explicit target, then Start. Uses your existing kubectl configuration."
                    .into(),
            diagnostic: String::new(),
            error: None,
            view: None,
            pending: None,
            worker: None,
            events: None,
        }
    }
    fn request(&self, cx: &App) -> anyhow::Result<Request> {
        let context = self.context.read(cx).value().trim().to_owned();
        let request = Request {
            context: (!context.is_empty()).then_some(context),
            namespace: self.namespace.read(cx).value().trim().to_owned(),
            pod: self.pod.read(cx).value().trim().to_owned(),
            container: self.container.read(cx).value().trim().to_owned(),
            previous: self.previous,
            follow: self.follow,
            tail_lines: self.tail.read(cx).value().trim().parse().map_err(|_| {
                anyhow::anyhow!("Initial lines must be a whole number between 0 and 100000")
            })?,
        };
        request.args()?;
        Ok(request)
    }
    fn start(&mut self, cx: &mut Context<Self>) {
        let request = match self.request(cx) {
            Ok(request) => request,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let scope = format!("kubernetes:{}:{generation}", self.scope);
        let (worker, receiver) = worker::Worker::start(request, scope);
        self.worker = Some(worker);
        self.running = true;
        self.stopping = false;
        self.message = "Reading Kubernetes logs…".into();
        self.error = None;
        self.diagnostic.clear();
        self.view = None;
        self.pending = None;
        self.events = Some(cx.spawn(async move |this, cx| {
            while let Ok(update) = receiver.recv().await {
                if this
                    .update(cx, |this, cx| {
                        this.receive(generation, update, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        cx.notify();
    }
    fn receive(&mut self, generation: u64, update: worker::Update, cx: &mut Context<Self>) {
        if self.generation != generation {
            return;
        }
        if let Some(snapshot) = update.snapshot {
            self.pending = Some(snapshot);
        }
        self.diagnostic = update.diagnostic;
        let suffix = if update.pending == 0 {
            String::new()
        } else {
            format!(" · {} trailing bytes waiting for a newline", update.pending)
        };
        self.message = match update.status {
            worker::Status::Reading => format!("Reading Kubernetes logs…{suffix}"),
            worker::Status::Complete => format!("Completed{suffix}"),
            worker::Status::Stopped => format!("Stopped{suffix}"),
            worker::Status::Failed(ref error) => {
                self.error = Some(error.clone());
                format!("Stopped; loaded rows retained{suffix}")
            }
        };
        if update.status != worker::Status::Reading {
            self.running = false;
            self.stopping = false;
            self.worker = None;
        }
        cx.notify();
    }
    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(worker) = &self.worker {
            worker.cancel();
            self.stopping = true;
            self.message = "Stopping Kubernetes log session…".into();
            cx.notify();
        }
    }
}

impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(snapshot) = self.pending.take() {
            if let Some(view) = &self.view {
                view.update(cx, |view, cx| view.replace_stream(snapshot, cx));
            } else {
                self.view = Some(cx.new(|cx| reader_log::LogView::stream(snapshot, window, cx)));
            }
        }
        let controls = v_flex()
            .p_3()
            .gap_2()
            .flex_none()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_xs().child("Context"))
                            .child(Input::new(&self.context).small().disabled(self.running)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .w(px(180.))
                            .child(div().text_xs().child("Namespace"))
                            .child(Input::new(&self.namespace).small().disabled(self.running)),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_xs().child("Pod"))
                            .child(Input::new(&self.pod).small().disabled(self.running)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_xs().child("Container"))
                            .child(Input::new(&self.container).small().disabled(self.running)),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(px(180.))
                            .child(Input::new(&self.tail).small().disabled(self.running)),
                    )
                    .child(
                        Button::new("kubernetes-follow")
                            .small()
                            .ghost()
                            .label("Follow")
                            .selected(self.follow)
                            .disabled(self.running)
                            .tooltip("Continue reading new log lines after initial history")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.follow = !this.follow;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("kubernetes-previous")
                            .small()
                            .ghost()
                            .label("Previous instance")
                            .selected(self.previous)
                            .disabled(self.running)
                            .tooltip("Read logs of the previous terminated container instance")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.previous = !this.previous;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("kubernetes-start")
                            .debug_selector(|| "kubernetes-start".into())
                            .small()
                            .primary()
                            .label("Start")
                            .disabled(self.running)
                            .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                    )
                    .child(
                        Button::new("kubernetes-stop")
                            .debug_selector(|| "kubernetes-stop".into())
                            .small()
                            .label("Stop")
                            .disabled(!self.running || self.stopping)
                            .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                    ),
            )
            .child(div().text_sm().child(self.message.clone()))
            .children(self.error.as_ref().map(|error| {
                div()
                    .text_sm()
                    .text_color(brand::palette(cx).danger)
                    .child(error.clone())
            }))
            .when(!self.diagnostic.is_empty(), |body| {
                body.child(
                    div()
                        .id("kubernetes-diagnostic")
                        .max_h(px(80.))
                        .overflow_y_scroll()
                        .text_sm()
                        .child(self.diagnostic.clone()),
                )
            });
        let mut body = v_flex()
            .size_full()
            .min_h_0()
            .child(TitleBar::new().child("Kubernetes logs"))
            .child(controls);
        body = if let Some(view) = &self.view {
            body.child(div().flex_1().min_h_0().child(view.clone()))
        } else {
            body.child(div().flex_1().p_4().text_color(cx.theme().muted_foreground).child("No log rows loaded. Start reads only the selected target; closing this window stops its kubectl process. Loaded rows remain session-only."))
        };
        body
    }
}

#[cfg(test)]
#[path = "reader_log_kubernetes/tests.rs"]
mod tests;
