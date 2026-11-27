use super::*;

/// One row of the browsing sidebar (#369).
#[derive(Clone)]
pub(super) enum SideItem {
    /// Recent / Pinned rows. The sidebar stopped emitting them when those
    /// lists moved to the title bar (#1508); the row renderer is kept until
    /// the tree-row work of #1594 lands, then removed with it.
    #[allow(dead_code)]
    Entry {
        section: reader_sidebar::Section,
        path: String,
        label: String,
        meta: Option<String>,
        location: Option<String>,
        folder: bool,
    },
    Empty(&'static str),
    Tree(reader_tree::Row),
    /// The name gets its own full-width row; options cannot shrink its input.
    #[cfg(any(unix, windows))]
    Create(Entity<InputState>, usize, bool),
    #[cfg(any(unix, windows))]
    CreateOptions(
        usize,
        bool,
        Vec<String>,
        Option<String>,
        (SharedString, SharedString),
    ),
    #[cfg(any(unix, windows))]
    CreateError(String, usize),
    #[cfg(any(unix, windows))]
    Rename(Entity<InputState>, usize, bool),
}

#[cfg(any(unix, windows))]
fn append_creation_rows(
    items: &mut Vec<SideItem>,
    create: &reader_create::Creation,
    depth: usize,
    scope: &str,
) {
    items.push(SideItem::Create(
        create.input.clone(),
        depth,
        create.directory,
    ));
    items.push(SideItem::CreateOptions(
        depth,
        create.directory,
        create
            .templates
            .as_ref()
            .map_or_else(Vec::new, |c| c.files.clone()),
        create.selected_template.clone(),
        creation_destination(scope, &create.folder),
    ));
}

/// The inline create row's «in: …» (#1512): the tree's root is «/» (the
/// vault root, or the project's folder inside a project), any other folder
/// its name; the tooltip says it in full.
#[cfg(any(unix, windows))]
fn creation_destination(scope: &str, folder: &str) -> (SharedString, SharedString) {
    if folder == scope || folder.is_empty() {
        let tooltip = if scope.is_empty() || folder.is_empty() {
            "Vault root"
        } else {
            "Project root"
        };
        return ("in: /".into(), tooltip.into());
    }
    let name = folder.rsplit('/').next().unwrap_or(folder);
    let mut label: String = name.chars().take(12).collect();
    if name.chars().count() > 12 {
        label.push('…');
    }
    (format!("in: {label}").into(), folder.to_owned().into())
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum TreeKey {
    Down,
    Up,
    Right,
    Left,
    Open,
    ExpandSubtree,
    CollapseSubtree,
}
/// #774: the accent line height and the space the name dot takes.
pub(super) const VAULT_ACCENT_LINE: f32 = 2.;
/// 8 px dot + 4 px margin + the header row's 4 px gap.
pub(super) const VAULT_DOT_SLOT: f32 = 16.;
/// The project's avatar in the sidebar header (#1627).
const SIDEBAR_AVATAR: f32 = 18.;

/// Every sidebar tree row, inline create and rename rows included.
pub(super) const TREE_ROW_HEIGHT: f32 = 28.;

pub(super) fn tree_row_base(id: SharedString) -> Stateful<Div> {
    h_flex()
        .id(id)
        .w_full()
        .h(px(TREE_ROW_HEIGHT))
        .gap(px(6.))
        .pr_2()
        .rounded(px(6.))
        .whitespace_nowrap()
}
pub(super) const HIDDEN_FILES_MENU: &str = "Show hidden files";
/// One sidebar header action (#1512): a 28 px button and its 2 px gap.
pub(super) const SIDEBAR_ACTION_SLOT: f32 = 30.;
/// Gap between the name, the spacer and the action group.
pub(super) const SIDEBAR_NAME_GAP: f32 = 4.;
/// The name button's padding and its ▾ caret around the text.
const SIDEBAR_NAME_CHROME: f32 = 30.;
/// The name keeps at least this much, caret included, before an action
/// moves into ⋯; a longer name is cut with an ellipsis first (#1512). At the
/// default 280 px width all five actions fit, vault colour dot included.
pub(super) const SIDEBAR_NAME_MIN: f32 = 80.;

/// The sidebar header's actions, in their fixed order (#1512). The order is
/// also their priority: a narrow sidebar sends the trailing ones into ⋯.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SidebarAction {
    NewNote,
    /// Today's daily note (#3182).
    DailyNote,
    NewFolder,
    CollapseAll,
    ShowHidden,
}

impl SidebarAction {
    pub(super) const ALL: [Self; 5] = [
        Self::NewNote,
        Self::DailyNote,
        Self::NewFolder,
        Self::CollapseAll,
        Self::ShowHidden,
    ];

    /// Only these leave the row for ⋯ on a narrow sidebar; they trail the
    /// fixed ones, so the spilled tail is always a run of them.
    pub(super) fn spills(self) -> bool {
        matches!(self, Self::ShowHidden)
    }

    /// Creating files needs the platform file layer.
    pub(super) fn available() -> &'static [Self] {
        const WITHOUT_FILES: [SidebarAction; 2] =
            [SidebarAction::CollapseAll, SidebarAction::ShowHidden];
        if cfg!(any(unix, windows)) {
            &Self::ALL
        } else {
            &WITHOUT_FILES
        }
    }

    pub(super) fn id(self) -> &'static str {
        match self {
            Self::NewNote => "sidebar-new-note",
            Self::DailyNote => "sidebar-daily-note",
            Self::NewFolder => "sidebar-new-folder",
            Self::CollapseAll => "sidebar-collapse-all",
            Self::ShowHidden => "sidebar-show-hidden",
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::NewNote => "New note",
            Self::DailyNote => "Today's note",
            Self::NewFolder => "New folder",
            Self::CollapseAll => "Collapse all folders",
            Self::ShowHidden => HIDDEN_FILES_MENU,
        }
    }

    pub(super) fn action(self) -> Box<dyn Action> {
        match self {
            Self::NewNote => Box::new(NewNote),
            Self::DailyNote => Box::new(OpenDailyNote),
            Self::NewFolder => Box::new(NewFolder),
            Self::CollapseAll => Box::new(CollapseFolders),
            Self::ShowHidden => Box::new(ToggleHiddenFiles),
        }
    }

    pub(super) fn tooltip(self, cx: &App) -> SharedString {
        reader_shortcuts::hint(self.label(), self.action().as_ref(), cx)
    }

    /// Show hidden is a toggle: its icon says whether hidden files show.
    pub(super) fn icon(self, show_hidden: bool) -> Icon {
        match self {
            Self::NewNote => Icon::default().path("icons/square-pen.svg"),
            Self::DailyNote => Icon::new(IconName::Calendar),
            Self::NewFolder => Icon::default().path("icons/folder-plus.svg"),
            Self::CollapseAll => Icon::default().path(brand::READER_COLLAPSE_ICON),
            Self::ShowHidden if show_hidden => Icon::new(IconName::Eye),
            Self::ShowHidden => Icon::new(IconName::EyeOff),
        }
    }

    pub(super) fn run(self, reader: &mut Reader, window: &mut Window, cx: &mut Context<Reader>) {
        match self {
            #[cfg(any(unix, windows))]
            Self::NewNote => reader.new_note(None, window, cx),
            #[cfg(any(unix, windows))]
            Self::DailyNote => reader.open_daily_note(window, cx),
            #[cfg(any(unix, windows))]
            Self::NewFolder => reader.new_folder(None, window, cx),
            Self::CollapseAll => reader.collapse_folders(cx),
            Self::ShowHidden => reader.toggle_hidden_files(cx),
            #[cfg(not(any(unix, windows)))]
            Self::NewNote | Self::DailyNote | Self::NewFolder => {}
        }
    }
}

pub(super) fn sidebar_action_click(
    reader: WeakEntity<Reader>,
    action: SidebarAction,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_, window, cx| {
        let _ = reader.update(cx, |this, cx| action.run(this, window, cx));
    }
}

/// How many header actions fit beside the name (#1512). New note, New
/// folder and Collapse all always stay; only Show hidden
/// move into ⋯, which then takes a slot of its own. The name gives way
/// first, down to its minimum; below that it is cut with an ellipsis.
pub(super) fn sidebar_visible_actions(
    available: f32,
    name: f32,
    actions: &[SidebarAction],
) -> usize {
    let total = actions.len();
    let fixed = actions.iter().filter(|action| !action.spills()).count();
    let name = name.min(SIDEBAR_NAME_MIN);
    (fixed..=total)
        .rev()
        .find(|&visible| {
            name + (visible + usize::from(visible < total)) as f32 * SIDEBAR_ACTION_SLOT
                <= available
        })
        .unwrap_or(fixed)
}

/// The sidebar name's menu (#1512).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SidebarNameItem {
    SwitchVault,
    RevealVault,
    VaultSettings,
    SwitchProject,
    ChangeProjectIcon,
    RevealProject,
    LeaveProject,
}

impl SidebarNameItem {
    pub(super) fn label(self) -> SharedString {
        use platform::labels::Os;
        match self {
            Self::SwitchVault => "Open folder…".into(),
            Self::RevealVault => Os::CURRENT.reveal_named("vault").into(),
            Self::VaultSettings => "Vault settings".into(),
            Self::SwitchProject => "Switch project…".into(),
            Self::ChangeProjectIcon => "Change icon…".into(),
            Self::RevealProject => Os::CURRENT.reveal_named("project").into(),
            Self::LeaveProject => "Leave project".into(),
        }
    }
}

pub(super) fn reader_item_menu(
    menu: gpui_component::menu::PopupMenu,
    reader: &Entity<Reader>,
    relative: String,
    window: &mut Window,
    cx: &mut Context<gpui_component::menu::PopupMenu>,
) -> gpui_component::menu::PopupMenu {
    let selection = reader.read(cx).tree.selected_paths();
    if selection.len() > 1 && selection.contains(&relative) {
        let mut menu = menu;
        for (label, kind) in [
            ("Move selected…", reader_batch::Kind::Move),
            ("Rename selected…", reader_batch::Kind::Rename),
            ("Trash selected…", reader_batch::Kind::Trash),
        ] {
            let reader = reader.downgrade();
            menu = menu.item(gpui_component::menu::PopupMenuItem::new(label).on_click(
                move |_, window, cx| {
                    let _ =
                        reader.update(cx, |reader, cx| reader.open_batch_dialog(kind, window, cx));
                },
            ));
        }
        return menu;
    }
    let root = reader.read(cx).vault_root.clone();
    let scope = reader.read(cx).link_scope();
    let menu = reader_files::menu(menu, root, relative.clone(), scope, window, cx);
    #[cfg(any(unix, windows))]
    let menu =
        {
            let movable =
                reader.read(cx).vault.entries.iter().any(|e| {
                    e.path == relative && e.kind != okilum_core::vault::EntryKind::Attachment
                });
            let rename_reader = reader.downgrade();
            let rename_path = relative.clone();
            let menu = menu.when(movable, |menu| {
                menu.item(
                    gpui_component::menu::PopupMenuItem::new("Rename…").on_click(
                        move |_, window, cx| {
                            let _ = rename_reader.update(cx, |this, cx| {
                                this.begin_rename(rename_path.clone(), window, cx)
                            });
                        },
                    ),
                )
            });
            let move_reader = reader.downgrade();
            let move_path = relative.clone();
            let menu = menu.when(movable, |menu| {
                menu.item(
                    gpui_component::menu::PopupMenuItem::new("Move to…").on_click(
                        move |_, window, cx| {
                            let _ = move_reader.update(cx, |r, cx| {
                                r.choose_move_folder(move_path.clone(), window, cx)
                            });
                        },
                    ),
                )
            });
            #[cfg(any(unix, windows))]
            let menu = {
                let reader = reader.downgrade();
                // Pinned files stay in the row above the document (#2789).
                let menu = match reader_pin_row::pin_menu_item(&reader, &relative, cx) {
                    Some(pin) => menu.item(pin),
                    None => menu,
                };
                menu.separator().item(
                    gpui_component::menu::PopupMenuItem::new(
                        crate::platform::labels::Os::CURRENT.move_to_trash(),
                    )
                    .on_click(move |_, window, cx| {
                        let _ = reader.update(cx, |this, cx| {
                            this.delete_path(relative.clone(), window, cx)
                        });
                    }),
                )
            };
            menu
        };
    menu
}

impl Reader {
    pub(super) fn focus_sidebar_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_navigation.active.is_some() {
            self.show_vault_files(window, cx);
        }
        self.tree.clear_selection();
        self.resizing_panel = None;
        self.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        self.save_sidebar(cx);
        self.panels.open(reader_layout::Panel::Notes);
        cx.notify();
        self.sidebar_search_focus.focus(window, cx);
    }

    pub(super) fn scroll_tree_to(&self, ix: usize) {
        self.tree_scroll.scroll_to_item(ix, ScrollStrategy::Center);
    }

    pub(super) fn save_sidebar(&self, cx: &mut Context<Self>) {
        let Some(path) = self.sidebar_path.clone() else {
            return;
        };
        let state = self.sidebar.clone();
        let root = self.vault_root.clone();
        let navigation = self.navigation_paths();
        let latest = self.sidebar_save_sequence.clone();
        let sequence = latest.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = reader_path_moves::save_sidebar(
                    state, &path, &root, navigation, sequence, &latest,
                ) {
                    diagnostic_error!("Could not save sidebar state: {error:#}");
                }
            })
            .detach();
    }

    pub(super) fn note_opened(&mut self, cx: &mut Context<Self>) {
        if self.pane.current_rel.is_empty() {
            return;
        }
        if self.single_file
            && self.vault.inventory_scanned
            && !self
                .vault
                .notes
                .iter()
                .any(|note| note.path == self.pane.current_rel)
        {
            let parent = Path::new(&self.pane.current_rel)
                .parent()
                .unwrap_or(Path::new(""));
            self.load_quick_folder(okilum_core::vault::note_path(parent), cx);
        }
        self.load_sidebar_state();
        let rel = self.pane.current_rel.clone();
        if self.sidebar.record_open(&rel, reader_sidebar::now()) {
            self.save_sidebar(cx);
        }
    }

    /// Switch sidebar state to the current root before anything records into
    /// it. Only the very first load keeps opens recorded before it.
    pub(super) fn load_sidebar_state(&mut self) {
        let path = self
            .session_directory
            .as_ref()
            .map(|directory| reader_sidebar::State::path(directory, &self.vault_root));
        if path == self.sidebar_path {
            return;
        }
        let mut state = path
            .as_deref()
            .map(reader_sidebar::State::load)
            .unwrap_or_default();
        if self.sidebar_path.is_none() {
            for (rel, at) in self.sidebar.recent.iter().rev() {
                if !state.recent.iter().any(|(p, _)| p == rel) {
                    state.record_open(rel, *at);
                }
            }
        }
        self.sidebar = state;
        self.sidebar_path = path;
        self.workbench_scroll = ScrollHandle::new();
        self.inbox.clear();
        self.inbox_source = None;
        self.recent_expanded = false;
        self.tree_revealed.clear();
    }

    /// Show hidden files (#395): per root, remembered with the sidebar.
    pub(super) fn toggle_hidden_files(&mut self, cx: &mut Context<Self>) {
        self.sidebar.show_hidden = !self.sidebar.show_hidden;
        self.tree.set_show_hidden(self.sidebar.show_hidden);
        self.tree_revealed.clear();
        self.save_sidebar(cx);
        cx.notify();
    }

    pub(super) fn toggle_pin(&mut self, path: &str, cx: &mut Context<Self>) {
        self.sidebar.toggle_pin(path);
        self.save_sidebar(cx);
        cx.notify();
    }

    pub(super) fn toggle_section(
        &mut self,
        section: reader_sidebar::Section,
        cx: &mut Context<Self>,
    ) {
        self.sidebar.toggle_section(section);
        self.save_sidebar(cx);
        cx.notify();
    }

    /// Load this root's sidebar state and recompute Inbox when the published
    /// inventory changes. Birth times are read off the UI thread.
    pub(super) fn sync_sidebar(&mut self, cx: &mut Context<Self>) {
        if self.single_file {
            return;
        }
        self.load_sidebar_state();
        self.refresh_agents_offer(cx);
        self.refresh_tree_rules(cx);
        if self.tree.show_hidden() != self.sidebar.show_hidden {
            self.tree.set_show_hidden(self.sidebar.show_hidden);
            self.tree_revealed.clear();
        }
        if !self.vault.inventory_complete
            || self
                .inbox_source
                .as_ref()
                .is_some_and(|source| Arc::ptr_eq(source, &self.vault))
        {
            return;
        }
        self.inbox_source = Some(self.vault.clone());
        let vault = self.vault.clone();
        let root = self.vault_root.clone();
        let mut first_seen = self.sidebar.first_seen.clone();
        let baseline = self.sidebar.baseline_taken;
        cx.spawn(async move |this, cx| {
            let compute_vault = vault.clone();
            let (items, first_seen) = cx
                .background_executor()
                .spawn(async move {
                    let items = reader_sidebar::compute_inbox(
                        &compute_vault,
                        &mut first_seen,
                        baseline,
                        reader_sidebar::now(),
                        |rel| reader_sidebar::birth_time(&root, rel),
                    );
                    (items, first_seen)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A newer inventory superseded this computation.
                if !this
                    .inbox_source
                    .as_ref()
                    .is_some_and(|source| Arc::ptr_eq(source, &vault))
                {
                    return;
                }
                let inbox_changed =
                    this.inbox != items || this.inbox_ready_root.as_ref() != Some(&this.vault_root);
                this.inbox = items;
                if this.inbox_ready_root.as_ref() != Some(&this.vault_root) {
                    // Rows above the tree changed height; place the current
                    // note again once per root.
                    this.inbox_ready_root = Some(this.vault_root.clone());
                    this.tree_revealed.clear();
                }
                this.sidebar.first_seen = first_seen;
                this.sidebar.baseline_taken = true;
                this.sidebar.prune(&vault);
                this.save_sidebar(cx);
                if inbox_changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Section rows; headers are mounted outside their content viewports (#434).
    #[cfg(test)]
    pub(super) fn sidebar_items(&self) -> Vec<SideItem> {
        self.sidebar_items_named(&vault_name::folder_name(&self.vault_root))
    }

    /// The left sidebar is the folder tree only (#1508): root folders and
    /// files sit directly under the sidebar header, with no Folders row that
    /// could fold the whole root away (#1512, #1474). Recent / Pinned live in
    /// the title bar. `vault` is what the vault is called in Inbox locations
    /// (#1327).
    pub(super) fn sidebar_items_named(&self, _vault: &str) -> Vec<SideItem> {
        // Folder mode uses the same inline creation/rename rows as a vault.
        // Returning only Tree rows here hides the focused name input (#1823).
        // A vault without notes shows no hint: the page says how to start (#1350).
        let hints = !self.vault.notes.is_empty();
        let mut items = Vec::new();
        #[cfg(any(unix, windows))]
        let creation = self.creation.as_ref();
        #[cfg(any(unix, windows))]
        let scope = self.tree.scope();
        // The tree's root (the vault root, or a project's folder) has no
        // row: its inline create row comes first (#1512).
        #[cfg(any(unix, windows))]
        if let Some(create) = creation.filter(|c| c.folder.is_empty() || c.folder == scope) {
            append_creation_rows(&mut items, create, 0, scope);
        }
        if hints && self.tree.rows.is_empty() {
            items.push(SideItem::Empty("Notes and folders appear here."));
        }
        for row in self.tree.rows.iter() {
            #[cfg(any(unix, windows))]
            if let Some(rename) = self
                .renaming
                .as_ref()
                .filter(|r| !r.in_header && r.path == row.path)
            {
                items.push(SideItem::Rename(
                    rename.input.clone(),
                    row.depth,
                    row.kind == okilum_core::vault::EntryKind::Directory,
                ));
                if let Some(error) = &rename.error {
                    let mut line = String::new();
                    for word in error.split_whitespace() {
                        if !line.is_empty() && line.chars().count() + word.chars().count() + 1 > 28
                        {
                            items.push(SideItem::CreateError(std::mem::take(&mut line), row.depth));
                        }
                        if !line.is_empty() {
                            line.push(' ');
                        }
                        line.push_str(word);
                    }
                    if !line.is_empty() {
                        items.push(SideItem::CreateError(line, row.depth));
                    }
                }
                continue;
            }
            items.push(SideItem::Tree(row.clone()));
            #[cfg(any(unix, windows))]
            if let Some(create) = creation.filter(|c| c.folder == row.path) {
                append_creation_rows(&mut items, create, row.depth + 1, scope);
            }
        }
        items
    }

    fn activate_side_entry(
        &mut self,
        path: &str,
        folder: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if folder {
            self.reveal_in_tree(path, window, cx);
        } else {
            self.select_panel_note(reader_layout::Panel::Notes, path, None, window, cx);
        }
    }

    pub(super) fn sync_tree(&mut self) {
        if !self
            .tree_source
            .as_ref()
            .is_some_and(|source| Arc::ptr_eq(source, &self.vault))
        {
            let same_root = self
                .tree_source
                .as_ref()
                .is_some_and(|source| source.root == self.vault.root);
            self.tree.refresh_from_vault(&self.vault_root, &self.vault);
            self.tree_sticky.clear();
            self.tree_earlier_roots.clear();
            self.tree_source = Some(self.vault.clone());
            // Reconciliation must not re-center a tree the user already scrolled.
            if !same_root || self.tree.cursor.is_none() {
                self.tree_revealed.clear();
            }
            // Titles belong to this inventory and root.
            if !same_root {
                Arc::make_mut(&mut self.backlink_titles).clear();
            }
            self.pane.backlinks_expanded.clear();
            // Collapsed cards are shorter: the list measures them again.
            self.backlinks_list.remeasure();
        }
        self.tree.set_titles(self.backlink_titles.clone());
        let selected = self.selected_file().to_owned();
        #[cfg(any(unix, windows))]
        let selected = self
            .creation
            .as_ref()
            .map_or(selected, |create| create.folder.clone());
        #[cfg(any(unix, windows))]
        let selected = self
            .renaming
            .as_ref()
            .map_or(selected, |rename| rename.path.clone());
        if self.tree_revealed != selected {
            // Another note is open: the index row's explicit entry is over.
            if self
                .tree_index_entry
                .as_ref()
                .is_some_and(|entry| *entry == self.tree_revealed && entry != self.selected_file())
            {
                self.tree_index_entry = None;
            }
            // An inline rename edits the row itself, whatever stands for it.
            let note = selected == self.selected_file();
            #[cfg(any(unix, windows))]
            let note = note && self.renaming.is_none();
            if note {
                self.follow_open_note_in_tree(&selected);
                return;
            }
            self.tree_revealed = selected.clone();
            if let Some(ix) = self.tree.reveal(&selected) {
                self.scroll_tree_to(ix);
            }
        }
    }

    /// The folder row that stands for `note` in the tree (#1512): `note` is
    /// that folder's index note and was not opened from its own row. The
    /// root and a project's own folder have no row; their index stays itself.
    pub(super) fn tree_index_folder(&self, note: &str) -> Option<String> {
        if self.tree_index_entry.as_deref() == Some(note) {
            return None;
        }
        let (folder, _) = note.rsplit_once('/')?;
        (folder != self.tree.scope() && self.tree.is_index_note(note)).then(|| folder.to_owned())
    }

    /// The tree row highlighted as the open note (#1512).
    pub(super) fn tree_current(&self) -> String {
        let current = self.selected_file();
        self.tree_index_folder(current)
            .unwrap_or_else(|| current.to_owned())
    }

    /// After revealing `note` (row `ix`), select its folder instead when the
    /// folder stands for it (#1512).
    pub(super) fn select_index_folder(&mut self, note: &str, ix: Option<usize>) -> Option<usize> {
        match (ix, self.tree_index_folder(note)) {
            (Some(_), Some(folder)) => {
                self.tree.cursor = Some(folder);
                self.tree.cursor_index()
            }
            _ => ix,
        }
    }

    /// Breadcrumb target (#366): show the tree and select
    /// `path`; a folder is also expanded, the current note is revealed.
    pub(super) fn reveal_in_tree(
        &mut self,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tree.clear_selection();
        self.resizing_panel = None;
        self.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        self.save_sidebar(cx);
        self.panels.open(reader_layout::Panel::Notes);
        if self
            .sidebar
            .collapsed
            .remove(&reader_sidebar::Section::Folders)
        {
            self.save_sidebar(cx);
        }
        self.sync_tree();
        if self.tree.reveal(path).is_some() {
            if self
                .tree
                .cursor_index()
                .map(|ix| &self.tree.rows[ix])
                .is_some_and(|row| {
                    row.kind == okilum_core::vault::EntryKind::Directory && !row.expanded
                })
            {
                self.expand_tree_folder(path, true, cx);
            }
            if let Some(ix) = self.tree.cursor_index() {
                self.scroll_tree_to(ix);
            }
        } else if self.vault.nested_boundary(path).is_some() {
            let folders = Path::new(path)
                .ancestors()
                .skip(1)
                .map(okilum_core::vault::note_path)
                .take_while(|folder| self.vault.nested_boundary(folder).is_some())
                .collect::<Vec<_>>();
            if !folders.is_empty() {
                self.load_nested_tree_folders(
                    folders.into_iter().rev().collect(),
                    Some(path.to_owned()),
                    cx,
                );
            }
        }
        self.tree_focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn tree_return(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Return retains inline rename for notes/folders. Attachments cannot
        // be renamed here, so activate their viewer instead of refusing it.
        let attachment = self.tree.selected_paths().len() <= 1
            && self.tree.cursor_index().is_some_and(|index| {
                self.tree.rows[index].kind == okilum_core::vault::EntryKind::Attachment
            });
        if attachment {
            self.tree_key(TreeKey::Open, window, cx);
        } else {
            #[cfg(any(unix, windows))]
            self.rename_tree_note(window, cx);
            #[cfg(not(any(unix, windows)))]
            self.tree_key(TreeKey::Open, window, cx);
        }
    }

    pub(super) fn tree_key(&mut self, key: TreeKey, window: &mut Window, cx: &mut Context<Self>) {
        self.tree.clear_selection();
        let previous_cursor = self.tree.cursor.clone();
        let lazy_folder = if matches!(key, TreeKey::Right | TreeKey::ExpandSubtree) {
            self.tree.cursor_folder()
        } else {
            None
        };
        let tree = &mut self.tree;
        match key {
            TreeKey::Down => tree.step(1),
            TreeKey::Up => tree.step(-1),
            TreeKey::Right => tree.right(),
            TreeKey::Left => tree.left(),
            TreeKey::ExpandSubtree | TreeKey::CollapseSubtree => {
                if let Some(folder) = tree.cursor_folder() {
                    tree.cursor = Some(folder.clone());
                    tree.set_subtree(&folder, key == TreeKey::ExpandSubtree);
                }
            }
            TreeKey::Open => {
                if let Some(ix) = tree.cursor_index() {
                    let row = tree.rows[ix].clone();
                    self.activate_tree_row(&row, window, cx);
                }
            }
        }
        if key != TreeKey::Open && self.tree.cursor != previous_cursor {
            self.follow_tree_preview(window, cx);
        }
        if let Some(folder) = lazy_folder {
            self.load_tree_folder(folder, cx);
        }
        if let Some(ix) = self.tree.cursor_index() {
            self.scroll_tree_to(ix);
        }
        cx.notify();
    }

    /// ⌥-click and the folder context menu (#410).
    pub(super) fn set_folder_subtree(
        &mut self,
        path: &str,
        expand: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tree = &mut self.tree;
        tree.cursor = Some(path.to_owned());
        tree.set_subtree(path, expand);
        if expand {
            self.load_tree_folder(path.to_owned(), cx);
        }
        self.tree_focus.focus(window, cx);
        cx.notify();
    }

    /// Whether `position` is in the tree's empty space under its `rows`.
    fn tree_space_below_rows(&self, position: Point<Pixels>, rows: usize) -> bool {
        let scroll = self.tree_scroll.0.borrow();
        let bounds = scroll.base_handle.bounds();
        let end = bounds.top() + scroll.base_handle.offset().y + px(TREE_ROW_HEIGHT * rows as f32);
        bounds.contains(&position) && position.y >= end
    }

    /// Nothing selected in the tree: New note / New folder then create in
    /// the root (Esc in the tree, a click on its empty space; #1512). The
    /// open note keeps its highlight.
    pub(super) fn clear_tree_selection(&mut self, cx: &mut Context<Self>) {
        self.tree.clear_selection();
        if self.tree.cursor.take().is_some() {
            self.tree_revealed = self.selected_file().to_owned();
            cx.notify();
        }
    }

    /// Sidebar header «Collapse all» (#410): folds every folder; the root
    /// entries stay listed (#1512).
    pub(super) fn collapse_folders(&mut self, cx: &mut Context<Self>) {
        self.tree.collapse_all();
        cx.notify();
    }

    /// Document toolbar «Reveal in sidebar» (#1756): only the path to the
    /// open note stays expanded, and the note is selected; a folder's index
    /// note selects its folder unless it was opened from its own row (#1512).
    pub(super) fn focus_current_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.selected_file().to_owned();
        if current.is_empty() {
            return;
        }
        self.sync_tree();
        if !self.tree.can_reveal(&current) {
            if self.vault.nested_boundary(&current).is_some() {
                self.tree.collapse_all();
                self.reveal_in_tree(&current, window, cx);
            }
            return;
        }
        self.resizing_panel = None;
        self.sidebar.left_mode = reader_sidebar::LeftMode::Tree;
        self.save_sidebar(cx);
        self.panels.open(reader_layout::Panel::Notes);
        if self
            .sidebar
            .collapsed
            .remove(&reader_sidebar::Section::Folders)
        {
            self.save_sidebar(cx);
        }
        let ix = self.tree.focus(&current);
        if let Some(ix) = self.select_index_folder(&current, ix) {
            self.scroll_tree_to(ix);
        }
        self.tree_focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn activate_tree_row(
        &mut self,
        row: &reader_tree::Row,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use okilum_core::vault::EntryKind;
        self.tree_preview.close();
        let tree = &mut self.tree;
        tree.cursor = Some(row.path.clone());
        match row.kind {
            EntryKind::Directory => {
                // A double click or Enter on a folder row (#1099, #1512): a
                // collapsed folder expands and opens its index note, an
                // expanded one collapses and its index stays open. The
                // selection stays on the folder either way. One click only
                // selects; the chevron only toggles.
                let expanded = self
                    .tree
                    .rows
                    .iter()
                    .find(|r| r.path == row.path)
                    .map_or(row.expanded, |r| r.expanded);
                if expanded {
                    self.expand_tree_folder(&row.path, false, cx);
                    // An index row hidden by the collapse hands the
                    // highlight to its folder.
                    let inside = format!("{}/", row.path);
                    if self
                        .tree_index_entry
                        .as_ref()
                        .is_some_and(|entry| entry.starts_with(&inside))
                    {
                        self.tree_index_entry = None;
                    }
                } else {
                    self.expand_tree_folder(&row.path, true, cx);
                    match reader_tree::folder_index(&row.path, &self.vault.entries) {
                        reader_tree::FolderIndex::One(index) => {
                            self.tree_index_entry = None;
                            if self.selected_file() != index {
                                self.open_note(&index, None, window, cx);
                            }
                        }
                        reader_tree::FolderIndex::Several => {
                            self.link_notice = Some(
                                "This folder has more than one index note, so none was opened."
                                    .into(),
                            );
                        }
                        reader_tree::FolderIndex::None => {}
                    }
                }
                self.tree.cursor = Some(row.path.clone());
            }
            EntryKind::Markdown => {
                // The index row itself: that row, not its folder, stays
                // selected for this open (#1512).
                self.tree_index_entry = row.index.then(|| row.path.clone());
                self.open_note(&row.path, None, window, cx);
            }
            EntryKind::Attachment => {
                self.preview_file(&row.path, window, cx);
            }
        }
        // Tree activation keeps keyboard navigation/rename in the navigator.
        // Async document replacement only transfers focus from the old content.
        self.tree_focus.focus(window, cx);
        if row.kind != EntryKind::Directory {
            self.dismiss_sidebar_after_selection(window, cx);
        }
        cx.notify();
    }

    /// One click on a folder row, its name or its icon (#1512): select the
    /// folder and nothing else. It does not expand, collapse or open the
    /// folder's index; a double click does that (`activate_tree_row`).
    fn select_tree_folder(
        &mut self,
        row: &reader_tree::Row,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tree_preview.close();
        self.tree.cursor = Some(row.path.clone());
        self.tree_focus.focus(window, cx);
        cx.notify();
    }

    /// Expand or collapse a folder row, nothing else (the chevron, #1099):
    /// no note opens and the selection stays where it is (#1512).
    fn toggle_tree_folder(
        &mut self,
        row: &reader_tree::Row,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tree_preview.close();
        self.expand_tree_folder(&row.path, !row.expanded, cx);
        self.tree_focus.focus(window, cx);
        cx.notify();
    }

    fn expand_tree_folder(&mut self, path: &str, expand: bool, cx: &mut Context<Self>) {
        self.tree.set_expanded(path, expand);
        if expand {
            self.load_tree_folder(path.to_owned(), cx);
        }
    }

    fn load_tree_folder(&mut self, folder: String, cx: &mut Context<Self>) {
        if self.single_file {
            self.load_quick_folder(folder, cx);
        } else if self.vault.nested_boundary(&folder).is_some() {
            self.load_nested_tree_folders(vec![folder], None, cx);
        }
    }

    /// What the sidebar name's menu offers (#1512): the vault's own menu, or
    /// the project's inside a project. Vault settings needs a vault.
    pub(super) fn sidebar_name_menu(&self) -> Vec<SidebarNameItem> {
        if self.project_navigation.active.is_some() {
            return vec![
                SidebarNameItem::SwitchProject,
                SidebarNameItem::ChangeProjectIcon,
                SidebarNameItem::RevealProject,
                SidebarNameItem::LeaveProject,
            ];
        }
        let mut items = vec![SidebarNameItem::SwitchVault, SidebarNameItem::RevealVault];
        if !self.single_file {
            items.push(SidebarNameItem::VaultSettings);
        }
        items
    }

    /// The sidebar's one header row (#1512, #1474): the vault or project
    /// name on the left opens its menu, the sidebar actions sit in fixed
    /// slots on the right, always in the same order. Hover never reveals,
    /// hides or moves an action; only a narrow sidebar sends the trailing
    /// ones into ⋯. The tree starts directly under this row.
    pub(super) fn render_sidebar_header(
        &self,
        width: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let project = self.project_navigation.active.clone();
        let title: SharedString = match &project {
            Some(project) => project.name.clone().into(),
            None => vault_name::shown(&self.vault_root, cx).into(),
        };
        let accent = project
            .is_none()
            .then(|| reader_ui_state::vault_color(&self.vault_root, cx))
            .flatten();
        // In a project its avatar stands where the vault's dot does (#1627).
        let avatar = project.as_ref().map(|project| {
            okilum_shell_kit::project_avatar::project_avatar(project, SIDEBAR_AVATAR, cx)
        });
        let dot = if accent.is_some() { VAULT_DOT_SLOT } else { 0. };
        // The avatar narrows the name, never the fixed action slots.
        let lead = if avatar.is_some() {
            SIDEBAR_AVATAR + 4. + SIDEBAR_NAME_GAP
        } else {
            0.
        };
        let modes_visible = !self.single_file && project.is_none();
        let areas_space = if self.areas_tree().is_some() { 62. } else { 0. };
        let modes_inline = modes_visible && width >= 260. + areas_space;
        let mode = self.left_sidebar_mode();
        let actions = if mode == reader_sidebar::LeftMode::Areas {
            &SidebarAction::ALL[..0]
        } else if mode == reader_sidebar::LeftMode::Now && !self.single_file && project.is_none() {
            // New note and Today's note.
            &SidebarAction::ALL[..2 * usize::from(cfg!(any(unix, windows)))]
        } else if mode == reader_sidebar::LeftMode::Tree && project.is_none() {
            // Creation and Collapse all have permanent slots beside Vault Index.
            &SidebarAction::ALL[4..]
        } else {
            SidebarAction::available()
        };
        let room = width
            - 24.
            - dot
            - 2. * SIDEBAR_NAME_GAP
            - if modes_inline { 132. + areas_space } else { 0. };
        let name = toolbar_text_width(&title, FontWeight::SEMIBOLD, window) + SIDEBAR_NAME_CHROME;
        let visible = if modes_inline {
            0
        } else {
            sidebar_visible_actions(room, name, actions)
        };
        let overflow = !modes_inline && visible < actions.len();
        let name_max =
            (room - lead - (visible + usize::from(overflow)) as f32 * SIDEBAR_ACTION_SLOT).max(0.);
        let show_hidden = self.sidebar.show_hidden;
        let reader = cx.entity().downgrade();
        let name_items = self.sidebar_name_menu();
        let menu_root = project
            .as_ref()
            .map_or_else(|| self.vault_root.clone(), |p| p.folder.clone());
        let menu_reader = reader.clone();
        let name_button = Button::new("sidebar-vault-title")
            .ghost()
            .label(title.clone())
            .dropdown_caret(true)
            .h(px(28.))
            .px_1p5()
            .min_w_0()
            .max_w(px(name_max))
            .overflow_hidden()
            .rounded(px(6.))
            .font_weight(FontWeight::SEMIBOLD)
            .tooltip(project.as_ref().map_or_else(
                || vault_name::path_tooltip(&self.vault_root),
                |_| title.to_string(),
            ))
            .debug_selector(|| "sidebar-vault-title".into())
            .dropdown_menu(move |menu, window, cx| {
                if !name_items.contains(&SidebarNameItem::SwitchProject) {
                    return reader_vaults::menu(
                        menu,
                        menu_reader.clone(),
                        if modes_inline { actions } else { &[] },
                        show_hidden,
                        window,
                        cx,
                    );
                }
                let menu = name_items.iter().fold(menu, |menu, item| {
                    let label = item.label();
                    let root = menu_root.clone();
                    let reader = menu_reader.clone();
                    match *item {
                        SidebarNameItem::SwitchVault => {
                            menu.menu(label, Box::new(reader_open::OpenFolder))
                        }
                        SidebarNameItem::RevealVault | SidebarNameItem::RevealProject => {
                            menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                reader_files::reveal(&root, window, cx)
                            }))
                        }
                        SidebarNameItem::VaultSettings => {
                            menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                reader_settings::show_vault(Some(reader.clone()), cx)
                            }))
                        }
                        SidebarNameItem::SwitchProject => {
                            menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                let _ = reader.update(cx, |this, cx| {
                                    this.toggle_project_switcher(window, cx)
                                });
                            }))
                        }
                        SidebarNameItem::LeaveProject => {
                            menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                let _ = reader
                                    .update(cx, |this, cx| this.leave_vault_project(window, cx));
                            }))
                        }
                        SidebarNameItem::ChangeProjectIcon => {
                            menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                let _ = reader.update(cx, |this, cx| {
                                    if let Some(project) = this.project_navigation.active.clone() {
                                        this.open_project_icon_dialog(project, window, cx)
                                    }
                                });
                            }))
                        }
                    }
                });
                if modes_inline {
                    actions.iter().fold(menu.separator(), |menu, &action| {
                        menu.item(
                            PopupMenuItem::new(action.label())
                                .checked(action == SidebarAction::ShowHidden && show_hidden)
                                .on_click(sidebar_action_click(menu_reader.clone(), action)),
                        )
                    })
                } else {
                    menu
                }
            });
        let mut slots = header_controls().debug_selector(|| "sidebar-header-actions".into());
        for &action in &actions[..visible] {
            let toggled = action == SidebarAction::ShowHidden && show_hidden;
            slots = slots.child(
                reader_icon_button(
                    action.id(),
                    action.icon(show_hidden),
                    action.tooltip(cx),
                    cx,
                )
                .selected(toggled)
                .debug_selector(move || action.id().into())
                .on_click(sidebar_action_click(reader.clone(), action)),
            );
        }
        if overflow {
            let spilled: Vec<SidebarAction> = actions[visible..].to_vec();
            let menu_reader = reader.clone();
            slots = slots.child(
                reader_icon_button("sidebar-actions", IconName::Ellipsis, "More actions", cx)
                    .debug_selector(|| "sidebar-actions".into())
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                        spilled.iter().fold(menu, |menu, &action| {
                            menu.item(
                                PopupMenuItem::new(action.label())
                                    .checked(action == SidebarAction::ShowHidden && show_hidden)
                                    .action(action.action())
                                    .on_click(sidebar_action_click(menu_reader.clone(), action)),
                            )
                        })
                    }),
            );
        }
        let in_vault = project.is_none();
        #[cfg(any(unix, windows))]
        let drop_root = self.vault_root.clone();
        let header_reader = reader.clone();
        let row = h_flex()
            .id("sidebar-header")
            .debug_selector(|| "sidebar-header".into())
            .h(px(READER_HEADER_HEIGHT))
            .flex_none()
            .gap(px(SIDEBAR_NAME_GAP))
            .px_3()
            .border_b_1()
            .border_color(brand::palette(cx).border_subtle)
            .text_size(px(brand::READER_CHROME_FONT_SIZE))
            .when_some(accent, |header, color| {
                header.child(
                    div()
                        .debug_selector(|| "sidebar-vault-dot".into())
                        .flex_none()
                        .size(px(8.))
                        .ml_1()
                        .rounded_full()
                        .bg(color.color(cx)),
                )
            })
            .when_some(avatar, |header, avatar| {
                header.child(
                    div()
                        .debug_selector(|| "sidebar-project-avatar".into())
                        .flex_none()
                        .ml_1()
                        .child(avatar),
                )
            })
            .child(name_button)
            .child(div().flex_1().min_w_0())
            .when_else(
                modes_inline,
                |header| header.child(self.render_left_modes(cx)),
                |header| header.child(slots),
            )
            // The row stands where the Folders header used to: dropping a
            // note on it still moves the note to the vault root.
            .map(|row| {
                #[cfg(any(unix, windows))]
                let row = if in_vault && mode != reader_sidebar::LeftMode::Areas {
                    reader_move_picker::drop_target(row, reader.clone(), drop_root, String::new())
                } else {
                    row
                };
                row
            })
            .context_menu(move |menu, _, _| {
                if !in_vault || !cfg!(any(unix, windows)) {
                    return menu;
                }
                let item = |label: &'static str, directory: bool| {
                    let reader = header_reader.clone();
                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        let _ = reader.update(cx, |this, cx| {
                            #[cfg(any(unix, windows))]
                            if directory {
                                this.new_folder(Some(""), window, cx);
                            } else {
                                this.new_note(Some(""), window, cx);
                            }
                            #[cfg(not(any(unix, windows)))]
                            let _ = (this, directory, window, cx);
                        });
                    })
                };
                menu.item(item("New note in vault root", false))
                    .when(actions.contains(&SidebarAction::NewFolder), |menu| {
                        menu.item(item("New folder in vault root", true))
                    })
            })
            .into_any_element();
        v_flex()
            .debug_selector(|| "sidebar-mode-header".into())
            .flex_none()
            .child(row)
            .when(modes_visible && !modes_inline, |header| {
                header.child(div().px_3().py_1().child(self.render_left_modes(cx)))
            })
            .when(
                modes_visible
                    && self.sidebar.offer_now(
                        self.workbench_ready
                            .as_ref()
                            .is_some_and(|index| index.view.suggest_now),
                    ),
                |header| header.child(self.render_now_offer(cx)),
            )
            .into_any_element()
    }

    /// `![alt](relative path)` → `![alt](file:///abs%20path)` via resolve_asset.
    pub(super) fn render_sidebar(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if self.project_navigation.active.is_some() {
            return div().into_any_element();
        }
        let content = match self.left_sidebar_mode() {
            reader_sidebar::LeftMode::Areas if !self.single_file => self.render_areas_tree(cx),
            reader_sidebar::LeftMode::Now if self.sidebar.now_enabled() && !self.single_file => {
                self.render_workbench(window, cx)
            }
            _ => self.render_tree(window, cx),
        };
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .size_full()
                    .pb(if self.agents_offer_slot(cx) {
                        px(32.)
                    } else {
                        px(0.)
                    })
                    .child(content),
            )
            .when(self.show_agents_offer(cx), |view| {
                view.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .child(self.render_agents_offer(cx)),
                )
            })
            .into_any_element()
    }

    /// docs/design/reader.md §Sidebar: the real folder hierarchy only
    /// (#335, #1508); Recent and Pinned live in the title bar.
    fn render_tree(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if self.project_navigation.active.is_some() {
            return div().into_any_element();
        }
        use gpui_component::menu::DropdownMenu as _;
        use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
        use okilum_core::vault::EntryKind;
        use reader_sidebar::Section;
        let p = brand::palette(cx);
        let tokens = brand::reader_palette(cx);
        let entity = cx.entity().downgrade();
        let tree = &self.tree;
        let items = self.sidebar_items_named(&vault_name::shown(&self.vault_root, cx));
        let pinned: Rc<std::collections::BTreeSet<String>> =
            Rc::new(self.sidebar.pinned.iter().cloned().collect());
        let current = self.selected_file().to_owned();
        // A folder's index note opened through the folder lights the folder.
        let tree_current = self.tree_current();
        // The selection stays visible when the tree loses focus: New note /
        // New folder go next to it, so it must never be invisible (#1512).
        let tree_focused = self.tree_focus.is_focused(window);
        let cursor = tree.cursor.clone();
        let selection = tree.selected_paths();
        let published_root = self.vault_root.clone();
        let published_generation = self.watcher_generation;
        let rules_root = self.vault_root.clone();
        // Every row action is bound to the inventory it was rendered from.
        let act =
            move |entity: &WeakEntity<Reader>,
                  window: &mut Window,
                  cx: &mut App,
                  f: &dyn Fn(&mut Reader, &mut Window, &mut Context<Reader>)| {
                if let Some(e) = entity.upgrade() {
                    e.update(cx, |this, cx| {
                        if this.vault_root == published_root
                            && this.watcher_generation == published_generation
                        {
                            f(this, window, cx)
                        }
                    });
                }
            };
        let act = Rc::new(act);
        #[cfg(any(unix, windows))]
        let drag_root = self.vault_root.clone();
        let render_row = Rc::new(move |item: SideItem, ix: usize, cx: &App| {
            let entity = entity.clone();
            let act = act.clone();
            let row_base = tree_row_base;
            let pin = |path: String, group: SharedString, on: bool| {
                let entity = entity.clone();
                let act = act.clone();
                div()
                    .id(SharedString::from(format!("pin-{group}")))
                    .flex_none()
                    .opacity(if on { 1. } else { 0. })
                    .group_hover(group, |s| s.opacity(1.))
                    .child(
                        Icon::default()
                            .path(brand::READER_PIN_ICON)
                            .xsmall()
                            .text_color(if on { p.accent } else { tokens.text_faint }),
                    )
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let path = path.clone();
                        act(&entity, window, cx, &|this, _, cx| {
                            this.toggle_pin(&path, cx)
                        });
                    })
            };
            match item {
                #[cfg(any(unix, windows))]
                SideItem::Rename(input, depth, directory) => row_base("inline-rename-row".into())
                    .debug_selector(|| "inline-rename-row".into())
                    .key_context("InlineRename")
                    // Consume Input's propagated submit before text fallback can
                    // replace a selected filename with an empty newline.
                    .on_action(|_: &gpui_component::input::Enter, _, cx| {
                        cx.stop_propagation();
                    })
                    .pl(px(18. + depth as f32 * 14.))
                    .child(
                        Icon::new(if directory {
                            IconName::Folder
                        } else {
                            IconName::FileText
                        })
                        .small(),
                    )
                    .child(div().flex_1().min_w_0().child(Input::new(&input).small()))
                    .into_any_element(),
                #[cfg(any(unix, windows))]
                SideItem::Create(input, depth, directory, ..) => {
                    row_base("inline-create-row".into())
                        .debug_selector(|| "inline-create-row".into())
                        .key_context("InlineCreate")
                        .on_action(|_: &gpui_component::input::Enter, _, cx| {
                            cx.stop_propagation();
                        })
                        .pl(px(18. + depth as f32 * 14.))
                        .child(
                            Icon::new(if directory {
                                IconName::Folder
                            } else {
                                IconName::FileText
                            })
                            .small(),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .debug_selector(|| "create-name-input".into())
                                .child(Input::new(&input).small()),
                        )
                        .into_any_element()
                }
                #[cfg(any(unix, windows))]
                SideItem::CreateOptions(depth, directory, templates, selected, destination) => {
                    let (destination, destination_tip) = destination;
                    let destination_entity = entity.clone();
                    let destination_act = act.clone();
                    row_base("inline-create-options".into())
                        .debug_selector(|| "inline-create-options".into())
                        .pl(px(18. + depth as f32 * 14.))
                        .when(!directory, |row| {
                            row.child(
                                Button::new("create-template-picker")
                                    .ghost()
                                    .small()
                                    // Says what it is: «Template» until one is
                                    // chosen, then its name (#1351).
                                    .label(
                                        selected
                                            .as_deref()
                                            .unwrap_or("Template")
                                            .trim_end_matches(".md")
                                            .chars()
                                            .take(10)
                                            .collect::<String>(),
                                    )
                                    .icon(IconName::ChevronDown)
                                    .tooltip(format!(
                                        "Template: {}",
                                        selected
                                            .as_deref()
                                            .unwrap_or("standard note")
                                            .trim_end_matches(".md")
                                    ))
                                    .dropdown_menu_with_anchor(
                                        Anchor::TopRight,
                                        move |menu, _, _| {
                                            let option = |label: String, value: Option<String>| {
                                                let entity = entity.clone();
                                                let act = act.clone();
                                                PopupMenuItem::new(label)
                                                    .checked(value == selected)
                                                    .on_click(move |_, window, cx| {
                                                        act(
                                                            &entity,
                                                            window,
                                                            cx,
                                                            &|this, window, cx| {
                                                                this.choose_creation_template(
                                                                    value.clone(),
                                                                    window,
                                                                    cx,
                                                                );
                                                            },
                                                        )
                                                    })
                                            };
                                            let mut menu =
                                                menu.item(option("Standard note".into(), None));
                                            for name in &templates {
                                                menu = menu.item(option(
                                                    name.trim_end_matches(".md").to_owned(),
                                                    Some(name.clone()),
                                                ));
                                            }
                                            menu
                                        },
                                    ),
                            )
                        })
                        // Where it will be created, changeable before Enter
                        // (#1512): «in: /» is the vault (or project) root.
                        .child(
                            Button::new("create-destination")
                                .ghost()
                                .small()
                                .label(destination)
                                .dropdown_caret(true)
                                .tooltip(destination_tip)
                                .debug_selector(|| "create-destination".into())
                                .on_click(move |_, window, cx| {
                                    destination_act(
                                        &destination_entity,
                                        window,
                                        cx,
                                        &|this, window, cx| {
                                            this.choose_creation_folder(window, cx);
                                        },
                                    )
                                }),
                        )
                        .into_any_element()
                }
                #[cfg(any(unix, windows))]
                SideItem::CreateError(error, depth) => {
                    row_base(format!("inline-create-error-{ix}").into())
                        .pl(px(38. + depth as f32 * 14.))
                        .text_color(p.danger)
                        .text_size(px(12.))
                        .child(error.clone())
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(error.clone()).build(window, cx)
                        })
                        .into_any_element()
                }
                SideItem::Empty(text) => row_base(SharedString::from(format!("side-empty-{ix}")))
                    .pl(px(30.))
                    .text_xs()
                    .text_color(tokens.text_faint)
                    .child(text)
                    .into_any_element(),
                SideItem::Entry {
                    section,
                    path,
                    label,
                    meta,
                    location,
                    folder,
                } => {
                    let group = SharedString::from(format!("side-{}-{path}", section.label()));
                    let is_current = path == current;
                    let on = pinned.contains(&path);
                    let open_path = path.clone();
                    let open_entity = entity.clone();
                    let open_act = act.clone();
                    row_base(group.clone())
                        .when(location.is_some(), |row| row.h(px(44.)))
                        .tooltip({
                            let path = path.clone();
                            move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(path.clone())
                                    .build(window, cx)
                            }
                        })
                        .debug_selector({
                            let group = group.clone();
                            move || group.to_string()
                        })
                        .group(group.clone())
                        .pl(px(10.))
                        .cursor_pointer()
                        .when(is_current, |d| {
                            d.bg(p.selected).font_weight(FontWeight::MEDIUM)
                        })
                        .when(!is_current, |d| d.hover(move |d| d.bg(tokens.hover)))
                        .child(
                            Icon::new(if folder {
                                IconName::Folder
                            } else {
                                IconName::FileText
                            })
                            .small()
                            .text_color(p.text_muted),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .child(div().text_ellipsis().child(label))
                                .when_some(location, |column, location| {
                                    column.child(
                                        div()
                                            .debug_selector({
                                                let group = group.clone();
                                                move || format!("{group}-location")
                                            })
                                            .text_size(px(12.))
                                            .line_height(px(16.))
                                            .text_color(p.text_muted)
                                            .text_ellipsis()
                                            .child(location),
                                    )
                                }),
                        )
                        .when_some(meta, |d, meta| {
                            d.child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(tokens.text_faint)
                                    .child(meta),
                            )
                        })
                        .when(section != Section::Inbox, |d| {
                            d.child(pin(path.clone(), group.clone(), on))
                        })
                        .on_click(move |_, window, cx| {
                            let path = open_path.clone();
                            open_act(&open_entity, window, cx, &|this, window, cx| {
                                this.activate_side_entry(&path, folder, window, cx)
                            })
                        })
                        .into_any_element()
                }
                SideItem::Tree(row) => {
                    use okilum_core::vault_descriptor::NestedBoundary;
                    let directory = row.kind == EntryKind::Directory;
                    let is_selected = selection.contains(&row.path);
                    let is_current =
                        is_selected || (selection.len() < 2 && row.path == tree_current);
                    let is_cursor = cursor.as_ref() == Some(&row.path);
                    let label = row.label.clone();
                    let vault_name = match &row.nested_boundary {
                        Some(NestedBoundary::Vault(name)) => Some(name),
                        _ => None,
                    };
                    let icon = match row.kind {
                        EntryKind::Directory if vault_name.is_some() => None,
                        EntryKind::Directory if row.depth == 0 && row.nested_boundary.is_none() => {
                            None
                        }
                        EntryKind::Directory if row.expanded => Some(IconName::FolderOpen),
                        EntryKind::Directory => Some(IconName::Folder),
                        // The folder's index note has its own glyph (#1512).
                        EntryKind::Markdown if row.index => Some(IconName::BookOpen),
                        EntryKind::Markdown => Some(IconName::FileText),
                        EntryKind::Attachment => Some(IconName::File),
                    };
                    let group = SharedString::from(format!("tree-row-{}", row.path));
                    let pinnable =
                        row.kind != EntryKind::Attachment && !(directory && row.depth == 0);
                    let on = pinned.contains(&row.path);
                    let path = row.path.clone();
                    row_base(group.clone())
                        .map(|d| {
                            #[cfg(any(unix, windows))]
                            let d = {
                                let root = drag_root.clone();
                                let d = if selection.len() < 2
                                    && (directory || row.kind == EntryKind::Markdown)
                                {
                                    reader_move_picker::draggable(
                                        d,
                                        root.clone(),
                                        row.path.clone(),
                                        label.clone(),
                                    )
                                } else {
                                    d
                                };
                                if directory {
                                    reader_move_picker::drop_target(
                                        d,
                                        entity.clone(),
                                        root,
                                        row.path.clone(),
                                    )
                                } else {
                                    d
                                }
                            };
                            d
                        })
                        .debug_selector({
                            let group = group.clone();
                            move || group.to_string()
                        })
                        .group(group.clone())
                        .pl(px(4. + row.depth as f32 * 14.))
                        .tooltip({
                            let path = if let Some(name) = vault_name {
                                format!("Separate vault “{name}”\n{}", row.path)
                            } else if row.agent_shim {
                                format!("{}\nAGENTS.md import shim", row.path)
                            } else {
                                row.path.clone()
                            };
                            move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(path.clone())
                                    .build(window, cx)
                            }
                        })
                        .cursor_pointer()
                        .when(is_current, |d| {
                            d.bg(p.selected).font_weight(FontWeight::MEDIUM)
                        })
                        .when(!is_current, |d| d.hover(move |d| d.bg(tokens.hover)))
                        .when(is_cursor && tree_focused, |d| {
                            d.border_1().border_color(p.focus)
                        })
                        .when(is_cursor && !tree_focused && !is_current, |d| {
                            d.bg(p.selected.opacity(0.5))
                        })
                        .when(directory && row.depth == 0, |d| {
                            d.font_weight(FontWeight::SEMIBOLD)
                        })
                        // Archive stays visible but recedes (#369);
                        // hidden items shown on demand too (#395).
                        .when((row.archived || row.hidden) && !is_current, |d| {
                            d.text_color(p.text_muted).opacity(0.75)
                        })
                        // The index note recedes under its folder (#1512).
                        .when(row.index && !is_current, |d| d.text_color(p.text_muted))
                        .child(if directory {
                            let toggle_entity = entity.clone();
                            let toggle_row = row.clone();
                            div()
                                .id(SharedString::from(format!(
                                    "folder-disclosure-{}",
                                    row.path
                                )))
                                .debug_selector({
                                    let path = row.path.clone();
                                    move || format!("folder-disclosure-{path}")
                                })
                                .on_click(move |event, window, cx| {
                                    cx.stop_propagation();
                                    if event.click_count() != 1 {
                                        return;
                                    }
                                    let _ = toggle_entity.update(cx, |this, cx| {
                                        this.toggle_tree_folder(&toggle_row, window, cx)
                                    });
                                })
                                .child(
                                    Icon::new(if row.expanded {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .xsmall()
                                    .text_color(tokens.text_faint),
                                )
                                .into_any_element()
                        } else {
                            div().w(px(14.)).flex_none().into_any_element()
                        })
                        .when_some(icon, |d, icon| {
                            d.child(Icon::new(icon).small().text_color(p.text_muted))
                        })
                        .when(vault_name.is_some(), |d| d.child(brand::logo(14., cx)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(label),
                        )
                        .when_some(row.agent_rules.clone(), |d, rules| {
                            d.child(reader_tree_rules::badge(
                                rules,
                                entity.clone(),
                                rules_root.clone(),
                                published_generation,
                            ))
                        })
                        .when(pinnable, |d| d.child(pin(path, group.clone(), on)))
                        .on_click({
                            let entity = entity.clone();
                            let act = act.clone();
                            let row = row.clone();
                            move |event, window, cx| {
                                let row = row.clone();
                                // ⌥-click: the folder and all its
                                // subfolders, as in Finder (#410).
                                let deep = directory && event.modifiers().alt;
                                act(&entity, window, cx, &|this, window, cx| {
                                    let modifiers = event.modifiers();
                                    if modifiers.control || modifiers.platform || modifiers.shift {
                                        this.tree.select_click(
                                            &row.path,
                                            modifiers.control || modifiers.platform,
                                            modifiers.shift,
                                        );
                                        this.tree_focus.focus(window, cx);
                                        cx.notify();
                                        return;
                                    }
                                    this.tree.select_click(&row.path, false, false);
                                    if deep {
                                        this.set_folder_subtree(
                                            &row.path,
                                            !row.expanded,
                                            window,
                                            cx,
                                        )
                                    } else if directory {
                                        // One click selects at once; the second
                                        // click of a double click expands and
                                        // opens, or collapses (#1512).
                                        match event.click_count() {
                                            1 => this.select_tree_folder(&row, window, cx),
                                            2 => this.activate_tree_row(&row, window, cx),
                                            _ => {}
                                        }
                                    } else {
                                        this.activate_tree_row(&row, window, cx)
                                    }
                                })
                            }
                        })
                        .map(|d| {
                            if !directory {
                                let file_entity = entity.clone();
                                let file_rel = row.path.clone();
                                return d
                                    .context_menu(move |menu, window, cx| {
                                        let Some(view) = file_entity.upgrade() else {
                                            return menu;
                                        };
                                        let menu = reader_item_menu(
                                            menu,
                                            &view,
                                            file_rel.clone(),
                                            window,
                                            cx,
                                        );
                                        // A log file joins the open log view (#1160).
                                        let reader = view.read(cx);
                                        let path = reader.vault_root.join(&file_rel);
                                        match reader
                                            .pane
                                            .file_preview
                                            .as_ref()
                                            .and_then(|preview| preview.log.clone())
                                        {
                                            Some(log)
                                                if okilum_core::log::is_log_path(&path)
                                                    && !log.read(cx).has_file(&path) =>
                                            {
                                                menu.separator().item(
                                                    PopupMenuItem::new("Add to log view").on_click(
                                                        move |_, _, cx| {
                                                            log.update(cx, |log, cx| {
                                                                log.add_file_paths(
                                                                    vec![path.clone()],
                                                                    cx,
                                                                )
                                                            });
                                                        },
                                                    ),
                                                )
                                            }
                                            _ => menu,
                                        }
                                    })
                                    .into_any_element();
                            }
                            let folder = row.path.clone();
                            d.context_menu(move |menu, window, cx| {
                                // A folder that is not a project yet can
                                // become one (#1475).
                                let make_project = entity.upgrade().is_some_and(|view| {
                                    reader_features::enabled(reader_features::Feature::Agents, cx)
                                        && view.read(cx).can_make_project(&folder)
                                });
                                let menu = if let Some(view) = entity.upgrade() {
                                    reader_item_menu(menu, &view, folder.clone(), window, cx)
                                        .separator()
                                } else {
                                    menu
                                };
                                let menu = menu.when(make_project, |menu| {
                                    let entity = entity.clone();
                                    let act = act.clone();
                                    let folder = folder.clone();
                                    menu.item(
                                        PopupMenuItem::new(reader_project_create::MAKE_PROJECT)
                                            .on_click(move |_, window, cx| {
                                                act(&entity, window, cx, &|this, window, cx| {
                                                    this.open_new_project_dialog(
                                                        Some(folder.clone()),
                                                        window,
                                                        cx,
                                                    )
                                                })
                                            }),
                                    )
                                    .separator()
                                });
                                let subtree = |label: &'static str, expand: bool| {
                                    let entity = entity.clone();
                                    let act = act.clone();
                                    let folder = folder.clone();
                                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                        let folder = folder.clone();
                                        act(&entity, window, cx, &|this, window, cx| {
                                            this.set_folder_subtree(&folder, expand, window, cx)
                                        })
                                    })
                                };
                                let create = |label: &'static str, directory: bool| {
                                    let entity = entity.clone();
                                    let act = act.clone();
                                    let folder = folder.clone();
                                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                        act(&entity, window, cx, &|this, window, cx| {
                                            if directory {
                                                this.new_folder(Some(&folder), window, cx);
                                            } else {
                                                this.new_note(Some(&folder), window, cx);
                                            }
                                        });
                                    })
                                };
                                menu.when(cfg!(any(unix, windows)), |menu| {
                                    menu.item(create("New File", false))
                                        .item(create("New Folder", true))
                                })
                                .separator()
                                .item(subtree("Expand all subfolders", true))
                                .item(subtree("Collapse all subfolders", false))
                            })
                            .into_any_element()
                        })
                }
            }
        });
        // The tree starts directly under the sidebar header: root folders
        // and files, no Folders row in between (#1512, #1474).
        let mut section_views = Vec::new();
        let rows: Vec<SideItem> = items;
        if !rows.is_empty() {
            let count = rows.len();
            let sticky_source = Rc::new(reader_tree_breadcrumbs::Ancestors::new(
                rows.iter()
                    .map(|item| match item {
                        SideItem::Tree(row) => Some(row.clone()),
                        _ => None,
                    })
                    .collect(),
            ));
            let render = render_row.clone();
            let list = uniform_list(
                SharedString::from("sidebar-body-Folders"),
                count,
                move |range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|ix| render(rows[ix].clone(), ix, cx))
                        .collect::<Vec<_>>()
                },
            );
            let observer = cx.entity().downgrade();
            let observed_root = self.vault_root.clone();
            let observed_generation = self.watcher_generation;
            let observed_tree_rows = self.tree.rows.clone();
            let observed_scroll = self.tree_scroll.clone();
            let last_first = std::cell::Cell::new(None);
            section_views.push(
                div()
                    .id("reader-tree")
                    .relative()
                    .debug_selector(|| "sidebar-tree-body".into())
                    .key_context("ReaderTree")
                    .track_focus(&self.tree_focus)
                    // A click below the last row clears the selection, so
                    // the next New note goes to the root (#1512).
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            if this.tree_space_below_rows(event.position, count) {
                                this.clear_tree_selection(cx);
                            }
                        }),
                    )
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(list.track_scroll(&self.tree_scroll).size_full())
                    .child({
                        // Auto-reveal waits while the tree is wheel-scrolled
                        // (#1762). Capture phase: the list still scrolls.
                        let wheel_observer = cx.entity().downgrade();
                        canvas(
                            |_, _, _| (),
                            move |bounds, _, window, _| {
                                let wheel_observer = wheel_observer.clone();
                                window.on_mouse_event(
                                    move |event: &ScrollWheelEvent, phase, _, cx| {
                                        if phase == DispatchPhase::Capture
                                            && bounds.contains(&event.position)
                                        {
                                            let _ = wheel_observer.update(cx, |this, cx| {
                                                this.tree_wheel_scrolled(cx)
                                            });
                                        }
                                    },
                                );
                            },
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                    })
                    .child(self.render_tree_breadcrumbs(window, cx))
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |_, _, _, cx| {
                                let offset =
                                    f32::from(observed_scroll.0.borrow().base_handle.offset().y);
                                let first = ((-offset / TREE_ROW_HEIGHT).floor().max(0.)) as usize;
                                // A pixel scroll never rebuilds ancestry. Only a new
                                // first row can change the one overlaid context strip.
                                if last_first.replace(Some(first)) == Some(first) {
                                    return;
                                }
                                let observer = observer.clone();
                                let observed_root = observed_root.clone();
                                let observed_tree_rows = observed_tree_rows.clone();
                                let (sticky, earlier_roots) = sticky_source.context(first);
                                cx.defer(move |cx| {
                                    let _ = observer.update(cx, |this, cx| {
                                        if this.vault_root != observed_root
                                            || this.watcher_generation != observed_generation
                                            || !Rc::ptr_eq(&this.tree.rows, &observed_tree_rows)
                                        {
                                            return;
                                        }
                                        if this
                                            .tree_sticky
                                            .iter()
                                            .map(|r| &r.path)
                                            .ne(sticky.iter().map(|r| &r.path))
                                            || this
                                                .tree_earlier_roots
                                                .iter()
                                                .map(|r| &r.path)
                                                .ne(earlier_roots.iter().map(|r| &r.path))
                                        {
                                            this.tree_sticky = sticky;
                                            this.tree_earlier_roots = earlier_roots;
                                            cx.notify();
                                        }
                                    });
                                });
                            },
                        )
                        .absolute()
                        .size_0(),
                    )
                    .into_any_element(),
            );
        }

        v_flex()
            .w_full()
            .h_full()
            .flex_none()
            .child(self.render_tree_header(cx))
            .children(self.render_batch_selection(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .pb_2()
                    .overflow_hidden()
                    .text_size(px(brand::READER_CHROME_FONT_SIZE))
                    .children(section_views),
            )
            .into_any_element()
    }
}
