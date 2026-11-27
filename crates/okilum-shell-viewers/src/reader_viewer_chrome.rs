//! File-viewer descriptions and the shared seven-slot header. Every slot is
//! erased at its boundary: the native Windows main stack must stay bounded.
use crate::prelude::*;
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use std::rc::Rc;

pub mod overflow;
pub use overflow::{layout_with_widths, Disposition, LayoutItem, Order as OverflowOrder};

pub type Invoke = Rc<dyn Fn(&mut Window, &mut App)>;
pub type BuildMenu = Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

#[derive(Clone)]
pub struct ViewerAction {
    pub id: &'static str,
    pub icon: Icon,
    pub label: SharedString,
    pub tooltip: SharedString,
    pub enabled: bool,
    pub disabled_reason: Option<SharedString>,
    pub toggled: bool,
    pub priority: u16,
    pub labelled: bool,
    /// ZIP extraction and labelled log menus keep a label without enabling
    /// optional toolbar labels. Their supplied collapse stage still applies.
    pub label_by_default: bool,
    /// Most labels yield first; mandatory actions such as ZIP Extract all may
    /// keep theirs until the menu-collapse stage of their supplied order table.
    pub collapse_priority: u16,
    pub filled: bool,
    pub never_dropped: bool,
    pub shortcut: Option<Rc<dyn Action>>,
    pub invoke: Invoke,
    pub menu: Option<BuildMenu>,
    pub caret: bool,
    pub minimum_width: f32,
    /// A measured sibling, like Find/Save. Its notifications cannot dirty the
    /// cached Header ancestor; the descriptor and reserved size stay static.
    pub live: Option<LiveSlot>,
}

#[derive(Clone)]
pub struct LiveSlot {
    pub view: AnyView,
    pub bounds: Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    pub width: f32,
}

impl LiveSlot {
    pub fn new(view: impl Into<AnyView>, width: f32) -> Self {
        Self {
            view: view.into(),
            bounds: Default::default(),
            width,
        }
    }

    #[inline(never)]
    pub fn placeholder(&self) -> AnyElement {
        let bounds = self.bounds.clone();
        div()
            .relative()
            .flex_none()
            .w(px(self.width))
            .h(px(28.))
            .child(
                canvas(
                    move |measured, _, _| bounds.set(Some(measured)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }

    #[inline(never)]
    pub fn overlay(self) -> AnyElement {
        canvas(
            move |_, window, cx| {
                self.bounds.get().map(|bounds| {
                    let mut element = self.view.into_any_element();
                    element.layout_as_root(bounds.size.into(), window, cx);
                    element.prepaint_at(bounds.origin, window, cx);
                    element
                })
            },
            |_, element, window, cx| {
                if let Some(mut element) = element {
                    element.paint(window, cx);
                }
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
}

impl ViewerAction {
    pub fn new(
        id: &'static str,
        icon: Icon,
        label: impl Into<SharedString>,
        invoke: Invoke,
    ) -> Self {
        let label = label.into();
        Self {
            id,
            icon,
            tooltip: label.clone(),
            label,
            enabled: true,
            disabled_reason: None,
            toggled: false,
            priority: 30,
            labelled: false,
            label_by_default: false,
            collapse_priority: 0,
            filled: false,
            never_dropped: false,
            shortcut: None,
            invoke,
            menu: None,
            caret: false,
            minimum_width: 28.,
            live: None,
        }
    }

    pub fn with_action<A: Action>(mut self, action: A, cx: &App) -> Self {
        self.tooltip = reader_shortcuts::hint(&self.label, &action, cx);
        self.shortcut = Some(Rc::new(action));
        self
    }

    pub fn button(&self, collapsed: bool) -> Button {
        let invoke = self.invoke.clone();
        Button::new(self.id)
            .ghost()
            .when(self.filled, |b| b.secondary())
            .icon(self.icon.clone())
            .h(px(28.))
            .rounded(px(6.))
            .when(collapsed || !self.labelled, |b| b.w(px(self.minimum_width)))
            .dropdown_caret(self.caret)
            .when(!collapsed && self.labelled, |b| b.label(self.label.clone()))
            .accessibility_label(self.label.clone())
            .tooltip(
                self.disabled_reason
                    .clone()
                    .unwrap_or_else(|| self.tooltip.clone()),
            )
            .disabled(!self.enabled)
            .selected(self.toggled)
            .debug_selector({
                let id = self.id;
                move || id.into()
            })
            .when(self.menu.is_none(), |button| {
                button.on_click(move |_, window, cx| invoke(window, cx))
            })
    }

    pub fn render_button(&self, collapsed: bool) -> AnyElement {
        if let Some(slot) = &self.live {
            return slot.placeholder();
        }
        let button = self.button(collapsed);
        if let Some(menu) = self.menu.clone() {
            button
                .dropdown_menu_with_anchor(Anchor::TopRight, move |m, w, cx| menu(m, w, cx))
                .into_any_element()
        } else {
            button.into_any_element()
        }
    }

    pub fn menu_item(&self) -> PopupMenuItem {
        let invoke = self.invoke.clone();
        let label = if !self.enabled {
            self.disabled_reason
                .as_ref()
                .map(|reason| format!("{} — {reason}", self.label).into())
                .unwrap_or_else(|| self.label.clone())
        } else {
            self.label.clone()
        };
        let mut item = PopupMenuItem::new(label)
            .icon(self.icon.clone())
            .checked(self.toggled)
            .disabled(!self.enabled)
            .on_click(move |_, window, cx| invoke(window, cx));
        if let Some(shortcut) = &self.shortcut {
            item = item.action(shortcut.boxed_clone());
        }
        item
    }

    pub fn layout_item(&self, labels: bool, window: &Window) -> LayoutItem {
        let wide = if (labels || self.label_by_default) && self.labelled {
            // Toolkit Medium buttons use a 1rem label, 20px padding, a
            // 16px icon and an 8px gap. A caret adds another 16px + 8px.
            toolbar_text_width(&self.label, FontWeight::MEDIUM, window) * window.rem_size().as_f32()
                / brand::READER_CHROME_FONT_SIZE
                + 44.
                + if self.caret { 24. } else { 0. }
        } else {
            self.minimum_width
        };
        let mut item = LayoutItem::action(
            self.id,
            wide,
            self.minimum_width,
            self.priority,
            self.never_dropped,
        );
        for step in &mut item.steps {
            if step.disposition == Disposition::Collapsed {
                step.priority = self.collapse_priority;
            }
        }
        item
    }

    pub fn append_menu(
        &self,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        if let Some(build) = &self.menu {
            let build = build.clone();
            menu.submenu_with_icon(
                Some(self.icon.clone()),
                self.label.clone(),
                window,
                cx,
                move |m, w, cx| build(m, w, cx),
            )
        } else {
            menu.item(self.menu_item())
        }
    }
}

#[derive(Clone)]
pub struct ViewerView {
    pub action: ViewerAction,
    pub active: bool,
}

pub use crate::reader_find_field::Target as ViewerFindTarget;

#[derive(Clone, Default)]
pub struct ViewerChrome {
    pub overflow_order: OverflowOrder,
    pub views: Vec<ViewerView>,
    pub compact_views: bool,
    pub find: Option<ViewerFindTarget>,
    pub actions: Vec<ViewerAction>,
    pub action_text: Option<SharedString>,
    pub action_text_after: Option<&'static str>,
    pub copy_actions: Vec<ViewerAction>,
    pub copy_in_menu: bool,
    pub open_actions: Vec<ViewerAction>,
    pub status: Option<reader_status_line::StatusLine>,
    pub state: Option<ViewerAction>,
    pub badges: Vec<ViewerAction>,
    pub compact_menu_id: Option<&'static str>,
}

pub struct HeaderLayout {
    pub items: Vec<LayoutItem>,
    pub states: Vec<Disposition>,
    pub widths: Vec<f32>,
    pub labels: bool,
    pub editable: bool,
    pub editing: bool,
    pub width_applies: bool,
    pub collapse_parents: bool,
    pub view_menu: bool,
}

impl HeaderLayout {
    pub fn shows(&self, id: &str) -> bool {
        self.items
            .iter()
            .zip(&self.states)
            .any(|(item, state)| item.id == id && *state != Disposition::Menu)
    }
    pub fn width(&self, id: &str) -> f32 {
        self.items
            .iter()
            .zip(&self.widths)
            .find(|(item, _)| item.id == id)
            .map_or(0., |(_, width)| *width)
    }
    pub fn collapsed(&self, id: &str) -> bool {
        self.items
            .iter()
            .zip(&self.states)
            .any(|(item, state)| item.id == id && *state == Disposition::Collapsed)
    }
}

pub fn views_slot(chrome: &ViewerChrome, plan: &HeaderLayout, cx: &App) -> AnyElement {
    reader_segmented::render(
        &chrome.views,
        if plan.view_menu {
            reader_segmented::Form::Menu
        } else if plan.collapsed("views") {
            reader_segmented::Form::Icons
        } else {
            reader_segmented::Form::Full
        },
        cx,
    )
}

pub fn separator(cx: &App) -> AnyElement {
    div()
        .flex_none()
        .w(px(1.))
        .h(px(16.))
        .mx_1()
        .bg(brand::palette(cx).border_subtle)
        .into_any_element()
}

#[inline(never)]
pub fn viewer_actions_slot(chrome: &ViewerChrome, plan: &HeaderLayout, cx: &App) -> AnyElement {
    let actions: Vec<_> = chrome.actions.iter().filter(|a| plan.shows(a.id)).collect();
    let text = || {
        div()
            .debug_selector(|| "viewer-action-text".into())
            .text_xs()
            .text_color(brand::palette(cx).text_muted)
            .children(chrome.action_text.clone())
            .into_any_element()
    };
    h_flex()
        .flex_none()
        .gap_1()
        .when(!actions.is_empty(), |row| row.child(separator(cx)))
        .children(actions.into_iter().map(|a| {
            h_flex()
                .gap_1()
                .child(
                    a.render_button((!plan.labels && !a.label_by_default) || plan.collapsed(a.id)),
                )
                .when(
                    plan.shows("viewer-action-text") && chrome.action_text_after == Some(a.id),
                    |row| row.child(text()),
                )
                .into_any_element()
        }))
        .when(
            plan.shows("viewer-action-text") && chrome.action_text_after.is_none(),
            |row| row.child(text()),
        )
        .into_any_element()
}

pub fn menu_button(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    labels: bool,
    cx: &App,
) -> Button {
    reader_icon_button(id, icon, label, cx)
        .small()
        .w_auto()
        .min_w(px(40.))
        .px_1()
        .dropdown_caret(true)
        .when(labels, |b| b.label(label))
        .accessibility_label(label)
        .debug_selector(move || id.into())
}

/// Hosts without a document header reserve Find and draw the same erased action
/// slot and overflow menus. They do not invent another set of log controls.
#[inline(never)]
pub fn render_compact_header(
    chrome: &ViewerChrome,
    width: f32,
    window: &Window,
    cx: &App,
) -> AnyElement {
    render_compact_header_with_leading(chrome, width, None, window, cx)
}

#[inline(never)]
pub fn render_compact_header_with_leading(
    chrome: &ViewerChrome,
    width: f32,
    leading: Option<AnyElement>,
    window: &Window,
    cx: &App,
) -> AnyElement {
    // Code entry panes keep the trigger in the compact row. The same field
    // expands over the crumb, preserving the switch and action positions.
    let compact_find = chrome.compact_menu_id == Some("code-compact-more") && width < 600.;
    let has_leading = leading.is_some();
    let labels = reader_ui_state::toolbar_labels(cx) && width >= 1400.;
    let mut items: Vec<_> = chrome
        .actions
        .iter()
        .map(|action| action.layout_item(labels, window))
        .collect();
    if let Some(find) = &chrome.find {
        let mut item = LayoutItem::pinned(
            "find",
            if compact_find { 28. } else { 200. },
            if find.large_file && !compact_find {
                128.
            } else {
                28.
            },
        );
        item.steps = vec![overflow::OverflowStep {
            priority: chrome.overflow_order.find_narrow,
            width: 128.,
            disposition: Disposition::Collapsed,
        }];
        if !find.large_file || compact_find {
            item.steps.push(overflow::OverflowStep {
                priority: chrome.overflow_order.find_icon,
                width: 28.,
                disposition: Disposition::Collapsed,
            });
        }
        if compact_find {
            item.steps.clear();
        }
        items.push(item);
    }
    if chrome.views.len() > 1 {
        let active = chrome
            .views
            .iter()
            .find(|v| v.active)
            .unwrap_or(&chrome.views[0]);
        items.push(LayoutItem::pinned(
            "views",
            52. + toolbar_text_width(&active.action.label, FontWeight::MEDIUM, window),
            80.,
        ));
    }
    items.push(LayoutItem::pinned("more", 28., 28.));
    let state_width = 32. * (usize::from(chrome.state.is_some()) + chrome.badges.len()) as f32;
    let (states, widths) = layout_with_widths(
        width,
        (if has_leading { 112. } else { 32. }) + state_width,
        0.,
        &items,
    );
    let plan = HeaderLayout {
        items,
        states,
        widths,
        labels,
        editable: false,
        editing: false,
        width_applies: false,
        collapse_parents: false,
        view_menu: false,
    };
    let find = chrome.find.as_ref().map(|target| {
        let mut target = target.clone();
        if compact_find {
            target.large_file = false;
        }
        let spare = (width - plan.widths.iter().sum::<f32>() - state_width - 56.).max(0.);
        let focused = target.input.read(cx).focus_handle(cx).is_focused(window);
        let left_space = plan.width("find") + if has_leading { spare } else { state_width };
        // A compact pane has no history buttons. Its focused field is an
        // overlay of the row, with enough room for the query and step buttons.
        // Preserve every underlying slot's position, including the view menu.
        let expanded = 272_f32.min((width - 24.).max(28.));
        let over_row = compact_find && focused;
        let field = reader_find_field::render(
            &target,
            reader_find_field::Presentation::Header {
                width: plan.width("find"),
                focused,
                max_width: if over_row {
                    expanded
                } else {
                    left_space.max(plan.width("find"))
                },
            },
            cx,
        );
        if over_row {
            div()
                .relative()
                .flex_none()
                .w(px(plan.width("find")))
                .h(px(28.))
                .child(deferred(
                    div()
                        .absolute()
                        .left(px((expanded - left_space).max(0.)))
                        .top_0()
                        .child(field),
                ))
                .into_any_element()
        } else {
            field
        }
    });
    let overflow: Vec<_> = chrome
        .actions
        .iter()
        .filter(|action| !plan.shows(action.id))
        .cloned()
        .collect();
    let copies = chrome.copy_actions.clone();
    let opens = chrome.open_actions.clone();
    let more_id = chrome.compact_menu_id.unwrap_or("log-more");
    h_flex()
        .debug_selector(|| "viewer-compact-header".into())
        .w_full()
        .flex_none()
        .min_w_0()
        .h(px(48.))
        .px_3()
        .gap_1()
        .children(leading)
        .children(
            chrome
                .state
                .iter()
                .chain(chrome.badges.iter())
                .map(|badge| badge.button(true)),
        )
        .children(find)
        .child(reader_segmented::render(
            &chrome.views,
            reader_segmented::Form::Menu,
            cx,
        ))
        .child(viewer_actions_slot(chrome, &plan, cx))
        .when(!has_leading, |row| row.child(div().flex_1()))
        .child(
            reader_icon_button(more_id, IconName::Ellipsis, "Document actions", cx)
                .debug_selector(move || more_id.into())
                .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, window, cx| {
                    for action in overflow.iter().chain(opens.iter()) {
                        menu = action.append_menu(menu, window, cx);
                    }
                    if !copies.is_empty() {
                        let copies = copies.clone();
                        menu = menu.submenu("Copy", window, cx, move |mut menu, window, cx| {
                            for action in &copies {
                                menu = action.append_menu(menu, window, cx);
                            }
                            menu
                        });
                    }
                    menu
                }),
        )
        .into_any_element()
}
