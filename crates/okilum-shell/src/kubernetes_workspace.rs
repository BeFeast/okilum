//! Native read-only Kubernetes Workspace (#3010, epic #3008): an explicit
//! context/namespace strip, a searchable resource list, an inspector
//! (Overview, Events, Manifest) and the existing LogView docked below.
//!
//! Three things stay independent (docs/kubernetes-workspace.md): the selected
//! resource, the pinned log session and the log viewport. Selecting rows or
//! changing scope never starts, stops or retargets logs; only Open logs does,
//! after confirming a retarget. Nothing reads the cluster until Load.
use crate::*;
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectState};
use okilum_core::kubernetes::{choose_namespace, Contexts, Failure, NamespaceOrigin};

mod backend;
mod dock;
mod inspector;
pub(crate) mod model;

use model::{Data, Key, Row, Scope, Section, Tone};

const CONTEXT: &str = "KubernetesWorkspaceList";

actions!(
    kubernetes_workspace,
    [
        SelectNextResource,
        SelectPreviousResource,
        OpenSelectedLogs,
        FocusResourceFilter
    ]
);

/// Supplementary keys, only while the resource list has focus: inputs and
/// the LogView keep their own bindings.
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("j", SelectNextResource, Some(CONTEXT)),
        KeyBinding::new("down", SelectNextResource, Some(CONTEXT)),
        KeyBinding::new("k", SelectPreviousResource, Some(CONTEXT)),
        KeyBinding::new("up", SelectPreviousResource, Some(CONTEXT)),
        KeyBinding::new("l", OpenSelectedLogs, Some(CONTEXT)),
        KeyBinding::new("/", FocusResourceFilter, Some(CONTEXT)),
    ]);
}

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
            size(px(1320.), px(900.)),
            cx,
        ))),
        window_min_size: Some(size(px(860.), px(600.))),
        ..TitleBar::window_options()
    };
    let key = scope.clone();
    if let Ok(handle) = cx.open_window(options, move |window, cx| {
        window.set_window_title("Kubernetes workspace");
        let reader = backend::production();
        let workspace = cx.new(|cx| Workspace::new(key, reader, window, cx));
        cx.new(|cx| Root::new(workspace, window, cx))
    }) {
        if cx.try_global::<Windows>().is_none() {
            cx.set_global(Windows::default());
        }
        cx.global_mut::<Windows>().0.insert(scope, handle);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    Overview,
    Events,
    Manifest,
}

pub(crate) struct Workspace {
    vault_scope: String,
    reader: Arc<dyn backend::Reader>,
    contexts: Option<Result<Contexts, Failure>>,
    context_select: Option<Entity<SelectState<SearchableVec<SharedString>>>>,
    chosen_context: Option<String>,
    namespace: Entity<InputState>,
    filter: Entity<InputState>,
    data: Option<Data>,
    /// The typed namespace and how it was confirmed, for the scope strip.
    namespace_note: Option<String>,
    /// A failure that blocks reading at all (auth, kubeconfig, MFA).
    notice: Option<String>,
    loading: Option<okilum_core::kubernetes::Cancel>,
    load_generation: u64,
    section: Section,
    selected: Option<Key>,
    tab: Tab,
    rows: Rc<Vec<Row>>,
    list_scroll: UniformListScrollHandle,
    list_focus: FocusHandle,
    dock: dock::Dock,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub(crate) fn new(
        vault_scope: String,
        reader: Arc<dyn backend::Reader>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let namespace = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Namespace (empty: the context's default)")
        });
        let filter = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Filter by name, status, reason or image…")
        });
        let subscriptions = vec![
            cx.subscribe(&filter, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.refresh_rows(cx);
                }
            }),
            cx.subscribe(&namespace, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.load(cx);
                }
            }),
        ];
        let receiver = backend::contexts(reader.clone());
        let executor = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            if let Some(result) = backend::next_batch(&receiver, &executor)
                .await
                .and_then(|batch| batch.into_iter().next())
            {
                let _ = this.update(cx, |this, cx| {
                    this.chosen_context = result.as_ref().ok().and_then(|c| c.current.clone());
                    this.contexts = Some(result);
                    cx.notify();
                });
            }
        });
        Self {
            vault_scope,
            reader,
            contexts: None,
            context_select: None,
            chosen_context: None,
            namespace,
            filter,
            data: None,
            namespace_note: None,
            notice: None,
            loading: None,
            load_generation: 0,
            section: Section::Pods,
            selected: None,
            tab: Tab::Overview,
            rows: Rc::new(Vec::new()),
            list_scroll: UniformListScrollHandle::new(),
            list_focus: cx.focus_handle(),
            dock: dock::Dock::default(),
            _tasks: vec![task],
            _subscriptions: subscriptions,
        }
    }

    // ---------- Scope ----------

    fn ensure_context_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_select.is_some() {
            return;
        }
        let Some(Ok(contexts)) = &self.contexts else {
            return;
        };
        let names: Vec<SharedString> = contexts
            .contexts
            .iter()
            .map(|c| SharedString::from(c.name.clone()))
            .collect();
        let index = self
            .chosen_context
            .as_ref()
            .and_then(|chosen| names.iter().position(|n| n.as_ref() == chosen.as_str()))
            .map(|row| gpui_component::IndexPath::default().row(row));
        let select = cx.new(|cx| {
            SelectState::new(SearchableVec::new(names), index, window, cx).searchable(true)
        });
        self._subscriptions.push(cx.subscribe(
            &select,
            |this, _, event: &SelectEvent<SearchableVec<SharedString>>, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    // Choosing a context changes nothing until Load.
                    this.chosen_context = Some(value.to_string());
                    cx.notify();
                }
            },
        ));
        self.context_select = Some(select);
    }

    /// Resolves the explicit scope and reads it. A scope change replaces the
    /// shown data only; the pinned log session is untouched.
    pub(crate) fn load(&mut self, cx: &mut Context<Self>) {
        let contexts = match &self.contexts {
            Some(Ok(contexts)) => contexts,
            Some(Err(failure)) => {
                self.notice = Some(failure.summary());
                cx.notify();
                return;
            }
            None => return,
        };
        let connection = match contexts.resolve(self.chosen_context.as_deref()) {
            Ok(connection) => connection,
            Err(failure) => {
                self.notice = Some(failure.summary());
                cx.notify();
                return;
            }
        };
        let typed = self.namespace.read(cx).value().trim().to_string();
        let choice = match choose_namespace(&connection, Some(typed.as_str()), None) {
            Ok(choice) => choice,
            Err(failure) => {
                self.notice = Some(failure.summary());
                cx.notify();
                return;
            }
        };
        let scope = Scope {
            connection,
            namespace: choice.namespace,
        };
        self.namespace_note = Some(namespace_note(&choice.origin));
        self.notice = None;
        if self.data.as_ref().map(|d| &d.scope) != Some(&scope) {
            self.data = Some(Data::new(scope.clone()));
            self.selected = None;
        }
        if let Some(cancel) = self.loading.take() {
            cancel.cancel();
        }
        let Some(data) = self.data.as_ref() else {
            return;
        };
        let tickets = backend::Tickets::take(data);
        let cancel = okilum_core::kubernetes::Cancel::new();
        self.loading = Some(cancel.clone());
        self.load_generation += 1;
        let generation = self.load_generation;
        let receiver = backend::load(
            self.reader.clone(),
            scope,
            tickets,
            cancel,
            !typed.is_empty(),
        );
        let executor = cx.background_executor().clone();
        self._tasks.push(cx.spawn(async move |this, cx| {
            while let Some(batch) = backend::next_batch(&receiver, &executor).await {
                let applied = this.update(cx, |this, cx| {
                    for loaded in batch {
                        this.loaded(generation, typed.clone(), loaded, cx);
                    }
                });
                if applied.is_err() {
                    break;
                }
            }
        }));
        self.refresh_rows(cx);
    }

    fn loaded(
        &mut self,
        generation: u64,
        typed: String,
        loaded: backend::Loaded,
        cx: &mut Context<Self>,
    ) {
        if generation != self.load_generation {
            return;
        }
        let Some(data) = self.data.as_mut() else {
            return;
        };
        match loaded {
            backend::Loaded::Namespaces(result) => {
                if let Ok(choice) =
                    choose_namespace(&data.scope.connection, Some(typed.as_str()), Some(&result))
                {
                    self.namespace_note = Some(namespace_note(&choice.origin));
                }
            }
            backend::Loaded::Done => self.loading = None,
            other => {
                if let Some(blocking) = blocking_failure(&other) {
                    self.notice = Some(blocking);
                }
                other.apply(data);
            }
        }
        self.refresh_rows(cx);
    }

    fn stop_loading(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.loading.take() {
            cancel.cancel();
            cx.notify();
        }
    }

    // ---------- List and selection ----------

    fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        let query = self.filter.read(cx).value().to_string();
        self.rows = Rc::new(match &self.data {
            Some(data) => model::rows(data, self.section, &query),
            None => Vec::new(),
        });
        cx.notify();
    }

    pub(crate) fn set_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        self.refresh_rows(cx);
    }

    /// Selection only changes what the inspector shows. It never reads the
    /// cluster and never touches the pinned log session.
    pub(crate) fn select(&mut self, key: Key, cx: &mut Context<Self>) {
        if key.section() != self.section {
            self.section = key.section();
            self.refresh_rows(cx);
        }
        if let Some(ix) = self.rows.iter().position(|r| r.key == key) {
            self.list_scroll
                .scroll_to_item(ix, gpui::ScrollStrategy::Nearest);
        }
        self.selected = Some(key);
        cx.notify();
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|key| self.rows.iter().position(|r| &r.key == key));
        let next = match current {
            None => 0,
            Some(ix) => (ix as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize,
        };
        let key = self.rows[next].key.clone();
        self.select(key, cx);
    }

    /// The `l` key: the selected container's current logs, or a Pod's only
    /// regular container. Goes through the same explicit Open logs path.
    fn open_selected(&mut self, cx: &mut Context<Self>) {
        let Some(data) = &self.data else {
            return;
        };
        let Some(key) = &self.selected else {
            return;
        };
        let target = match model::resolve(data, key) {
            model::Resolved::Container(pod, c) => {
                model::LogTarget::new(&data.scope, pod, &c.name, false)
            }
            model::Resolved::Pod(pod) => {
                let regular: Vec<_> = pod
                    .containers
                    .iter()
                    .filter(|c| c.kind == okilum_core::kubernetes::ContainerKind::Regular)
                    .collect();
                match regular.as_slice() {
                    [only] => model::LogTarget::new(&data.scope, pod, &only.name, false),
                    _ => Err("Choose a container in the inspector to open its logs.".into()),
                }
            }
            _ => return,
        };
        match target {
            Ok(target) => self.request_logs(target, cx),
            Err(error) => {
                self.dock.message = Some(error);
                cx.notify();
            }
        }
    }

    fn render_scope(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_context_select(window, cx);
        let tokens = brand::palette(cx);
        let caps = |text: &'static str| {
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(tokens.text_muted)
                .child(text)
        };
        let context: AnyElement = match (&self.contexts, &self.context_select) {
            (_, Some(select)) => div()
                .debug_selector(|| "k8s-context".into())
                .child(
                    Select::new(select)
                        .small()
                        .w(px(240.))
                        .menu_width(px(360.))
                        .menu_max_h(px(320.))
                        .accessibility_label("Kubernetes context"),
                )
                .into_any_element(),
            (None, _) => div()
                .text_sm()
                .text_color(tokens.text_muted)
                .child("Reading kubeconfig contexts…")
                .into_any_element(),
            (Some(Err(failure)), _) => div()
                .text_sm()
                .text_color(tokens.danger)
                .child(failure.summary())
                .into_any_element(),
            (Some(Ok(_)), None) => div().into_any_element(),
        };
        let loaded_scope = self.data.as_ref().map(|d| d.scope.clone());
        let server = loaded_scope
            .as_ref()
            .and_then(|s| s.connection.server.clone())
            .or_else(|| {
                let chosen = self.chosen_context.as_ref()?;
                let Some(Ok(contexts)) = &self.contexts else {
                    return None;
                };
                contexts
                    .contexts
                    .iter()
                    .find(|c| &c.name == chosen)
                    .and_then(|c| c.server.clone())
            });
        h_flex()
            .debug_selector(|| "k8s-scope".into())
            .flex_none()
            .flex_wrap()
            .gap_3()
            .px_4()
            .py_2()
            .items_center()
            .border_b_1()
            .border_color(tokens.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(caps("CONTEXT"))
                    .child(context),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(caps("NAMESPACE"))
                    .child(div().w(px(240.)).child(Input::new(&self.namespace).small())),
            )
            .child(
                Button::new("k8s-load")
                    .debug_selector(|| "k8s-load".into())
                    .small()
                    .primary()
                    .label(if self.data.is_some() {
                        "Refresh"
                    } else {
                        "Load"
                    })
                    .disabled(!matches!(self.contexts, Some(Ok(_))))
                    .tooltip("Read this context and namespace (one bounded kubectl get per kind)")
                    .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
            )
            .when(self.loading.is_some(), |row| {
                row.child(
                    Button::new("k8s-load-stop")
                        .small()
                        .ghost()
                        .label("Stop reading")
                        .on_click(cx.listener(|this, _, _, cx| this.stop_loading(cx))),
                )
            })
            .children(server.map(|server| {
                div()
                    .text_xs()
                    .font_family(brand::MONO_FONT)
                    .text_color(tokens.text_muted)
                    .child(server)
            }))
            .child(div().flex_1())
            .child(
                div()
                    .px_2()
                    .py(px(2.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(tokens.border)
                    .text_xs()
                    .text_color(tokens.text_muted)
                    .child("Read-only"),
            )
            .into_any_element()
    }

    fn render_banner(&self, cx: &App) -> Option<AnyElement> {
        let tokens = brand::palette(cx);
        let mut lines = Vec::new();
        if let Some(notice) = &self.notice {
            lines.push((notice.clone(), tokens.danger));
        }
        if let Some(note) = &self.namespace_note {
            lines.push((note.clone(), tokens.text_muted));
        }
        if lines.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .debug_selector(|| "k8s-banner".into())
                .flex_none()
                .px_4()
                .py_1()
                .gap_1()
                .border_b_1()
                .border_color(tokens.border_subtle)
                .children(
                    lines
                        .into_iter()
                        .map(|(text, color)| div().text_xs().text_color(color).child(text)),
                )
                .into_any_element(),
        )
    }

    fn render_filterbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = brand::palette(cx);
        let counts = |section: Section| -> Option<usize> {
            let data = self.data.as_ref()?;
            Some(model::rows(data, section, "").len())
        };
        h_flex()
            .flex_none()
            .gap_2()
            .px_4()
            .py_2()
            .items_center()
            .border_b_1()
            .border_color(tokens.border)
            .child(
                div().flex_1().min_w(px(180.)).child(
                    Input::new(&self.filter)
                        .small()
                        .prefix(Icon::new(IconName::Search).small()),
                ),
            )
            .child(
                h_flex()
                    .debug_selector(|| "k8s-sections".into())
                    .flex_none()
                    .p(px(2.))
                    .rounded(px(6.))
                    .bg(tokens.border_subtle.opacity(0.5))
                    .children(Section::ALL.map(|section| {
                        let label = match counts(section) {
                            Some(n) => format!("{} {n}", section.label()),
                            None => section.label().to_string(),
                        };
                        Button::new(SharedString::from(format!(
                            "k8s-section-{}",
                            section.label()
                        )))
                        .debug_selector(move || format!("k8s-section-{}", section.label()))
                        .small()
                        .ghost()
                        .label(label)
                        .selected(self.section == section)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_section(section, cx)))
                    })),
            )
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = brand::palette(cx);
        let header = h_flex()
            .flex_none()
            .px_3()
            .py_1()
            .justify_between()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(tokens.text_muted)
            .border_b_1()
            .border_color(tokens.border_subtle)
            .child(self.section.label().to_uppercase())
            .child(format!("{}", self.rows.len()));
        let body: AnyElement = if self.rows.is_empty() {
            let text = match &self.data {
                None => "Choose a context and namespace, then Load. Nothing reads the cluster before that.".to_string(),
                Some(data) => {
                    let what = self.section.label();
                    match self.section {
                        Section::Pods | Section::Containers => {
                            model::empty_reason(&data.pods, "Pods", &data.scope)
                        }
                        Section::Deployments => {
                            model::empty_reason(&data.deployments, what, &data.scope)
                        }
                        Section::Services => {
                            model::empty_reason(&data.services, what, &data.scope)
                        }
                    }
                }
            };
            div()
                .debug_selector(|| "k8s-list-empty".into())
                .p_4()
                .text_sm()
                .text_color(tokens.text_muted)
                .child(text)
                .into_any_element()
        } else {
            let rows = self.rows.clone();
            uniform_list(
                "k8s-resources",
                rows.len(),
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|ix| this.render_row(ix, &rows[ix], cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.list_scroll)
            .size_full()
            .into_any_element()
        };
        v_flex()
            .debug_selector(|| "k8s-list".into())
            .id("k8s-list")
            .key_context(CONTEXT)
            .track_focus(&self.list_focus)
            .on_action(cx.listener(|this, _: &SelectNextResource, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPreviousResource, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &OpenSelectedLogs, _, cx| this.open_selected(cx)))
            .on_action(cx.listener(|this, _: &FocusResourceFilter, window, cx| {
                this.filter.update(cx, |input, cx| input.focus(window, cx));
            }))
            .w(relative(0.42))
            .min_w(px(300.))
            .h_full()
            .min_h_0()
            .border_r_1()
            .border_color(tokens.border)
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    fn render_row(&self, ix: usize, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        let tokens = brand::palette(cx);
        let selected = self.selected.as_ref() == Some(&row.key);
        let key = row.key.clone();
        let muted = tokens.text_muted;
        let mut facts = Vec::new();
        if let Some(ready) = &row.ready {
            facts.push(format!("Ready {ready}"));
        }
        if let Some(restarts) = row.restarts {
            facts.push(format!("Restarts {restarts}"));
        }
        h_flex()
            .id(("k8s-row", ix))
            .debug_selector(move || format!("k8s-row-{ix}"))
            .px_3()
            .py(px(5.))
            .gap_2()
            .items_start()
            .border_b_1()
            .border_color(tokens.border_subtle)
            .when(selected, |row| row.bg(tokens.selected))
            .hover(|style| style.bg(tokens.border_subtle.opacity(0.4)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.list_focus.focus(window, cx);
                this.select(key.clone(), cx);
            }))
            .child(tone_dot(row.tone, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(1.))
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_between()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(brand::MONO_FONT)
                                    .text_sm()
                                    .child(row.title.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(tone_color(row.tone, cx))
                                    .child(row.status.clone()),
                            ),
                    )
                    .when(!row.subtitle.is_empty(), |col| {
                        col.child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(row.subtitle.clone()),
                        )
                    })
                    .when(!facts.is_empty() || !row.reasons.is_empty(), |col| {
                        col.child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .text_xs()
                                .children(facts.into_iter().map(|f| div().child(f)))
                                .children(row.reasons.iter().map(|reason| {
                                    div()
                                        .px_1()
                                        .rounded(px(4.))
                                        .bg(tokens.border_subtle.opacity(0.6))
                                        .child(reason.clone())
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_status(&self, cx: &App) -> AnyElement {
        let tokens = brand::palette(cx);
        let now = std::time::SystemTime::now();
        let mut parts: Vec<(String, Tone)> = vec![("Read-only workspace".into(), Tone::Good)];
        match &self.data {
            Some(data) => {
                parts.push((data.scope.label(), Tone::Neutral));
                for (name, (text, tone)) in [
                    ("Pods", model::freshness(&data.pods, now)),
                    ("Workloads", model::freshness(&data.deployments, now)),
                    ("Services", model::freshness(&data.services, now)),
                    ("Endpoints", model::freshness(&data.slices, now)),
                    ("Events", model::freshness(&data.events, now)),
                    ("HPA", model::freshness(&data.hpas, now)),
                ] {
                    parts.push((format!("{name}: {text}"), tone));
                }
                if self.loading.is_some() {
                    parts.push(("reading…".into(), Tone::Neutral));
                }
            }
            None => parts.push(("No scope loaded".into(), Tone::Neutral)),
        }
        h_flex()
            .debug_selector(|| "k8s-status".into())
            .flex_none()
            .flex_wrap()
            .gap_3()
            .px_4()
            .py_1()
            .text_xs()
            .border_t_1()
            .border_color(tokens.border)
            .children(parts.into_iter().map(|(text, tone)| {
                let color = match tone {
                    Tone::Neutral => tokens.text_muted,
                    tone => tone_color(tone, cx),
                };
                div().text_color(color).child(text)
            }))
            .child(div().flex_1())
            .child(
                div()
                    .text_color(tokens.text_muted)
                    .child("/ Filter · J K Navigate · L Logs"),
            )
            .into_any_element()
    }
}

fn namespace_note(origin: &NamespaceOrigin) -> String {
    match origin {
        NamespaceOrigin::TypedUnchecked => "Namespace as typed.".into(),
        NamespaceOrigin::TypedListed => "Namespace as typed; present in the namespace listing.".into(),
        NamespaceOrigin::TypedListingDenied(failure) => format!(
            "Namespace listing is not allowed ({}); using the typed namespace. Namespaced reads decide access.",
            model::failure_kind(failure)
        ),
        NamespaceOrigin::TypedNotListed => {
            "The typed namespace is not in the complete namespace listing.".into()
        }
        NamespaceOrigin::TypedListingPartial => {
            "The namespace listing was partial; the typed namespace was not in its first page.".into()
        }
        NamespaceOrigin::Context => "Namespace from the context's kubeconfig entry.".into(),
        NamespaceOrigin::KubectlDefault => {
            "No namespace typed or configured: kubectl's default namespace.".into()
        }
    }
}

/// Failures that make every further read pointless until the user acts
/// outside Okilum: shown in the banner at once, not only per source.
fn blocking_failure(loaded: &backend::Loaded) -> Option<String> {
    let failure = match loaded {
        backend::Loaded::Pods(_, Err(f))
        | backend::Loaded::Deployments(_, Err(f))
        | backend::Loaded::ReplicaSets(_, Err(f))
        | backend::Loaded::Services(_, Err(f))
        | backend::Loaded::Slices(_, Err(f))
        | backend::Loaded::Events(_, Err(f))
        | backend::Loaded::Hpas(_, Err(f)) => f,
        _ => return None,
    };
    matches!(
        failure,
        Failure::AuthExpired { .. }
            | Failure::InteractiveLogin { .. }
            | Failure::Configuration { .. }
            | Failure::Unavailable { .. }
    )
    .then(|| failure.summary())
}

pub(crate) fn tone_color(tone: Tone, cx: &App) -> Hsla {
    let tokens = brand::palette(cx);
    match tone {
        Tone::Good => tokens.success,
        Tone::Warning => tokens.warning,
        Tone::Danger => tokens.danger,
        Tone::Neutral => tokens.text_muted,
    }
}

pub(crate) fn tone_dot(tone: Tone, cx: &App) -> Div {
    div()
        .flex_none()
        .mt(px(6.))
        .size(px(8.))
        .rounded_full()
        .bg(tone_color(tone, cx))
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_dock_view(window, cx);
        let tokens = brand::palette(cx);
        let scope = self.render_scope(window, cx);
        let banner = self.render_banner(cx);
        let filterbar = self.render_filterbar(cx);
        let list = self.render_list(cx);
        let inspector = self.render_inspector(cx);
        let dock = self.render_dock(cx);
        let status = self.render_status(cx);
        v_flex()
            .debug_selector(|| "k8s-workspace".into())
            .size_full()
            .min_h_0()
            .bg(tokens.surface)
            .text_color(tokens.text)
            .font_family(brand::SANS_FONT)
            .child(TitleBar::new().child("Kubernetes workspace"))
            .child(scope)
            .children(banner)
            .child(filterbar)
            .child(
                h_flex()
                    .flex_1()
                    .min_h(px(200.))
                    .w_full()
                    .child(list)
                    .child(inspector),
            )
            .child(dock)
            .child(status)
    }
}

#[cfg(test)]
#[path = "kubernetes_workspace/tests.rs"]
mod tests;
