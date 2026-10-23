//! A shared Projects picker, mounted by the title-bar switcher or command palette.
//! It never activates a project or starts/contacts a manager on its own.
use gpui::{
    anchored, div, prelude::*, px, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, Global, IntoElement, KeyBinding, Pixels, Point, Render, ScrollHandle, Subscription,
    Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    tooltip::Tooltip,
    v_flex, ActiveTheme, Icon, IconName, Sizable,
};
use okilum_t3::projects::{
    picker::{
        Group, GroupKey, KindFilter, ManagerObservation, PickerModel, Problem, Row, RowKey,
        RowStatus,
    },
    Discovery, Kind, Project,
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

/// The title-bar switcher outside a project (#1519).
pub const PROJECTS_ICON: &str = "icons/projects.svg";
pub const CODE_ICON: &str = "icons/project-code.svg";
pub const KNOWLEDGE_ICON: &str = "icons/project-knowledge.svg";
pub const HYBRID_ICON: &str = "icons/project-hybrid.svg";
/// After a project's name while its manager agent works.
pub const ACTIVITY_ICON: &str = "icons/activity.svg";
/// The file tree's density: rows of 13 px text, no second line.
const ROW_HEIGHT: f32 = 26.;
const ICON_SLOT: f32 = 16.;

/// The one picture in front of a project's name, here, in the title bar and
/// in the palette. Today it is the kind; a per-project icon or avatar will
/// replace it in this one place.
pub fn project_icon(project: &Project) -> Icon {
    Icon::default().path(match project.kind {
        Kind::Code => CODE_ICON,
        Kind::Knowledge => KNOWLEDGE_ICON,
        Kind::Hybrid => HYBRID_ICON,
    })
}

/// A fixed 16 px slot, so names line up whether or not a picture is there.
pub fn icon_slot(icon: Option<Icon>) -> gpui::Div {
    h_flex()
        .size(px(ICON_SLOT))
        .flex_none()
        .justify_center()
        .children(icon.map(|icon| icon.small()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    /// Own search/filter controls; caller positions `mount` below the switcher.
    Popover,
    /// Palette owns search/focus. Feed `set_query` and route navigation methods.
    Embedded,
    /// The Projects section: own search/filter, left-aligned, fills its host.
    /// Esc only clears the search; there is nothing to dismiss.
    Page,
}
/// The host must revalidate and activate this project through its existing pipeline.
#[derive(Clone, Debug)]
pub struct Selected(pub Project);
/// Emitted by `record_opened` only after the host confirms successful activation.
#[derive(Clone, Debug)]
pub struct RecentsChanged(pub Vec<PathBuf>);
/// «Leave project» at the foot of the title-bar switcher (#1519). Shown only
/// in `Popover` while a project is active; the host leaves and dismisses.
#[derive(Clone, Debug)]
pub struct LeaveProject;

#[derive(Clone)]
enum Item {
    Header(Group),
    Project(Row),
    Problem(Problem),
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
impl EventEmitter<LeaveProject> for ProjectsPicker {}
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
            focus_on_mount: presentation != Presentation::Embedded,
            loading: false,
            _input_subscription: subscription,
        };
        picker.rebuild(cx);
        picker
    }
    /// Read-only view for hosts and host-level tests.
    pub fn model(&self) -> &PickerModel {
        &self.model
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
                    let detail = self.model.manager_detail(&row.project.folder);
                    changed |= row.manager != indicator || row.manager_detail != detail;
                    row.manager = indicator;
                    row.manager_detail = detail;
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
            let problems = std::mem::take(&mut group.problems);
            self.items.push(Item::Header(group));
            self.items.extend(rows.into_iter().map(Item::Project));
            self.items.extend(problems.into_iter().map(Item::Problem));
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
            if self.presentation != Presentation::Page {
                cx.emit(DismissEvent);
            }
        } else {
            self.set_query(String::new(), window, cx);
        }
    }
    pub fn focus_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }
    /// A section label, not a row: small caps-style text over a hairline.
    /// Domains carry their project count; sections never collapse.
    fn render_header(&self, group: &Group, index: usize, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let label = match group.key {
            GroupKey::Recent => group.label.to_uppercase(),
            GroupKey::Domain(_) => format!("{} · {}", group.label.to_uppercase(), group.count),
        };
        div()
            .id(("project-section", index))
            .debug_selector(move || format!("project-section-{index}"))
            .w_full()
            .flex_none()
            .px_2()
            .pt(px(if index == 0 { 4. } else { 10. }))
            .pb(px(4.))
            .when(index > 0, |d| {
                d.mt_1().border_t_1().border_color(theme.border)
            })
            .text_size(px(12.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme.muted_foreground)
            .child(label)
    }
    /// ⚠ for a project.yaml problem, the activity glyph while the manager
    /// works; the tooltip says which. Nothing otherwise.
    fn render_status(status: RowStatus, index: usize, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let (icon, color, tip) = match status {
            RowStatus::Active(tip) => (Icon::default().path(ACTIVITY_ICON), theme.success, tip),
            RowStatus::Problem(tip) => (Icon::new(IconName::TriangleAlert), theme.warning, tip),
        };
        let tip: gpui::SharedString = tip.into();
        div()
            .id(("project-status", index))
            .debug_selector(move || format!("project-status-{index}"))
            .flex_none()
            .child(icon.xsmall().text_color(color))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
    }
    fn render_row(&self, row: &Row, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let key = row.key.clone();
        let hovered_key = key.clone();
        let theme = cx.theme();
        let selected = self.selected.as_ref() == Some(&row.key);
        let active = self.active.as_ref() == Some(&row.project.folder);
        // The domain is already the section's name, except in Recent and
        // while searching, where rows from many domains meet.
        let show_domain =
            row.key.group == GroupKey::Recent || !self.model.query().trim().is_empty();
        let hover = theme.list_hover;
        let muted = theme.muted_foreground;
        let location = row.location.clone();
        let kind: gpui::SharedString = match row.project.kind {
            Kind::Code => "Code project",
            Kind::Knowledge => "Knowledge project",
            Kind::Hybrid => "Hybrid project",
        }
        .into();
        h_flex()
            .id(("project-choice", index))
            .debug_selector(move || format!("project-choice-{index}"))
            .w_full()
            .min_w_0()
            .h(px(ROW_HEIGHT))
            .flex_none()
            .px_2()
            .gap_2()
            .rounded(px(6.))
            .cursor_pointer()
            .when(selected, |r| r.bg(theme.list_active))
            .hover(move |r| r.bg(hover))
            .child(
                div()
                    .id(("project-icon", index))
                    .debug_selector(move || format!("project-icon-{index}"))
                    .flex_none()
                    .text_color(muted)
                    .child(icon_slot(Some(project_icon(&row.project))))
                    .tooltip(move |window, cx| Tooltip::new(kind.clone()).build(window, cx)),
            )
            .child(div().min_w_0().truncate().child(row.project.name.clone()))
            .children(
                row.status()
                    .map(|status| Self::render_status(status, index, cx)),
            )
            .when(show_domain, |r| {
                r.child(
                    div()
                        .flex_none()
                        .text_size(px(12.))
                        .text_color(muted)
                        .child(row.domain.clone()),
                )
            })
            .child(div().flex_1())
            .when(active, |r| {
                r.child(Icon::new(IconName::Check).small().text_color(muted))
            })
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
    /// A folder whose project.yaml cannot be read: a muted row with the
    /// same ⚠ after its name; the reason is in the tooltip.
    fn render_problem(&self, problem: &Problem, index: usize, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .id(("project-problem", index))
            .debug_selector(move || format!("project-problem-{index}"))
            .w_full()
            .min_w_0()
            .h(px(ROW_HEIGHT))
            .flex_none()
            .px_2()
            .gap_2()
            .text_color(theme.muted_foreground)
            .child(icon_slot(None))
            .child(div().min_w_0().truncate().child(problem.name.clone()))
            .child(Self::render_status(
                RowStatus::Problem(format!("project.yaml cannot be read: {}", problem.detail)),
                index,
                cx,
            ))
    }
    /// All / Code / Knowledge / Hybrid as one compact segmented control.
    fn render_filter(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (track, thumb) = (theme.muted, theme.background);
        let (on, off) = (theme.foreground, theme.muted_foreground);
        let current = self.model.filter();
        h_flex().w_full().child(
            h_flex()
                .id("project-kind-filter")
                .debug_selector(|| "project-kind-filter".into())
                .flex_none()
                .p(px(2.))
                .gap(px(2.))
                .rounded(px(7.))
                .bg(track)
                .children(KindFilter::ALL.into_iter().enumerate().map(|(i, filter)| {
                    let selected = current == filter;
                    div()
                        .id(("project-kind", i))
                        .debug_selector(move || format!("project-kind-{i}"))
                        .h(px(22.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .text_size(px(12.))
                        .cursor_pointer()
                        .text_color(if selected { on } else { off })
                        .when(selected, |d| {
                            d.bg(thumb)
                                .shadow_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                        })
                        .when(!selected, |d| d.hover(move |d| d.text_color(on)))
                        .child(filter.label())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_filter(filter, cx);
                            this.focus_input(window, cx);
                        }))
                })),
        )
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
        let page = self.presentation == Presentation::Page;
        let own_controls = popover || page;
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
                    .w(px(400.))
                    .max_w(window.viewport_size().width - px(24.))
                    .p_2()
                    .rounded_lg()
                    .shadow_lg()
                    .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            })
            .when(self.presentation == Presentation::Embedded, |d| d.w_full())
            .when(page, |d| d.size_full().max_w(px(760.)))
            .on_action(cx.listener(|this, _: &NextProject, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousProject, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &AcceptProject, _, cx| {
                this.confirm(cx);
            }))
            .on_action(cx.listener(|this, _: &DismissProjects, window, cx| this.escape(window, cx)))
            .when(own_controls, |d| {
                d.child(
                    Input::new(&self.input)
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small()),
                )
            });
        if own_controls {
            content = content.child(self.render_filter(cx));
        }
        let max_height = (window.viewport_size().height - px(160.))
            .max(px(100.))
            .min(px(520.));
        content = content.child(
            v_flex()
                .id("projects-picker-results")
                .debug_selector(|| "projects-picker-results".into())
                .track_scroll(&self.scroll)
                .w_full()
                .when(page, |d| d.flex_1().min_h_0())
                .when(!page, |d| d.max_h(max_height))
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
                    Item::Problem(problem) => {
                        self.render_problem(problem, i, cx).into_any_element()
                    }
                })),
        );
        if popover && self.active.is_some() {
            // The switcher's menu ends with Leave project; the title bar has
            // no separate button for it (#1519).
            content = content.child(
                h_flex()
                    .w_full()
                    .pt_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("projects-picker-leave")
                            .ghost()
                            .small()
                            .label("Leave project")
                            .debug_selector(|| "projects-picker-leave".into())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(LeaveProject))),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .px_2()
                            .text_size(px(12.))
                            .text_color(muted)
                            .child("↵ Open"),
                    ),
            );
        }
        content
    }
}

#[cfg(test)]
mod tests;
