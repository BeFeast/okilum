//! File-viewer descriptions and the shared seven-slot header. Every slot is
//! erased at its boundary: the native Windows main stack must stay bounded.
use super::*;
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use std::rc::Rc;

pub(crate) mod overflow;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) use overflow::{layout_with_widths, Disposition, LayoutItem, Order as OverflowOrder};

pub(crate) type Invoke = Rc<dyn Fn(&mut Window, &mut App)>;
pub(crate) type BuildMenu =
    Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

#[cfg(test)]
thread_local! {
    static OPEN_WITH_ITEMS: std::cell::RefCell<Option<(WeakEntity<PopupMenu>, Vec<SharedString>)>> = const { std::cell::RefCell::new(None) };
    static HEADER_RENDERS: std::cell::RefCell<std::collections::HashMap<EntityId, usize>> = Default::default();
    static RENDERED_ACTIONS: std::cell::RefCell<std::collections::HashMap<EntityId, Vec<RenderedAction>>> = Default::default();
}

#[cfg(test)]
pub(crate) fn header_renders(reader: EntityId) -> usize {
    HEADER_RENDERS.with(|counts| counts.borrow().get(&reader).copied().unwrap_or(0))
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct RenderedAction {
    pub id: &'static str,
    pub label: SharedString,
    pub filled: bool,
    pub tooltip: SharedString,
}

#[cfg(test)]
pub(crate) fn rendered_actions(reader: EntityId) -> Vec<RenderedAction> {
    RENDERED_ACTIONS.with(|states| states.borrow().get(&reader).cloned().unwrap_or_default())
}

/// Only presentation state invalidates the cached slot builders. A Reader
/// notification may describe editor bookkeeping that has no header consequence.
#[derive(PartialEq)]
struct HeaderKey {
    file: (PathBuf, String, String),
    editing: Option<EntityId>,
    viewers: Vec<EntityId>,
    drawing_revision: Option<u64>,
    overflow_order: OverflowOrder,
    geometry: (f32, reader_layout::Panels, reader_layout::Widths),
    navigation: (usize, usize, bool),
    labels: bool,
    full_width: bool,
    editable: bool,
    renaming: Option<(EntityId, Option<String>)>,
    actions: Vec<ActionKey>,
    views: Vec<bool>,
    compact_views: bool,
    find: Option<FindKey>,
    action_text: Option<SharedString>,
    action_text_after: Option<&'static str>,
    deleted: bool,
}
#[derive(PartialEq)]
struct ActionKey {
    id: &'static str,
    label: SharedString,
    tooltip: SharedString,
    reason: Option<SharedString>,
    flags: (bool, bool, bool, bool, bool, bool),
    priority: (u16, u16),
    menu: (bool, bool),
    minimum_width: f32,
    live: Option<EntityId>,
}
impl From<&ViewerAction> for ActionKey {
    fn from(action: &ViewerAction) -> Self {
        Self {
            id: action.id,
            label: action.label.clone(),
            tooltip: action.tooltip.clone(),
            reason: action.disabled_reason.clone(),
            flags: (
                action.enabled,
                action.toggled,
                action.labelled,
                action.label_by_default,
                action.filled,
                action.never_dropped,
            ),
            priority: (action.priority, action.collapse_priority),
            menu: (action.menu.is_some(), action.caret),
            minimum_width: action.minimum_width,
            live: action.live.as_ref().map(|slot| slot.view.entity_id()),
        }
    }
}

#[derive(PartialEq)]
struct FindKey {
    input: EntityId,
    scope: SharedString,
    large_file: bool,
}

impl HeaderKey {
    fn new(reader: &Reader, _window: &Window, cx: &App) -> Self {
        let chrome = reader.viewer_header_chrome(cx);
        Self {
            file: (
                reader.vault_root.clone(),
                reader.selected_file().to_owned(),
                reader.selected_title(),
            ),
            viewers: reader
                .file_preview
                .as_ref()
                .map(|preview| {
                    // Callback ownership is presentation state even if two viewers
                    // happen to have identical labels and capabilities.
                    [
                        preview.pdf.as_ref().map(Entity::entity_id),
                        preview.code.as_ref().map(Entity::entity_id),
                        preview.table.as_ref().map(Entity::entity_id),
                        preview.archive.as_ref().map(Entity::entity_id),
                        preview.audio.as_ref().map(Entity::entity_id),
                        preview.canvas.as_ref().map(Entity::entity_id),
                        preview.drawing.as_ref().map(Entity::entity_id),
                        preview.log.as_ref().map(Entity::entity_id),
                        preview.virtual_log.as_ref().map(Entity::entity_id),
                    ]
                    .into_iter()
                    .flatten()
                    .collect()
                })
                .unwrap_or_default(),
            overflow_order: chrome.overflow_order,
            drawing_revision: reader
                .file_preview
                .as_ref()
                .and_then(|preview| preview.drawing.as_ref())
                .map(|drawing| drawing.read(cx).revision()),
            editing: reader
                .editing
                .as_ref()
                .map(|editing| editing.input().entity_id()),
            geometry: (
                reader.body_bounds.size.width.as_f32(),
                reader.panels.clone(),
                reader.panel_widths.clone(),
            ),
            navigation: (
                reader.navigation.history_ix,
                reader.navigation.history.len(),
                reader.tree.can_reveal(reader.selected_file()),
            ),
            labels: reader_ui_state::toolbar_labels(cx),
            full_width: reader_ui_state::full_note_width(cx),
            editable: reader.code_file_editable(cx),
            renaming: reader
                .renaming
                .as_ref()
                .filter(|rename| rename.in_header)
                .map(|rename| (rename.input.entity_id(), rename.error.clone())),
            actions: chrome
                .actions
                .iter()
                .chain(chrome.views.iter().map(|view| &view.action))
                .chain(chrome.state.iter())
                .chain(chrome.badges.iter())
                .map(ActionKey::from)
                .collect(),
            views: chrome.views.iter().map(|view| view.active).collect(),
            compact_views: chrome.compact_views,
            find: chrome.find.as_ref().map(|find| FindKey {
                input: find.input.entity_id(),
                scope: find.scope.clone(),
                large_file: find.large_file,
            }),
            action_text: chrome.action_text.clone(),
            action_text_after: chrome.action_text_after,
            deleted: reader
                .deleted_current()
                .is_some_and(|deleted| deleted.disk == reader_deleted_note::Disk::Missing),
        }
    }
}

pub(crate) struct Header {
    reader: WeakEntity<Reader>,
    key: HeaderKey,
}

impl Header {
    fn new(reader: Entity<Reader>, key: HeaderKey) -> Self {
        Self {
            reader: reader.downgrade(),
            key,
        }
    }
}

impl Render for Header {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.reader
            .update(cx, |reader, cx| {
                if reader.file_preview.is_some() {
                    reader.render_file_header(window, cx).into_any_element()
                } else {
                    div().into_any_element()
                }
            })
            .unwrap_or_else(|_| div().into_any_element())
    }
}

/// Only this view follows clean/dirty transitions. The header slots and their
/// overflow plan stay outside the editor's per-key notification path.
pub(crate) struct SaveControls {
    input: EntityId,
    reader: WeakEntity<Reader>,
    dirty: bool,
    _subscriptions: [Subscription; 2],
}

/// Query, count, case, caret and focus belong to this sibling of Header.
/// Input notifications must never invalidate the cached seven-slot view.
pub(crate) struct FindControls {
    input: EntityId,
    reader: WeakEntity<Reader>,
}

impl Render for FindControls {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(reader) = self.reader.upgrade() else {
            return div().into_any_element();
        };
        let reader = reader.read(cx);
        let Some(target) = reader.viewer_find_target(cx) else {
            return div().into_any_element();
        };
        let Some(bounds) = reader.viewer_find_bounds.get() else {
            return div().into_any_element();
        };
        let width = bounds.size.width.as_f32();
        let max_width = (bounds.right() - reader.viewer_find_min_x.get())
            .as_f32()
            .max(width);
        reader_find_field::render(
            &target,
            reader_find_field::Presentation::Header {
                width,
                focused: target.input.read(cx).focus_handle(cx).is_focused(window),
                max_width,
            },
            cx,
        )
    }
}

impl SaveControls {
    fn synchronize(&mut self, cx: &mut Context<Self>) {
        let dirty = self.reader.upgrade().is_some_and(|reader| {
            let reader = reader.read(cx);
            reader.editing.as_ref().is_some_and(|editing| {
                editing.input().entity_id() == self.input && reader.source_indicator_dirty(cx)
            })
        });
        if self.dirty != dirty {
            self.dirty = dirty;
            cx.notify();
        }
    }
}

impl Render for SaveControls {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reader = self.reader.clone();
        h_flex()
            .flex_none()
            .w(px(44.))
            .h(px(28.))
            .gap_1()
            .when(self.dirty, |row| {
                let status = reader
                    .upgrade()
                    .map(|reader| reader.read(cx).render_save_status(cx));
                row.children(status).child(
                    reader_icon_button(
                        "source-save",
                        Icon::default().path("icons/save.svg"),
                        reader_shortcuts::hint("Save", &SaveSource, cx),
                        cx,
                    )
                    .debug_selector(|| "source-save".into())
                    .on_click(move |_, _, cx| {
                        let _ = reader.update(cx, |reader, cx| reader.request_source_save(cx));
                    }),
                )
            })
            .into_any_element()
    }
}

#[derive(Clone)]
pub(crate) struct ViewerAction {
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
pub(crate) struct LiveSlot {
    pub view: AnyView,
    pub bounds: Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    pub width: f32,
}

impl LiveSlot {
    pub(crate) fn new(view: impl Into<AnyView>, width: f32) -> Self {
        Self {
            view: view.into(),
            bounds: Default::default(),
            width,
        }
    }

    #[inline(never)]
    fn placeholder(&self) -> AnyElement {
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
    fn overlay(self) -> AnyElement {
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
    pub(crate) fn new(
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

    pub(crate) fn with_action<A: Action>(mut self, action: A, cx: &App) -> Self {
        self.tooltip = reader_shortcuts::hint(&self.label, &action, cx);
        self.shortcut = Some(Rc::new(action));
        self
    }

    pub(crate) fn button(&self, collapsed: bool) -> Button {
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

    pub(crate) fn render_button(&self, collapsed: bool) -> AnyElement {
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

    pub(crate) fn menu_item(&self) -> PopupMenuItem {
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

    fn layout_item(&self, labels: bool, window: &Window) -> LayoutItem {
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

    pub(crate) fn append_menu(
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
pub(crate) struct ViewerView {
    pub action: ViewerAction,
    pub active: bool,
}

pub(crate) use crate::reader_find_field::Target as ViewerFindTarget;

#[derive(Clone, Default)]
pub(crate) struct ViewerChrome {
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

struct HeaderLayout {
    items: Vec<LayoutItem>,
    states: Vec<Disposition>,
    widths: Vec<f32>,
    labels: bool,
    editable: bool,
    editing: bool,
    width_applies: bool,
    collapse_parents: bool,
    view_menu: bool,
}

impl HeaderLayout {
    fn shows(&self, id: &str) -> bool {
        self.items
            .iter()
            .zip(&self.states)
            .any(|(item, state)| item.id == id && *state != Disposition::Menu)
    }
    fn width(&self, id: &str) -> f32 {
        self.items
            .iter()
            .zip(&self.widths)
            .find(|(item, _)| item.id == id)
            .map_or(0., |(_, width)| *width)
    }
    fn collapsed(&self, id: &str) -> bool {
        self.items
            .iter()
            .zip(&self.states)
            .any(|(item, state)| item.id == id && *state == Disposition::Collapsed)
    }
}

impl Reader {
    pub(crate) fn invoke_viewer_action(
        &self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chrome = self.viewer_chrome(cx);
        if let Some(action) = chrome
            .actions
            .into_iter()
            .chain(chrome.copy_actions)
            .chain(chrome.open_actions)
            .find(|action| action.id == id && action.enabled)
        {
            (action.invoke)(window, cx);
        }
    }

    #[inline(never)]
    pub(crate) fn render_file_header_viewport(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chrome = self.viewer_header_chrome(cx);
        let live: Vec<_> = chrome
            .actions
            .iter()
            .chain(chrome.state.iter())
            .chain(chrome.badges.iter())
            .filter_map(|action| action.live.clone())
            .collect();
        let find = chrome.find.map(|target| {
            let mut cached = self.viewer_find_controls.borrow_mut();
            if cached
                .as_ref()
                .is_none_or(|view| view.read(cx).input != target.input.entity_id())
            {
                let reader = cx.entity().downgrade();
                *cached = Some(cx.new(|_| FindControls {
                    input: target.input.entity_id(),
                    reader,
                }));
            }
            cached.as_ref().unwrap().clone()
        });
        if find.is_none() {
            self.viewer_find_controls.borrow_mut().take();
            self.viewer_find_bounds.set(None);
        }
        let save = if self.editing.is_some() {
            Some(self.file_save_controls(cx))
        } else {
            self.viewer_save_controls.borrow_mut().take();
            self.viewer_save_bounds.set(None);
            None
        };
        let reader = cx.entity();
        let key = HeaderKey::new(self, window, cx);
        let mut cached = self.viewer_header.borrow_mut();
        if cached
            .as_ref()
            .is_none_or(|header| header.read(cx).key != key)
        {
            // Notifying a view while GPUI is already drawing can be deferred
            // beyond this frame. A new identity applies a structural change
            // immediately, while ordinary input retains the cached identity.
            *cached = Some(cx.new(|_| Header::new(reader, key)));
        }
        let header = cached.as_ref().unwrap().clone();
        drop(cached);
        let header = header.cached(
            StyleRefinement::default()
                .w_full()
                .h(px(48.))
                .flex_shrink_0(),
        );
        div()
            .debug_selector(|| "document-header-viewport".into())
            .relative()
            .flex_none()
            .overflow_hidden()
            .h(px(48.))
            .child(header)
            .children(live.into_iter().map(LiveSlot::overlay))
            .children(find.map(|find| {
                let bounds = self.viewer_find_bounds.clone();
                canvas(
                    move |_, window, cx| {
                        bounds.get().map(|bounds| {
                            let mut element = find.into_any_element();
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
            }))
            // GPUI invalidates cached ancestors of notified child views. Save
            // therefore occupies its measured slot as a sibling overlay.
            .children(save.map(|save| {
                let bounds = self.viewer_save_bounds.clone();
                canvas(
                    move |_, window, cx| {
                        bounds.get().map(|bounds| {
                            let mut element = save.into_any_element();
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
            }))
            .into_any_element()
    }
    pub(super) fn viewer_find_target(&self, cx: &App) -> Option<ViewerFindTarget> {
        if let Some(target) = self.file_editor_find_target(cx) {
            return Some(target);
        }
        if let Some(code) = self
            .file_preview
            .as_ref()
            .filter(|_| self.editing.is_none())
            .and_then(|p| p.code.as_ref())
        {
            return code
                .read(cx)
                .header_chrome(code.clone(), cx)
                .find
                .map(|_| code.read(cx).find_target(code.clone(), cx));
        }
        self.viewer_chrome(cx).find
    }
    fn viewer_header_chrome(&self, cx: &App) -> ViewerChrome {
        if let Some(chrome) = self.file_editor_chrome(cx, false) {
            return chrome;
        }
        if let Some(code) = self
            .file_preview
            .as_ref()
            .filter(|_| self.editing.is_none())
            .and_then(|p| p.code.as_ref())
        {
            return code.read(cx).header_chrome(code.clone(), cx);
        }
        self.viewer_chrome(cx)
    }
    /// Only this adapter knows where the active viewer lives. Slot builders
    /// render the resulting description without matching on a viewer kind.
    pub(crate) fn viewer_chrome(&self, cx: &App) -> ViewerChrome {
        if let Some(chrome) = self.file_editor_chrome(cx, true) {
            return chrome;
        }
        let Some(preview) = &self.file_preview else {
            return ViewerChrome::default();
        };
        if let Some(log) = &preview.log {
            return reader_log::chrome::describe(log, cx);
        }
        if let Some(code) = &preview.code {
            return if self.editing.is_none() {
                code.read(cx).chrome(code.clone(), cx)
            } else {
                ViewerChrome::default()
            };
        }
        if let Some(pdf) = &preview.pdf {
            return reader_pdf::chrome(pdf, cx);
        }
        if let Some(archive) = &preview.archive {
            return reader_archive::chrome(archive, cx);
        }
        if let Some(canvas) = &preview.canvas {
            return reader_canvas::chrome(canvas, cx);
        }
        if let Some(drawing) = &preview.drawing {
            return reader_drawing::chrome(drawing.read(cx), cx);
        }
        if preview.image {
            let mut action = ViewerAction::new(
                "copy-viewer-image",
                Icon::new(IconName::Copy),
                "Copy image",
                Rc::new(|window, cx| window.dispatch_action(Box::new(CopyViewerImage), cx)),
            );
            action.shortcut = Some(Rc::new(CopyViewerImage));
            return ViewerChrome {
                copy_actions: vec![action],
                ..Default::default()
            };
        }
        ViewerChrome::default()
    }

    #[inline(never)]
    pub(super) fn render_file_header(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        #[cfg(test)]
        HEADER_RENDERS.with(|counts| {
            *counts.borrow_mut().entry(cx.entity_id()).or_default() += 1;
        });
        let available = if self.body_bounds.size.width > px(0.) {
            self.body_bounds.size.width.as_f32()
        } else {
            window.viewport_size().width.as_f32()
        };
        let widths = self.panels.widths(&self.panel_widths, available);
        let width = available
            - if reader_layout::overlay(available) {
                0.
            } else {
                widths.notes + widths.backlinks
            };
        let chrome = self.viewer_header_chrome(cx);
        let plan = self.header_layout(&chrome, width, window, cx);
        for action in chrome
            .actions
            .iter()
            .chain(chrome.state.iter())
            .chain(chrome.badges.iter())
        {
            if let Some(slot) = &action.live {
                slot.bounds.set(None);
            }
        }
        // A test observes the values actually handed to the slot builders,
        // not a fresh description that could hide an incorrectly cached Header.
        #[cfg(test)]
        RENDERED_ACTIONS.with(|states| {
            states.borrow_mut().insert(
                cx.entity_id(),
                chrome
                    .actions
                    .iter()
                    .map(|action| RenderedAction {
                        id: action.id,
                        label: action.label.clone(),
                        filled: action.filled,
                        tooltip: action.tooltip.clone(),
                    })
                    .collect(),
            );
        });
        h_flex()
            .id("document-header")
            .debug_selector(|| "document-header".into())
            .flex_none()
            .h(px(48.))
            .w_full()
            .min_w_0()
            .px_4()
            .gap_1()
            .when(plan.view_menu, |row| row.px_3().gap(px(2.)))
            .overflow_hidden()
            .child(navigation_slot(self, &plan, cx))
            .child(name_slot(self, &chrome, &plan, cx))
            .child(find_slot(self, &chrome, &plan))
            .child(views_slot(&chrome, &plan, cx))
            .child(viewer_actions_slot(&chrome, &plan, cx))
            .child(file_actions_slot(self, &plan, cx))
            .child(window_slot(self, &chrome, &plan, cx))
    }

    fn header_layout(
        &self,
        chrome: &ViewerChrome,
        width: f32,
        window: &Window,
        cx: &App,
    ) -> HeaderLayout {
        let editing = self.editing.is_some();
        let editable = self
            .file_preview
            .as_ref()
            .is_some_and(|p| reader_delimited::editable(&p.rel))
            && self.code_file_editable(cx);
        let width_applies = self
            .file_preview
            .as_ref()
            .is_some_and(|p| p.log.is_none() && (p.code.is_some() || p.text.is_some()));
        let labels = reader_ui_state::toolbar_labels(cx) && width >= 1400.;
        let order = chrome.overflow_order;
        let mut items = vec![LayoutItem::optional("reveal", 28., 28., order.reveal)];
        let view_menu = chrome.compact_views && width < 900.;
        if view_menu && chrome.views.len() > 1 {
            items.push(LayoutItem::pinned("views", 68., 68.));
        } else {
            items.extend(reader_segmented::layout(&chrome.views, window));
        }
        items.extend(
            chrome
                .actions
                .iter()
                .map(|action| action.layout_item(labels, window)),
        );
        if chrome.action_text.is_some() {
            items.push(LayoutItem::optional(
                "viewer-action-text",
                44.,
                44.,
                order.viewer_text,
            ));
        }
        if let Some(find) = &chrome.find {
            let mut item =
                LayoutItem::pinned("find", 200., if find.large_file { 128. } else { 28. });
            item.steps = vec![overflow::OverflowStep {
                priority: order.find_narrow,
                width: 128.,
                disposition: Disposition::Collapsed,
            }];
            if !find.large_file {
                item.steps.push(overflow::OverflowStep {
                    priority: order.find_icon,
                    width: 28.,
                    disposition: Disposition::Collapsed,
                });
            }
            items.push(item);
        }
        if !chrome.copy_in_menu {
            items.push(LayoutItem::optional(
                "copy",
                if labels { 90. } else { 40. },
                40.,
                order.copy,
            ));
        }
        if editable {
            items.push(LayoutItem::pinned(
                "edit",
                if labels { 74. } else { 28. },
                28.,
            ));
        }
        if editing {
            items.push(LayoutItem::pinned("save", 44., 44.));
        }
        items.push(LayoutItem::optional(
            "open-with",
            if labels { 120. } else { 40. },
            40.,
            order.open_with,
        ));
        if width_applies {
            items.push(LayoutItem::optional("width", 28., 28., order.width));
        }
        items.push(LayoutItem::optional("maximize", 28., 28., order.maximize));
        items.push(LayoutItem::pinned("more", 28., 28.));
        // Padding + navigation + group separators. The title owns the remaining
        // space, with 120 px protected before any optional control is admitted.
        let title_minimum =
            136. + if self.selected_file().contains('/') {
                32.
            } else {
                0.
            } + if chrome.state.is_some() { 32. } else { 0. }
                + 32. * chrome.badges.len() as f32;
        // Optional controls yield before the name truncates. Once they have
        // yielded, the flex name slot may shrink down to its protected minimum.
        let title_budget = title_minimum
            + (toolbar_text_width(&self.selected_title(), FontWeight::MEDIUM, window) - 120.)
                .max(0.);
        let (states, widths) = layout_with_widths(width, 128., title_budget, &items);
        let parent_width = self
            .selected_file()
            .rsplit_once('/')
            .map_or(0., |(dir, _)| {
                dir.split('/')
                    .map(|part| toolbar_text_width(part, FontWeight::NORMAL, window) + 24.)
                    .sum::<f32>()
            });
        let compact_parent = if parent_width > 0. { 32. } else { 0. };
        let controls: f32 = states
            .iter()
            .zip(&widths)
            .map(|(state, width)| {
                if *state == Disposition::Menu {
                    0.
                } else {
                    width + 4.
                }
            })
            .sum();
        let title_width = toolbar_text_width(&self.selected_title(), FontWeight::MEDIUM, window);
        let spare = width
            - 128.
            - controls
            - title_width
            - 16.
            - compact_parent
            - if chrome.state.is_some() { 32. } else { 0. }
            - 32. * chrome.badges.len() as f32;
        let collapse_parents = parent_width > compact_parent + spare.max(0.);

        HeaderLayout {
            items,
            states,
            widths,
            labels,
            editable,
            editing,
            width_applies,
            collapse_parents,
            view_menu,
        }
    }

    fn file_save_controls(&self, cx: &mut Context<Self>) -> Entity<SaveControls> {
        let input = self.editing.as_ref().unwrap().input().clone();
        let mut cached = self.viewer_save_controls.borrow_mut();
        if let Some(controls) = cached
            .as_ref()
            .filter(|controls| controls.read(cx).input == input.entity_id())
        {
            return controls.clone();
        }
        let reader = cx.entity();
        let dirty = self.source_indicator_dirty(cx);
        let controls = cx.new(|cx: &mut Context<SaveControls>| SaveControls {
            input: input.entity_id(),
            reader: reader.downgrade(),
            dirty,
            _subscriptions: [
                cx.observe(&input, |this, _, cx| this.synchronize(cx)),
                cx.observe(&reader, |this, _, cx| this.synchronize(cx)),
            ],
        });
        *cached = Some(controls.clone());
        controls
    }
}

#[inline(never)]
fn navigation_slot(reader: &Reader, plan: &HeaderLayout, cx: &mut Context<Reader>) -> AnyElement {
    let min_x = reader.viewer_find_min_x.clone();
    h_flex()
        .relative()
        .flex_none()
        .gap_1()
        .child(
            canvas(
                move |bounds, _, _| min_x.set(bounds.right() + px(4.)),
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .child(
            reader_icon_button(
                "reader-history-back",
                IconName::ArrowLeft,
                reader_shortcuts::hint("Back", &HistoryBack, cx),
                cx,
            )
            .debug_selector(|| "reader-history-back".into())
            .disabled(reader.navigation.history_ix == 0)
            .on_click(cx.listener(|r, _, w, cx| r.history_move(-1, w, cx))),
        )
        .child(
            reader_icon_button(
                "reader-history-forward",
                IconName::ArrowRight,
                reader_shortcuts::hint("Forward", &HistoryForward, cx),
                cx,
            )
            .debug_selector(|| "reader-history-forward".into())
            .disabled(reader.navigation.history_ix + 1 >= reader.navigation.history.len())
            .on_click(cx.listener(|r, _, w, cx| r.history_move(1, w, cx))),
        )
        .when(plan.shows("reveal"), |row| {
            row.child(
                reader_icon_button(
                    "reader-reveal-current",
                    Icon::default().path(brand::READER_FOCUS_ICON),
                    reader_shortcuts::hint("Reveal in sidebar", &FocusCurrentFolder, cx),
                    cx,
                )
                .debug_selector(|| "reader-reveal-current".into())
                .disabled(!reader.tree.can_reveal(reader.selected_file()))
                .on_click(cx.listener(|r, _, w, cx| r.focus_current_folder(w, cx))),
            )
        })
        .into_any_element()
}

#[inline(never)]
fn name_slot(
    reader: &Reader,
    chrome: &ViewerChrome,
    plan: &HeaderLayout,
    cx: &mut Context<Reader>,
) -> AnyElement {
    let nested = reader.selected_file().contains('/');
    h_flex()
        .debug_selector(|| "viewer-name-slot".into())
        .flex_1()
        .min_w(px(136.
            + if nested { 32. } else { 0. }
            + if chrome.state.is_some() { 32. } else { 0. }
            + 32. * chrome.badges.len() as f32))
        .overflow_hidden()
        .child(reader.render_breadcrumbs_with_state(
            plan.collapse_parents,
            chrome.state.as_ref().map(|state| {
                h_flex()
                    .gap_1()
                    .child(state.render_button(true))
                    .children(chrome.badges.iter().map(|badge| badge.render_button(true)))
                    .into_any_element()
            }),
            cx,
        ))
        .into_any_element()
}

#[inline(never)]
fn find_slot(reader: &Reader, chrome: &ViewerChrome, plan: &HeaderLayout) -> AnyElement {
    let Some(_) = &chrome.find else {
        return div().into_any_element();
    };
    let bounds = reader.viewer_find_bounds.clone();
    div()
        .relative()
        .flex_none()
        .w(px(plan.width("find")))
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
fn views_slot(chrome: &ViewerChrome, plan: &HeaderLayout, cx: &App) -> AnyElement {
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

fn separator(cx: &App) -> AnyElement {
    div()
        .flex_none()
        .w(px(1.))
        .h(px(16.))
        .mx_1()
        .bg(brand::palette(cx).border_subtle)
        .into_any_element()
}

#[inline(never)]
fn viewer_actions_slot(chrome: &ViewerChrome, plan: &HeaderLayout, cx: &App) -> AnyElement {
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

fn menu_button(
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

#[inline(never)]
fn file_actions_slot(reader: &Reader, plan: &HeaderLayout, cx: &mut Context<Reader>) -> AnyElement {
    let weak = cx.entity().downgrade();
    let copy_weak = weak.clone();
    let open_weak = weak.clone();
    let root = reader.vault_root.clone();
    let rel = reader.selected_file().to_owned();
    let text = copy_content_available(reader);
    let mut row = h_flex().flex_none().gap_1();
    if plan.shows("copy") {
        row = row.child(
            menu_button(
                "viewer-copy",
                IconName::Copy,
                "Copy",
                plan.labels && !plan.collapsed("copy"),
                cx,
            )
            .dropdown_menu_with_anchor(Anchor::TopRight, move |m, _, cx| {
                copy_menu(m, copy_weak.clone(), text, cx)
            }),
        );
    }
    row = row.child(separator(cx));
    if plan.editable {
        let (id, label, icon) = if plan.editing {
            ("reader-read", "Read", Icon::new(IconName::BookOpen))
        } else {
            (
                "reader-edit",
                "Edit",
                Icon::default().path("icons/pencil.svg"),
            )
        };
        row = row.child(
            reader_icon_button(
                id,
                icon,
                reader_shortcuts::hint(label, &ToggleSource, cx),
                cx,
            )
            .small()
            .when(plan.labels && !plan.collapsed("edit"), |b| {
                b.w_auto().label(label)
            })
            .accessibility_label(label)
            .debug_selector(move || id.into())
            .on_click(cx.listener(|r, _, w, cx| r.toggle_source(w, cx))),
        );
    }
    if plan.editing {
        let bounds = reader.viewer_save_bounds.clone();
        row = row.child(
            div().relative().flex_none().w(px(44.)).h(px(28.)).child(
                canvas(
                    move |measured, _, _| bounds.set(Some(measured)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            ),
        );
    }
    if plan.shows("open-with") {
        row = row.child(
            menu_button(
                "viewer-open-with",
                IconName::ExternalLink,
                "Open with",
                plan.labels && !plan.collapsed("open-with"),
                cx,
            )
            .dropdown_menu_with_anchor(Anchor::TopRight, move |m, w, cx| {
                open_menu(m, open_weak.clone(), root.clone(), rel.clone(), w, cx)
            }),
        );
    }
    row.into_any_element()
}

pub(crate) fn copy_content_available(reader: &Reader) -> bool {
    reader.file_preview.as_ref().is_some_and(|p| {
        p.code.is_some() || p.table.is_some() || p.text.is_some() || p.log.is_some()
    })
}

fn copy_menu(mut menu: PopupMenu, reader: WeakEntity<Reader>, text: bool, cx: &App) -> PopupMenu {
    if let Some(target) = reader.upgrade() {
        for action in target.read(cx).viewer_chrome(cx).copy_actions {
            menu = menu.item(action.menu_item());
        }
    }
    if text {
        let target = reader.clone();
        menu = menu
            .item(
                PopupMenuItem::new("Copy file content")
                    .action(Box::new(CopyFileContent))
                    .on_click(move |_, w, cx| {
                        let _ = target.update(cx, |r, cx| r.copy_file_content(w, cx));
                    }),
            )
            .separator();
    }
    for (label, action) in [
        ("Copy path", reader_files::FileAction::Absolute),
        ("Copy vault path", reader_files::FileAction::Relative),
        ("Copy wikilink", reader_files::FileAction::Wiki),
    ] {
        let reader = reader.clone();
        let command: Box<dyn Action> = match action {
            reader_files::FileAction::Absolute => Box::new(CopyFilePath),
            reader_files::FileAction::Relative => Box::new(CopyVaultPath),
            _ => Box::new(CopyFileWikilink),
        };
        menu = menu.item(
            PopupMenuItem::new(label)
                .action(command)
                .on_click(move |_, w, cx| {
                    let _ = reader.update(cx, |r, cx| {
                        reader_files::run(action, &r.vault_root, r.selected_file(), w, cx)
                    });
                }),
        );
    }
    menu
}

fn open_commands() -> Vec<(&'static str, reader_files::FileAction, Box<dyn Action>)> {
    let commands: Vec<(&'static str, reader_files::FileAction, Box<dyn Action>)> = vec![
        (
            "Open with default app",
            reader_files::FileAction::Open,
            Box::new(OpenFileDefault),
        ),
        (
            platform::labels::Os::CURRENT.reveal(),
            reader_files::FileAction::Reveal,
            Box::new(RevealFile),
        ),
        (
            reader_files::TERMINAL_LABEL,
            reader_files::FileAction::Terminal,
            Box::new(project_terminal::OpenInTerminal),
        ),
    ];
    #[cfg(target_os = "macos")]
    let commands = {
        let mut commands = commands;
        commands.push((
            "Quick Look",
            reader_files::FileAction::QuickLook,
            Box::new(QuickLookFile),
        ));
        commands
    };
    commands
}

fn open_menu(
    mut menu: PopupMenu,
    reader: WeakEntity<Reader>,
    root: PathBuf,
    rel: String,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    #[cfg(test)]
    let mut rendered_labels = Vec::new();
    if let Some(target) = reader.upgrade() {
        for action in target.read(cx).viewer_chrome(cx).open_actions {
            #[cfg(test)]
            rendered_labels.push(action.label.clone());
            menu = menu.item(action.menu_item());
        }
    }
    for (index, (label, action, command)) in open_commands().into_iter().enumerate() {
        if index == 1 {
            menu = open_in::submenu(menu, root.clone(), rel.clone(), window, cx);
        }
        let target = reader.clone();
        let item = PopupMenuItem::new(label)
            .action(command)
            .on_click(move |_, w, cx| {
                let _ = target.update(cx, |r, cx| {
                    reader_files::run(action, &r.vault_root, r.selected_file(), w, cx)
                });
            });
        #[cfg(test)]
        if let PopupMenuItem::Item { label, .. } = &item {
            rendered_labels.push(label.clone());
        }
        menu = menu.item(item);
    }
    #[cfg(test)]
    OPEN_WITH_ITEMS
        .with(|record| *record.borrow_mut() = Some((cx.entity().downgrade(), rendered_labels)));
    menu
}

fn file_width_label(cx: &App) -> &'static str {
    if reader_ui_state::full_note_width(cx) {
        "Normal width"
    } else {
        "Full width"
    }
}

#[inline(never)]
fn window_slot(
    reader: &Reader,
    chrome: &ViewerChrome,
    plan: &HeaderLayout,
    cx: &mut Context<Reader>,
) -> AnyElement {
    let mut row = h_flex().flex_none().gap_1().child(separator(cx));
    if plan.shows("width") {
        row = row.child(
            reader_icon_button(
                "note-width",
                Icon::default().path(if reader_ui_state::full_note_width(cx) {
                    brand::NOTE_WIDTH_NORMAL_ICON
                } else {
                    brand::NOTE_WIDTH_FULL_ICON
                }),
                reader_shortcuts::hint(file_width_label(cx), &ToggleNoteWidth, cx),
                cx,
            )
            .debug_selector(|| "note-width".into())
            .selected(reader_ui_state::full_note_width(cx))
            .on_click(|_, _, cx| toggle_note_width(cx)),
        );
    }
    if plan.shows("maximize") {
        row = row.child(preserve_reader_selection(
            "reader-maximize-note-preserve",
            reader_icon_button(
                "reader-maximize-note",
                IconName::Maximize,
                reader_shortcuts::hint(
                    if reader.panels.maximized() {
                        "Restore panels"
                    } else {
                        "Maximize"
                    },
                    &MaximizeNote,
                    cx,
                ),
                cx,
            )
            .debug_selector(|| "reader-maximize-note".into())
            .selected(reader.panels.maximized())
            .on_click(cx.listener(|r, _, w, cx| r.maximize_note(w, cx))),
        ));
    }
    let weak = cx.entity().downgrade();
    let root = reader.vault_root.clone();
    let rel = reader.selected_file().to_owned();
    let text = copy_content_available(reader);
    let width_applies = plan.width_applies;
    let hidden_width = width_applies && !plan.shows("width");
    let hidden_maximize = !plan.shows("maximize");
    let hidden_copy = !plan.shows("copy");
    let hidden_open = !plan.shows("open-with");
    let overflow: Vec<_> = chrome
        .actions
        .iter()
        .filter(|a| !plan.shows(a.id))
        .cloned()
        .collect();
    let table = reader
        .file_preview
        .as_ref()
        .filter(|p| p.table.is_some())
        .map(|p| p.path.clone());
    row.child(
        reader_icon_button("document-more", IconName::Ellipsis, "Document actions", cx)
            .debug_selector(|| "document-more".into())
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut m, w, cx| {
                m = m.menu_with_icon(
                    "Reveal in sidebar",
                    Icon::default().path(brand::READER_FOCUS_ICON),
                    Box::new(FocusCurrentFolder),
                );
                if hidden_width {
                    m = m.menu(file_width_label(cx), Box::new(ToggleNoteWidth));
                }
                if hidden_maximize {
                    m = m.menu("Maximize / restore", Box::new(MaximizeNote));
                }
                for action in &overflow {
                    m = action.append_menu(m, w, cx);
                }
                if let Some(path) = table.clone() {
                    let no_header = !reader_ui_state::delimited_header(&path, cx);
                    m = m.item(
                        PopupMenuItem::new("No header row")
                            .checked(no_header)
                            .on_click(move |_, _, cx| {
                                reader_ui_state::set_delimited_header(path.clone(), no_header, cx)
                            }),
                    );
                }
                if hidden_copy {
                    let target = weak.clone();
                    m = m.submenu("Copy", w, cx, move |m, _, cx| {
                        copy_menu(m, target.clone(), text, cx)
                    });
                }
                if hidden_open {
                    let target = weak.clone();
                    let root = root.clone();
                    let rel = rel.clone();
                    m = m.submenu("Open with", w, cx, move |m, w, cx| {
                        open_menu(m, target.clone(), root.clone(), rel.clone(), w, cx)
                    });
                }
                if width_applies {
                    m = m
                        .menu_with_check(
                            "Wrap long lines",
                            reader_ui_state::code_soft_wrap(cx),
                            Box::new(ToggleSoftWrap),
                        )
                        .menu("Zoom in", Box::new(reader_text_zoom::ZoomIn))
                        .menu("Zoom out", Box::new(reader_text_zoom::ZoomOut))
                        .menu("Actual size", Box::new(reader_text_zoom::Reset));
                }
                let target = weak.clone();
                let rel = rel.clone();
                m.separator()
                    .item(
                        PopupMenuItem::new(platform::labels::Os::CURRENT.move_to_trash())
                            .action(Box::new(DeleteNote))
                            .on_click(move |_, w, cx| {
                                let _ =
                                    target.update(cx, |r, cx| r.delete_path(rel.clone(), w, cx));
                            }),
                    )
                    .separator()
                    .menu("Close file", Box::new(CloseNote))
            }),
    )
    .into_any_element()
}

/// Hosts without a document header reserve Find and draw the same erased action
/// slot and overflow menus. They do not invent another set of log controls.
#[inline(never)]
pub(crate) fn render_compact_header(
    chrome: &ViewerChrome,
    width: f32,
    window: &Window,
    cx: &App,
) -> AnyElement {
    render_compact_header_with_leading(chrome, width, None, window, cx)
}

#[inline(never)]
pub(crate) fn render_compact_header_with_leading(
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
