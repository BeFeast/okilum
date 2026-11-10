//! File-viewer descriptions and the shared seven-slot header. Every slot is
//! erased at its boundary: the native Windows main stack must stay bounded.
use super::*;
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use std::rc::Rc;

mod overflow;
#[cfg(test)]
mod tests;
pub(crate) use overflow::{layout, Disposition, LayoutItem};

pub(crate) type Invoke = Rc<dyn Fn(&mut Window, &mut App)>;

#[cfg(test)]
thread_local! {
    static HEADER_RENDERS: std::cell::RefCell<std::collections::HashMap<EntityId, usize>> = Default::default();
}

/// Only this view follows clean/dirty transitions. The header slots and their
/// overflow plan stay outside the editor's per-key notification path.
pub(crate) struct SaveControls {
    input: EntityId,
    reader: WeakEntity<Reader>,
    dirty: bool,
    _subscriptions: [Subscription; 2],
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
            .w(px(60.))
            .gap_1()
            .when(self.dirty, |row| {
                let status = reader
                    .upgrade()
                    .map(|reader| reader.update(cx, |reader, cx| reader.render_save_status(cx)));
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
}

impl ViewerAction {
    pub(crate) fn button(&self, collapsed: bool) -> Button {
        let invoke = self.invoke.clone();
        Button::new(self.id)
            .ghost()
            .when(self.filled, |b| b.primary())
            .icon(self.icon.clone())
            .h(px(28.))
            .rounded(px(6.))
            .when(collapsed || !self.labelled, |b| b.w(px(28.)))
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
            .on_click(move |_, window, cx| invoke(window, cx))
    }

    pub(crate) fn menu_item(&self) -> PopupMenuItem {
        let invoke = self.invoke.clone();
        let mut item = PopupMenuItem::new(self.label.clone())
            .icon(self.icon.clone())
            .checked(self.toggled)
            .disabled(!self.enabled)
            .on_click(move |_, window, cx| invoke(window, cx));
        if let Some(shortcut) = &self.shortcut {
            item = item.action(shortcut.boxed_clone());
        }
        item
    }
}

#[derive(Clone)]
pub(crate) struct ViewerView {
    pub action: ViewerAction,
    pub active: bool,
}

/// Viewers opt into Find through a description, never through header routing.
/// The shared field and adapters fill this contract in the next step.
#[derive(Clone)]
pub(crate) struct ViewerFindTarget {
    pub scope: SharedString,
    pub focus: Invoke,
    pub large_file: bool,
}

#[derive(Clone, Default)]
pub(crate) struct ViewerChrome {
    pub views: Vec<ViewerView>,
    pub find: Option<ViewerFindTarget>,
    pub actions: Vec<ViewerAction>,
    pub status: Option<reader_status_line::StatusLine>,
    pub state: Option<ViewerAction>,
}

struct HeaderLayout {
    items: Vec<LayoutItem>,
    states: Vec<Disposition>,
    labels: bool,
    editable: bool,
    editing: bool,
    width_applies: bool,
}

impl HeaderLayout {
    fn shows(&self, id: &str) -> bool {
        self.items
            .iter()
            .zip(&self.states)
            .any(|(item, state)| item.id == id && *state != Disposition::Menu)
    }
    fn collapsed(&self, id: &str) -> bool {
        self.items
            .iter()
            .zip(&self.states)
            .any(|(item, state)| item.id == id && *state == Disposition::Collapsed)
    }
}

impl Reader {
    #[inline(never)]
    pub(crate) fn render_file_header_viewport(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .debug_selector(|| "document-header-viewport".into())
            .flex_none()
            .overflow_hidden()
            .h(px(48.))
            .child(self.render_document_header(window, cx))
            .into_any_element()
    }
    /// Only this adapter knows where the active viewer lives. Slot builders
    /// render the resulting description without matching on a viewer kind.
    pub(crate) fn viewer_chrome(&self, cx: &App) -> ViewerChrome {
        self.file_preview
            .as_ref()
            .and_then(|p| p.pdf.as_ref())
            .map(|pdf| reader_pdf::chrome(pdf, cx))
            .unwrap_or_default()
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
        let chrome = self.viewer_chrome(cx);
        let editing = self.editing.is_some();
        let editable = self
            .file_preview
            .as_ref()
            .is_some_and(|p| reader_delimited::editable(&p.rel))
            && self.code_file_editable(cx);
        let width_applies = self
            .file_preview
            .as_ref()
            .is_some_and(|p| p.code.is_some() || p.text.is_some());
        let labels = reader_ui_state::toolbar_labels(cx) && width >= 1400.;
        let mut items = vec![LayoutItem::optional("reveal", 28., 28., 10)];
        for action in &chrome.actions {
            let wide = if (labels || action.label_by_default) && action.labelled {
                toolbar_text_width(&action.label, FontWeight::MEDIUM, window) + 44.
            } else {
                28.
            };
            let mut item =
                LayoutItem::action(action.id, wide, 28., action.priority, action.never_dropped);
            for step in &mut item.steps {
                if step.disposition == Disposition::Collapsed {
                    step.priority = action.collapse_priority;
                }
            }
            items.push(item);
        }
        items.push(LayoutItem::optional(
            "copy",
            if labels { 90. } else { 40. },
            40.,
            50,
        ));
        if editable {
            items.push(LayoutItem::pinned(
                "edit",
                if labels { 74. } else { 28. },
                28.,
            ));
        }
        if editing {
            items.push(LayoutItem::pinned("save", 60., 60.));
        }
        if editing {
            items.push(LayoutItem::pinned("find-note", 28., 28.));
        }
        items.push(LayoutItem::optional(
            "open-with",
            if labels { 120. } else { 40. },
            40.,
            40,
        ));
        if width_applies {
            items.push(LayoutItem::optional("width", 28., 28., 20));
        }
        items.push(LayoutItem::optional("maximize", 28., 28., 21));
        items.push(LayoutItem::pinned("more", 28., 28.));
        // Padding + navigation + group separators. The title owns the remaining
        // space, with 120 px protected before any optional control is admitted.
        let title_minimum =
            136. + if self.selected_file().contains('/') {
                32.
            } else {
                0.
            } + if chrome.state.is_some() { 32. } else { 0. };
        // Optional controls yield before the name truncates. Once they have
        // yielded, the flex name slot may shrink down to its protected minimum.
        let title_budget = title_minimum
            + (toolbar_text_width(&self.selected_title(), FontWeight::MEDIUM, window) - 120.)
                .max(0.);
        let states = layout(width, 128., title_budget, &items);
        let plan = HeaderLayout {
            items,
            states,
            labels,
            editable,
            editing,
            width_applies,
        };
        h_flex()
            .id("document-header")
            .debug_selector(|| "document-header".into())
            .flex_none()
            .h(px(48.))
            .w_full()
            .min_w_0()
            .px_4()
            .gap_1()
            .overflow_hidden()
            .child(navigation_slot(self, &plan, cx))
            .child(name_slot(self, &chrome, cx))
            .child(find_slot(&chrome))
            .child(views_slot(&chrome))
            .child(viewer_actions_slot(&chrome, &plan, cx))
            .child(file_actions_slot(self, &plan, cx))
            .child(window_slot(self, &chrome, &plan, cx))
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
    h_flex()
        .flex_none()
        .gap_1()
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
fn name_slot(reader: &Reader, chrome: &ViewerChrome, cx: &mut Context<Reader>) -> AnyElement {
    let nested = reader.selected_file().contains('/');
    h_flex()
        .debug_selector(|| "viewer-name-slot".into())
        .flex_1()
        .min_w(px(136.
            + if nested { 32. } else { 0. }
            + if chrome.state.is_some() { 32. } else { 0. }))
        .overflow_hidden()
        .child(reader.render_breadcrumbs(nested, cx))
        .children(chrome.state.as_ref().map(|state| state.button(true)))
        .into_any_element()
}

#[inline(never)]
fn find_slot(chrome: &ViewerChrome) -> AnyElement {
    let Some(target) = &chrome.find else {
        return div().into_any_element();
    };
    let focus = target.focus.clone();
    h_flex()
        .flex_none()
        .min_w(px(if target.large_file { 128. } else { 28. }))
        .child(
            Button::new("viewer-find")
                .ghost()
                .icon(IconName::Search)
                .h(px(28.))
                .label(target.scope.clone())
                .tooltip(target.scope.clone())
                .on_click(move |_, w, cx| focus(w, cx)),
        )
        .into_any_element()
}

#[inline(never)]
fn views_slot(chrome: &ViewerChrome) -> AnyElement {
    h_flex()
        .flex_none()
        .children(
            chrome
                .views
                .iter()
                .map(|view| view.action.button(false).selected(view.active)),
        )
        .into_any_element()
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
    h_flex()
        .flex_none()
        .gap_1()
        .when(!actions.is_empty(), |row| row.child(separator(cx)))
        .children(
            actions
                .into_iter()
                .map(|a| a.button((!plan.labels && !a.label_by_default) || plan.collapsed(a.id))),
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
            .dropdown_menu_with_anchor(Anchor::TopRight, move |m, _, _| {
                copy_menu(m, copy_weak.clone(), text)
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
        row = row.child(reader.file_save_controls(cx));
    } else {
        reader.viewer_save_controls.borrow_mut().take();
    }
    if plan.editing {
        row = row.child(
            reader_icon_button(
                "note-find",
                Icon::default().path("icons/text-search.svg"),
                reader_shortcuts::hint("Find in note", &FindInNote, cx),
                cx,
            )
            .on_click(cx.listener(|r, _, w, cx| r.open_find(w, cx))),
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

fn copy_menu(mut menu: PopupMenu, reader: WeakEntity<Reader>, text: bool) -> PopupMenu {
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
    for (index, (label, action, command)) in open_commands().into_iter().enumerate() {
        if index == 1 {
            menu = open_in::submenu(menu, root.clone(), rel.clone(), window, cx);
        }
        let target = reader.clone();
        menu = menu.item(
            PopupMenuItem::new(label)
                .action(command)
                .on_click(move |_, w, cx| {
                    let _ = target.update(cx, |r, cx| {
                        reader_files::run(action, &r.vault_root, r.selected_file(), w, cx)
                    });
                }),
        );
    }
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
                    m = m.item(action.menu_item());
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
                    m = m.submenu("Copy", w, cx, move |m, _, _| {
                        copy_menu(m, target.clone(), text)
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
