//! Title-bar navigation (#1508, variant A): the project switcher
//! «<project> ▾» (#1519), then Recent ▾ and Pinned ▾ (#1517), then search.
//!
//! Recent and Pinned left the sidebar, which is now only the tree. Inside a
//! project both menus list that project's notes unless its This project /
//! All vault switch, remembered per project, says otherwise. The stores are
//! the vault-wide `reader_sidebar::State` lists; a project is a path prefix.
use super::*;
use std::cell::Cell;

gpui::actions!(
    title_nav,
    [
        NavMenuNext,
        NavMenuPrevious,
        NavMenuCycleNext,
        NavMenuCyclePrevious,
        NavMenuOpen
    ]
);

const MENU_CONTEXT: &str = "TitleNavMenu";
/// Below this window width Recent and Pinned shrink to their glyphs.
const COMPACT_BELOW: f32 = 960.;
const MENU_WIDTH: f32 = 350.;

pub(super) fn bind_menu_keys(cx: &mut App) {
    cx.bind_keys([
        // The open menu owns ⌃Tab, the key its footer names (#1517): without
        // these the Reader-wide recent switcher took it behind the menu.
        KeyBinding::new("ctrl-tab", NavMenuCycleNext, Some(MENU_CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", NavMenuCyclePrevious, Some(MENU_CONTEXT)),
        KeyBinding::new("down", NavMenuNext, Some(MENU_CONTEXT)),
        KeyBinding::new("up", NavMenuPrevious, Some(MENU_CONTEXT)),
        KeyBinding::new("enter", NavMenuOpen, Some(MENU_CONTEXT)),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Menu {
    Recent,
    Pinned,
}

impl Menu {
    fn id(self) -> &'static str {
        match self {
            Menu::Recent => "recent",
            Menu::Pinned => "pinned",
        }
    }
}

/// What Recent and Pinned list inside a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    Project,
    Vault,
}

#[derive(Clone, Copy, Default)]
struct Triggers {
    switcher: Bounds<Pixels>,
    recent: Bounds<Pixels>,
    pinned: Bounds<Pixels>,
}

#[derive(Default)]
pub(super) struct State {
    pub menu: Option<Menu>,
    pub selected: usize,
    pub show_all: bool,
    pub switcher_open: bool,
    switcher_origin: Point<Pixels>,
    /// Painted trigger bounds: menus open below them, and a press on a
    /// trigger is the trigger's click, not an outside press.
    triggers: Rc<Cell<Triggers>>,
    focus: Option<FocusHandle>,
    scroll: ScrollHandle,
}

impl State {
    pub fn close(&mut self) {
        self.menu = None;
        self.switcher_open = false;
        self.show_all = false;
    }

    pub fn on_switcher(&self, position: Point<Pixels>) -> bool {
        self.triggers.get().switcher.contains(&position)
    }

    fn on_trigger(&self, position: Point<Pixels>) -> bool {
        let t = self.triggers.get();
        [t.switcher, t.recent, t.pinned]
            .iter()
            .any(|bounds| bounds.contains(&position))
    }
}

/// One row of the Recent or Pinned menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub path: String,
    pub label: String,
    /// The parent folder's name; none at the scope's root.
    pub location: Option<String>,
    pub age: Option<String>,
    pub folder: bool,
}

/// `path` is `folder` or lies inside it; an empty folder is the whole vault.
pub(super) fn in_folder(path: &str, folder: &str) -> bool {
    folder.is_empty()
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The parent folder's name, shown under the title; nothing for a note at
/// the root of what the menu lists.
pub(super) fn location(path: &str, folder: &str) -> Option<String> {
    let (parent, _) = path.rsplit_once('/')?;
    if parent == folder {
        return None;
    }
    parent.rsplit('/').next().map(str::to_owned)
}

impl Reader {
    /// The active project's folder, vault-relative.
    pub(super) fn project_folder(&self) -> Option<String> {
        let project = self.project_navigation.active.as_ref()?;
        let relative = project.folder.strip_prefix(&self.vault_root).ok()?;
        Some(okilum_core::vault::note_path(relative))
    }

    /// `None` outside a project, where the menus are the whole vault.
    pub(super) fn nav_scope(&self) -> Option<Scope> {
        let folder = self.project_folder()?;
        Some(if self.sidebar.nav_all_vault.contains(&folder) {
            Scope::Vault
        } else {
            Scope::Project
        })
    }

    pub(super) fn set_nav_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        let Some(folder) = self.project_folder() else {
            return;
        };
        let all = &mut self.sidebar.nav_all_vault;
        let changed = match scope {
            Scope::Vault => all.insert(folder),
            Scope::Project => all.remove(&folder),
        };
        if changed {
            self.title_nav.selected = 0;
            self.title_nav.show_all = false;
            self.save_sidebar(cx);
            cx.notify();
        }
    }

    /// Everything the menu lists, before «Show all» trims Recent.
    pub(super) fn nav_entries(&self, menu: Menu, now: u64) -> Vec<Entry> {
        let folder = match self.nav_scope() {
            Some(Scope::Project) => self.project_folder().unwrap_or_default(),
            _ => String::new(),
        };
        // Hidden notes follow Show hidden files, as in the tree (#635).
        let shown = |path: &str| in_folder(path, &folder) && !self.tree.hidden_by_preference(path);
        let entry = |path: &str, age: Option<String>| Entry {
            path: path.to_owned(),
            label: self.note_label(path),
            location: location(path, &folder),
            age,
            folder: self.tree.is_directory(path),
        };
        match menu {
            Menu::Recent => self
                .sidebar
                .recent
                .iter()
                .filter(|(path, _)| shown(path))
                .map(|(path, at)| entry(path, Some(reader_sidebar::age_label(*at, now))))
                .collect(),
            Menu::Pinned => self
                .sidebar
                .pinned
                .iter()
                .filter(|path| shown(path))
                .map(|path| entry(path, None))
                .collect(),
        }
    }

    fn nav_visible(&self, menu: Menu) -> (Vec<Entry>, bool) {
        let mut entries = self.nav_entries(menu, reader_sidebar::now());
        let more = menu == Menu::Recent
            && !self.title_nav.show_all
            && entries.len() > reader_sidebar::RECENT_SHOWN;
        if more {
            entries.truncate(reader_sidebar::RECENT_SHOWN);
        }
        (entries, more)
    }

    pub(super) fn toggle_nav_menu(
        &mut self,
        menu: Menu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.title_nav.menu == Some(menu) {
            self.close_nav_menu(window, cx);
            return;
        }
        self.close_project_switcher(window, cx);
        self.title_nav.menu = Some(menu);
        self.title_nav.selected = 0;
        self.title_nav.show_all = false;
        self.focus_nav_menu(window, cx);
        cx.notify();
    }

    fn focus_nav_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.title_nav
            .focus
            .get_or_insert_with(|| cx.focus_handle())
            .focus(window, cx);
    }

    pub(super) fn close_nav_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.title_nav.menu.take().is_some() {
            self.title_nav.show_all = false;
            self.restore_document_focus(window, cx);
            cx.notify();
        }
    }

    /// Arrows stop at the ends; ⌃Tab cycling (`wrap`) goes round.
    fn move_nav_menu(&mut self, delta: isize, wrap: bool, cx: &mut Context<Self>) {
        let Some(menu) = self.title_nav.menu else {
            return;
        };
        let len = self.nav_visible(menu).0.len();
        if len == 0 {
            return;
        }
        let current = self.title_nav.selected.min(len - 1);
        let next = if wrap {
            (current as isize + delta).rem_euclid(len as isize) as usize
        } else {
            current.saturating_add_signed(delta).min(len - 1)
        };
        self.title_nav.selected = next;
        self.title_nav.scroll.scroll_to_item(next);
        cx.notify();
    }

    fn open_nav_entry(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.title_nav.menu else {
            return;
        };
        let Some(entry) = self.nav_visible(menu).0.into_iter().nth(index) else {
            return;
        };
        self.close_nav_menu(window, cx);
        if entry.folder {
            self.reveal_in_tree(&entry.path, window, cx);
        } else {
            self.open_note(&entry.path, None, window, cx);
        }
    }

    pub(super) fn toggle_project_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.title_nav.switcher_open {
            self.close_project_switcher(window, cx);
            return;
        }
        self.close_nav_menu(window, cx);
        let bounds = self.title_nav.triggers.get().switcher;
        self.title_nav.switcher_origin = point(bounds.left(), bounds.bottom() + px(4.));
        self.title_nav.switcher_open = true;
        self.show_project_switcher(window, cx);
        cx.notify();
    }

    pub(super) fn close_project_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.title_nav.switcher_open) {
            self.release_picker_focus(window, cx);
            cx.notify();
        }
    }

    /// The left group of the title bar: [switcher] Recent ▾ Pinned ▾.
    /// The caller adds the panel toggle before it and search after it.
    pub(super) fn render_title_nav(&self, available: f32, cx: &mut Context<Self>) -> Div {
        let compact = available < COMPACT_BELOW;
        let mut row = header_controls();
        if self.single_file {
            return row;
        }
        let label: SharedString = match &self.project_navigation.active {
            Some(project) => project.name.clone().into(),
            None => vault_name::shown(&self.vault_root, cx).into(),
        };
        let tip = if self.project_navigation.active.is_some() {
            "Switch or leave project"
        } else {
            "Projects"
        };
        row = row.child(
            self.trigger_probe(
                Button::new("title-switcher")
                    .ghost()
                    .label(label)
                    .dropdown_caret(true)
                    .selected(self.title_nav.switcher_open)
                    .h(px(28.))
                    .max_w(px(220.))
                    .overflow_hidden()
                    .rounded(px(6.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .tooltip(tip)
                    .debug_selector(|| "title-switcher".into())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.toggle_project_switcher(window, cx)),
                    ),
                |t, b| t.switcher = b,
            ),
        );
        for (menu, label, icon) in [
            (Menu::Recent, "Recent", brand::READER_CLOCK_ICON),
            (Menu::Pinned, "Pinned", brand::READER_PIN_ICON),
        ] {
            let selector = format!("title-{}", menu.id());
            let button = Button::new(SharedString::from(selector.clone()))
                .ghost()
                .icon(Icon::default().path(icon))
                .when(!compact, |b| b.label(label).dropdown_caret(true))
                .when(compact, |b| b.w(px(28.)))
                .selected(self.title_nav.menu == Some(menu))
                .h(px(28.))
                .rounded(px(6.))
                .tooltip(label)
                .debug_selector(move || selector.clone())
                .on_click(
                    cx.listener(move |this, _, window, cx| this.toggle_nav_menu(menu, window, cx)),
                );
            row = row.child(self.trigger_probe(button, move |t, b| match menu {
                Menu::Recent => t.recent = b,
                Menu::Pinned => t.pinned = b,
            }));
        }
        row
    }

    fn trigger_probe(
        &self,
        control: impl IntoElement,
        record: impl Fn(&mut Triggers, Bounds<Pixels>) + 'static,
    ) -> Div {
        let triggers = self.title_nav.triggers.clone();
        div().relative().flex_none().child(control).child(
            canvas(
                |_, _, _| (),
                move |bounds, _, _, _| {
                    let mut t = triggers.get();
                    record(&mut t, bounds);
                    triggers.set(t);
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
    }

    /// The open switcher popover or menu, mounted over the window.
    pub(super) fn render_title_nav_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.title_nav.switcher_open {
            let picker = self.project_navigation.switcher.clone()?;
            return Some(
                okilum_shell::projects_picker::mount(picker, self.title_nav.switcher_origin)
                    .into_any_element(),
            );
        }
        let menu = self.title_nav.menu?;
        let triggers = self.title_nav.triggers.get();
        let trigger = match menu {
            Menu::Recent => triggers.recent,
            Menu::Pinned => triggers.pinned,
        };
        Some(
            anchored()
                .position(point(trigger.left(), trigger.bottom() + px(4.)))
                .snap_to_window_with_margin(px(12.))
                .child(self.render_nav_menu(menu, cx))
                .into_any_element(),
        )
    }

    fn render_nav_menu(&self, menu: Menu, cx: &mut Context<Self>) -> impl IntoElement {
        let p = brand::palette(cx);
        let theme = cx.theme();
        let (surface, foreground) = (theme.popover, theme.popover_foreground);
        let (active, hover, border) = (theme.list_active, theme.list_hover, theme.border);
        let muted = p.text_muted;
        let (entries, more) = self.nav_visible(menu);
        let selected = self.title_nav.selected.min(entries.len().saturating_sub(1));
        let scope = self.nav_scope();
        let mut panel = v_flex()
            .id("title-nav-menu")
            .debug_selector(move || format!("title-nav-{}", menu.id()))
            .key_context(MENU_CONTEXT)
            .occlude()
            .w(px(MENU_WIDTH))
            .p_1()
            .gap_0p5()
            .rounded(px(10.))
            .bg(surface)
            .text_color(foreground)
            .text_size(px(13.))
            .border_1()
            .border_color(border)
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                if !this.title_nav.on_trigger(event.position) {
                    this.close_nav_menu(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NavMenuNext, _, cx| this.move_nav_menu(1, false, cx)))
            .on_action(
                cx.listener(|this, _: &NavMenuPrevious, _, cx| this.move_nav_menu(-1, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &NavMenuCycleNext, _, cx| this.move_nav_menu(1, true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &NavMenuCyclePrevious, _, cx| {
                    this.move_nav_menu(-1, true, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &NavMenuOpen, window, cx| {
                this.open_nav_entry(this.title_nav.selected, window, cx)
            }));
        if let Some(focus) = &self.title_nav.focus {
            panel = panel.track_focus(focus);
        }
        if let Some(scope) = scope {
            panel = panel.child(
                h_flex().p_1().child(
                    gpui_component::button::ButtonGroup::new("title-nav-scope").children(
                        [
                            (Scope::Project, "This project"),
                            (Scope::Vault, "All vault"),
                        ]
                        .into_iter()
                        .map(|(choice, label)| {
                            let id = match choice {
                                Scope::Project => "title-nav-scope-project",
                                Scope::Vault => "title-nav-scope-vault",
                            };
                            Button::new(id)
                                .small()
                                .ghost()
                                .label(label)
                                .selected(scope == choice)
                                .debug_selector(move || id.into())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_nav_scope(choice, cx);
                                    this.focus_nav_menu(window, cx);
                                }))
                        }),
                    ),
                ),
            );
        }
        let empty = match menu {
            Menu::Recent => "Notes you open appear here.",
            Menu::Pinned => "Notes you pin appear here.",
        };
        panel = panel.child(
            v_flex()
                .id("title-nav-entries")
                .track_scroll(&self.title_nav.scroll)
                .max_h(px(360.))
                .overflow_y_scroll()
                .when(entries.is_empty(), |list| {
                    list.child(div().px_2().py_2().text_color(muted).child(empty))
                })
                .children(entries.into_iter().enumerate().map(|(index, entry)| {
                    let current = index == selected;
                    h_flex()
                        .id(("title-nav-entry", index))
                        .debug_selector(move || format!("title-nav-entry-{index}"))
                        .w_full()
                        .min_w_0()
                        .h(px(if entry.location.is_some() { 42. } else { 30. }))
                        .flex_none()
                        .px_2()
                        .gap_2()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .when(current, |row| row.bg(active))
                        .hover(move |row| row.bg(hover))
                        .child(
                            Icon::new(if entry.folder {
                                IconName::Folder
                            } else {
                                IconName::FileText
                            })
                            .small()
                            .text_color(muted),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().truncate().child(entry.label))
                                .when_some(entry.location, |column, location| {
                                    column.child(
                                        div()
                                            .truncate()
                                            .text_size(px(12.))
                                            .text_color(muted)
                                            .child(location),
                                    )
                                }),
                        )
                        .when_some(entry.age, |row, age| {
                            row.child(
                                div()
                                    .flex_none()
                                    .text_size(px(12.))
                                    .text_color(muted)
                                    .child(age),
                            )
                        })
                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                            if this.title_nav.selected != index {
                                this.title_nav.selected = index;
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_nav_entry(index, window, cx)
                        }))
                })),
        );
        if menu == Menu::Recent {
            // Names the key the open menu itself handles, not the
            // Reader-wide recent switcher it would otherwise fall through to.
            let cycle = reader_shortcuts::shortcut(&NavMenuCycleNext, cx)
                .map(|key| format!("{key} cycles recent"));
            panel = panel.child(
                h_flex()
                    .px_2()
                    .pt_1()
                    .h(px(28.))
                    .border_t_1()
                    .border_color(border)
                    .text_size(px(12.))
                    .text_color(muted)
                    .children(cycle)
                    .child(div().flex_1())
                    .when(more, |footer| {
                        footer.child(
                            Button::new("title-nav-show-all")
                                .ghost()
                                .xsmall()
                                .label("Show all")
                                .debug_selector(|| "title-nav-show-all".into())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.title_nav.show_all = true;
                                    this.focus_nav_menu(window, cx);
                                    cx.notify();
                                })),
                        )
                    }),
            );
        }
        panel
    }
}

#[cfg(test)]
mod tests;
