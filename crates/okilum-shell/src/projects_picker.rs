//! A shared Projects picker, mounted by the title-bar switcher or command palette.
//! It never activates a project or starts/contacts a manager on its own.
use gpui::{
    anchored, div, prelude::*, px, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, Global, IntoElement, KeyBinding, Pixels, Point, Render, ScrollHandle, Subscription,
    Window,
};
use gpui_component::{
    button::{Button, ButtonGroup, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    tooltip::Tooltip,
    v_flex, ActiveTheme, Icon, IconName, Selectable, Sizable,
};
use okilum_t3::projects::{
    picker::{
        kind_label, Group, GroupKey, KindFilter, ManagerIndicator, ManagerObservation, PickerModel,
        Row, RowKey,
    },
    Discovery, Project,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub use okilum_t3::projects::picker;

gpui::actions!(
    projects_picker,
    [NextProject, PreviousProject, AcceptProject, DismissProjects]
);
struct KeysInstalled;
impl Global for KeysInstalled {}
fn init(cx: &mut App) {
    if cx.has_global::<KeysInstalled>() {
        return;
    }
    cx.set_global(KeysInstalled);
    cx.bind_keys([
        KeyBinding::new("down", NextProject, Some("ProjectsPicker > Input")),
        KeyBinding::new("up", PreviousProject, Some("ProjectsPicker > Input")),
        KeyBinding::new("enter", AcceptProject, Some("ProjectsPicker > Input")),
        KeyBinding::new("escape", DismissProjects, Some("ProjectsPicker > Input")),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    /// Own search/filter controls; caller positions `mount` below the switcher.
    Popover,
    /// Palette owns search/focus. Feed `set_query` and route navigation methods.
    Embedded,
}
/// The host must revalidate and activate this project through its existing pipeline.
#[derive(Clone, Debug)]
pub struct Selected(pub Project);
/// Emitted by `record_opened` only after the host confirms successful activation.
#[derive(Clone, Debug)]
pub struct RecentsChanged(pub Vec<PathBuf>);

#[derive(Clone)]
enum Item {
    Header(Group),
    Project(Row),
}

pub struct ProjectsPicker {
    model: PickerModel,
    presentation: Presentation,
    input: Entity<InputState>,
    items: Vec<Item>,
    selected: Option<RowKey>,
    active: Option<PathBuf>,
    scroll: ScrollHandle,
    focus_on_mount: bool,
    loading: bool,
    _input_subscription: Subscription,
}
impl EventEmitter<Selected> for ProjectsPicker {}
impl EventEmitter<RecentsChanged> for ProjectsPicker {}
impl EventEmitter<DismissEvent> for ProjectsPicker {}
impl Focusable for ProjectsPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

/// Window-coordinate anchor at the switcher's left/bottom edge; never centered.
pub fn mount(picker: Entity<ProjectsPicker>, origin: Point<Pixels>) -> impl IntoElement {
    anchored()
        .position(origin)
        .snap_to_window_with_margin(px(12.))
        .child(picker)
}

impl ProjectsPicker {
    pub fn new(
        model: PickerModel,
        presentation: Presentation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        init(cx);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search projects…"));
        input.update(cx, |input, cx| {
            input.set_value(model.query().to_owned(), window, cx)
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                let query = this.input.read(cx).value().to_string();
                if this.model.query() != query {
                    this.model.set_query(query);
                    this.rebuild(cx);
                }
            }
        });
        let mut picker = Self {
            model,
            presentation,
            input,
            items: Vec::new(),
            selected: None,
            active: None,
            scroll: ScrollHandle::new(),
            focus_on_mount: presentation == Presentation::Popover,
            loading: false,
            _input_subscription: subscription,
        };
        picker.rebuild(cx);
        picker
    }
    pub fn set_query(
        &mut self,
        query: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = query.into();
        if self.model.query() == query && self.input.read(cx).value().as_str() == query {
            return;
        }
        self.model.set_query(query.clone());
        self.input
            .update(cx, |input, cx| input.set_value(query, window, cx));
        self.rebuild(cx);
    }
    pub fn set_filter(&mut self, filter: KindFilter, cx: &mut Context<Self>) {
        self.model.set_filter(filter);
        self.rebuild(cx);
    }
    pub fn update_discovery(&mut self, discovery: Discovery, cx: &mut Context<Self>) {
        self.model.refresh(discovery);
        self.loading = false;
        self.rebuild(cx);
    }
    pub fn update_statuses(
        &mut self,
        observations: BTreeMap<PathBuf, ManagerObservation>,
        cx: &mut Context<Self>,
    ) {
        if !self.model.set_observations(observations) {
            return;
        }
        let mut changed = false;
        for item in &mut self.items {
            if let Item::Project(row) = item {
                if let Some(indicator) = self.model.manager_indicator(&row.project.folder) {
                    changed |= row.manager != indicator;
                    row.manager = indicator;
                }
            }
        }
        if changed {
            cx.notify();
        }
    }
    pub fn set_loading(&mut self, loading: bool, cx: &mut Context<Self>) {
        self.loading = loading;
        cx.notify();
    }
    pub fn set_active(&mut self, folder: Option<PathBuf>, cx: &mut Context<Self>) {
        self.active = folder;
        cx.notify();
    }
    /// Selection is a request, not proof of opening. The host calls this only
    /// after activation succeeds, then persists the emitted vault-scoped history.
    pub fn record_opened(&mut self, folder: &Path, cx: &mut Context<Self>) {
        if self.model.record_opened(folder) {
            cx.emit(RecentsChanged(self.model.recent().to_vec()));
            self.rebuild(cx);
        }
    }
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.items.clear();
        for mut group in self.model.groups() {
            let rows = std::mem::take(&mut group.rows);
            self.items.push(Item::Header(group));
            self.items.extend(rows.into_iter().map(Item::Project));
        }
        if !self
            .items
            .iter()
            .any(|item| matches!(item, Item::Project(r) if Some(&r.key) == self.selected.as_ref()))
        {
            self.selected = self.items.iter().find_map(|item| match item {
                Item::Project(r) => Some(r.key.clone()),
                _ => None,
            });
        }
        self.scroll_to_selection();
        cx.notify();
    }
    fn scroll_to_selection(&self) {
        if let Some(index) = self.items.iter().position(
            |item| matches!(item, Item::Project(r) if Some(&r.key) == self.selected.as_ref()),
        ) {
            self.scroll.scroll_to_item(index);
        }
    }
    /// Palette adapters can route their Up/Down actions here without moving focus.
    pub fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let keys: Vec<_> = self
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Project(r) => Some(r.key.clone()),
                _ => None,
            })
            .collect();
        if keys.is_empty() {
            return;
        }
        let current = keys
            .iter()
            .position(|key| Some(key) == self.selected.as_ref())
            .unwrap_or(0);
        self.selected =
            Some(keys[current.saturating_add_signed(delta).min(keys.len() - 1)].clone());
        self.scroll_to_selection();
        cx.notify();
    }
    pub fn confirm(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(selected) = &self.selected else {
            return false;
        };
        let Some(project) = self.model.project(&selected.folder) else {
            return false;
        };
        cx.emit(Selected(project.clone()));
        true
    }
    /// Esc clears an active search first, then dismisses the popover.
    pub fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.query().is_empty() {
            cx.emit(DismissEvent);
        } else {
            self.set_query(String::new(), window, cx);
        }
    }
    pub fn focus_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }
    fn render_header(
        &self,
        group: &Group,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let key = group.key.clone();
        let muted = cx.theme().muted_foreground;
        h_flex()
            .id(("project-domain", index))
            .debug_selector(move || format!("project-domain-{index}"))
            .w_full()
            .px_2()
            .h(px(30.))
            .gap_1()
            .text_size(px(12.))
            .text_color(muted)
            .font_weight(gpui::FontWeight::MEDIUM)
            .when(matches!(key, GroupKey::Domain(_)), |header| {
                header.cursor_pointer().child(
                    Icon::new(if group.collapsed {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    })
                    .small(),
                )
            })
            .child(group.label.clone())
            .child(div().flex_1())
            .when(matches!(key, GroupKey::Domain(_)), |header| {
                header.child(group.count.to_string()).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let GroupKey::Domain(domain) = &key {
                            this.model.toggle_domain(domain);
                            this.rebuild(cx);
                        }
                    },
                ))
            })
    }
    fn render_row(&self, row: &Row, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let key = row.key.clone();
        let hovered_key = key.clone();
        let theme = cx.theme();
        let selected = self.selected.as_ref() == Some(&row.key);
        let active = self.active.as_ref() == Some(&row.project.folder);
        let recent = row.key.group == GroupKey::Recent;
        let dot_color = match row.manager {
            ManagerIndicator::Running => theme.success,
            ManagerIndicator::Unreachable => theme.danger,
            ManagerIndicator::Checking => theme.warning,
            ManagerIndicator::Idle | ManagerIndicator::NotStarted => theme.muted_foreground,
        };
        let label = row.manager.label();
        let hover = theme.list_hover;
        let location = row.location.clone();
        h_flex()
            .id(("project-choice", index))
            .debug_selector(move || format!("project-choice-{index}"))
            .w_full()
            .min_w_0()
            .h(px(34.))
            .px_2()
            .gap_2()
            .rounded_md()
            .cursor_pointer()
            .when(selected, |r| r.bg(theme.list_active))
            .hover(move |r| r.bg(hover))
            .child(
                div()
                    .id(("project-manager-dot", index))
                    .size(px(7.))
                    .flex_none()
                    .rounded_full()
                    .bg(dot_color)
                    .tooltip(move |window, cx| Tooltip::new(label).build(window, cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(row.project.name.clone()),
            )
            .when(recent, |r| {
                r.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.muted_foreground)
                        .child(row.domain.clone()),
                )
            })
            .child(
                div()
                    .px_1()
                    .py_0p5()
                    .rounded_sm()
                    .text_size(px(12.))
                    .bg(theme.muted)
                    .text_color(theme.muted_foreground)
                    .child(kind_label(row.project.kind)),
            )
            .child(
                div()
                    .w(px(14.))
                    .flex_none()
                    .when(active, |d| d.child(Icon::new(IconName::Check).small())),
            )
            .tooltip(move |window, cx| Tooltip::new(location.clone()).build(window, cx))
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.selected.as_ref() != Some(&hovered_key) {
                    this.selected = Some(hovered_key.clone());
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(key.clone());
                this.confirm(cx);
            }))
    }
}
impl Render for ProjectsPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_on_mount) {
            let input = self.input.clone();
            window.defer(cx, move |window, cx| {
                input.update(cx, |input, cx| input.focus(window, cx))
            });
        }
        let theme = cx.theme();
        let surface = theme.popover;
        let foreground = theme.popover_foreground;
        let muted = theme.muted_foreground;
        let popover = self.presentation == Presentation::Popover;
        let mut content = v_flex()
            .id("projects-picker")
            .debug_selector(|| "projects-picker".into())
            .min_w_0()
            .text_size(px(13.))
            .text_color(foreground)
            .bg(surface)
            .gap_1()
            .key_context("ProjectsPicker")
            .when(popover, |d| {
                d.occlude()
                    .w(px(470.))
                    .max_w(window.viewport_size().width - px(24.))
                    .p_2()
                    .rounded_lg()
                    .shadow_lg()
                    .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            })
            .when(!popover, |d| d.w_full())
            .on_action(cx.listener(|this, _: &NextProject, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousProject, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &AcceptProject, _, cx| {
                this.confirm(cx);
            }))
            .on_action(cx.listener(|this, _: &DismissProjects, window, cx| this.escape(window, cx)))
            .when(popover, |d| {
                d.child(
                    Input::new(&self.input)
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small()),
                )
            });
        if popover {
            content = content.child(ButtonGroup::new("project-kind-filter").children(
                KindFilter::ALL.into_iter().enumerate().map(|(i, filter)| {
                    Button::new(("project-kind", i))
                        .small()
                        .ghost()
                        .label(filter.label())
                        .selected(self.model.filter() == filter)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_filter(filter, cx);
                            this.focus_input(window, cx);
                        }))
                }),
            ));
        }
        let max_height = (window.viewport_size().height - px(160.))
            .max(px(100.))
            .min(px(520.));
        content = content.child(
            v_flex()
                .id("projects-picker-results")
                .track_scroll(&self.scroll)
                .max_h(max_height)
                .overflow_y_scroll()
                .when(self.loading, |d| {
                    d.child(
                        div()
                            .px_2()
                            .py_2()
                            .text_color(muted)
                            .child("Loading projects…"),
                    )
                })
                .when(!self.loading && self.items.is_empty(), |d| {
                    d.child(
                        div()
                            .px_2()
                            .py_2()
                            .text_color(muted)
                            .child("No matching projects"),
                    )
                })
                .children(self.items.iter().enumerate().map(|(i, item)| match item {
                    Item::Header(group) => self.render_header(group, i, cx).into_any_element(),
                    Item::Project(row) => self.render_row(row, i, cx).into_any_element(),
                })),
        );
        for (i, (path, error)) in self.model.errors().iter().enumerate() {
            let error = error.clone();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            content = content.child(
                div()
                    .id(("project-discovery-error", i))
                    .px_2()
                    .text_size(px(12.))
                    .text_color(muted)
                    .child(format!("{name} — Cannot read this project"))
                    .tooltip(move |window, cx| Tooltip::new(error.clone()).build(window, cx)),
            );
        }
        content
    }
}

#[cfg(test)]
mod tests;
