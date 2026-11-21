//! The start screen (#3208): a launcher with the actions on the left and a
//! filterable recents list on the right, a first-launch page when there is
//! nothing to list, and a stacked layout for narrow windows.
//!
//! Spec: the «Welcome screen: interaction spec (combined design)» comment on
//! BeFeast/okilum #3208. The data (which vaults, why one is unavailable)
//! lives in `reader_recent_vaults` and `reader_startup`; this module draws it
//! and handles the keyboard.
use super::platform::labels::Os;
use super::reader_recent_vaults::{Fix, Status, Unavailable};
use super::reader_startup::{Entry, Row, Startup};
use super::{brand, reader_files, reader_open, reader_toast, vault_name};
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    notification::Notification,
    v_flex, Icon, IconName, Root, Sizable, TitleBar,
};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// The key context of the start screen: ⌘N, ⌘1–9 and the list keys are
/// bound here only, so nothing changes inside a vault window.
pub(crate) const CONTEXT: &str = "ReaderEntry";
const FILTER_CONTEXT: &str = "ReaderEntry > Input";

/// Below this window width the panes stack.
const NARROW: f32 = 640.;
/// The left pane of the wide layout.
const LEFT_PANE: f32 = 320.;
/// Unsaved drafts listed above the vaults before Show all.
const DRAFTS_LISTED: usize = 3;
/// PageUp / PageDown move this many rows.
const PAGE: usize = 5;

actions!(
    reader_entry,
    [
        EntryNext,
        EntryPrevious,
        EntryFirst,
        EntryLast,
        EntryPageDown,
        EntryPageUp,
        EntryOpen,
        EntryEscape,
        EntryRemove,
        EntryFocusNext,
        EntryFocusPrevious,
    ]
);

/// ⌘1…⌘9 (Ctrl+1…9): open the nth visible row.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = reader_entry, no_json, no_register)]
pub(crate) struct OpenRecent(pub usize);

pub(crate) fn install(cx: &mut App) {
    let mut bindings = Vec::new();
    for context in [CONTEXT, FILTER_CONTEXT] {
        let context = Some(context);
        bindings.extend([
            KeyBinding::new("down", EntryNext, context),
            KeyBinding::new("up", EntryPrevious, context),
            KeyBinding::new("pagedown", EntryPageDown, context),
            KeyBinding::new("pageup", EntryPageUp, context),
            KeyBinding::new("enter", EntryOpen, context),
            KeyBinding::new("secondary-down", EntryOpen, context),
            KeyBinding::new("escape", EntryEscape, context),
            KeyBinding::new("tab", EntryFocusNext, context),
            KeyBinding::new("shift-tab", EntryFocusPrevious, context),
            KeyBinding::new("secondary-n", super::NewUntitled, context),
        ]);
        for number in 1..=9 {
            bindings.push(KeyBinding::new(
                &format!("secondary-{number}"),
                OpenRecent(number),
                context,
            ));
        }
    }
    // Not in the filter field, where these keys edit the text.
    bindings.extend([
        KeyBinding::new("home", EntryFirst, Some(CONTEXT)),
        KeyBinding::new("end", EntryLast, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-backspace", EntryRemove, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("delete", EntryRemove, Some(CONTEXT)),
    ]);
    cx.bind_keys(bindings);
    // New file where no Reader handles it: the start screen (#3208). A vault
    // window's Reader handles `NewUntitled` itself and never reaches this.
    cx.on_action(|_: &super::NewUntitled, cx| super::reader_startup::new_file(cx));
}

/// The start screen's view state: focus, filter, selection.
pub(crate) struct View {
    list_focus: FocusHandle,
    actions_focus: FocusHandle,
    filter: Entity<InputState>,
    pub(crate) query: String,
    filter_focused: bool,
    /// The selected row among the visible rows.
    pub(crate) selected: usize,
    /// The selected action while the action list has focus.
    action: usize,
    pub(crate) show_all: bool,
    confirm_clear: bool,
    /// ⌘ (Ctrl) held for 500 ms: rows show their number instead of a time.
    numbers: bool,
    modifier_generation: u64,
    pub(crate) opening: Option<Row>,
    opening_spinner: bool,
    /// Try again found the volume still missing: its row says so briefly.
    still: Option<PathBuf>,
    /// The keyboard was used last: focus rings show.
    keyboard: bool,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl View {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Entry>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter recent vaults"));
        let subscription =
            cx.subscribe_in(
                &filter,
                window,
                |entry, input, event, window, cx| match event {
                    InputEvent::Change => {
                        let query = input.read(cx).value().to_string();
                        entry.set_query(query, cx);
                    }
                    InputEvent::PressEnter { .. } => entry.open_selected(window, cx),
                    InputEvent::Focus => {
                        entry.view.filter_focused = true;
                        cx.notify();
                    }
                    InputEvent::Blur => {
                        entry.view.filter_focused = false;
                        cx.notify();
                    }
                },
            );
        Self {
            list_focus: cx.focus_handle(),
            actions_focus: cx.focus_handle(),
            filter,
            query: String::new(),
            filter_focused: false,
            selected: 0,
            action: 0,
            show_all: false,
            confirm_clear: false,
            numbers: false,
            modifier_generation: 0,
            opening: None,
            opening_spinner: false,
            still: None,
            keyboard: false,
            scroll: ScrollHandle::new(),
            _subscriptions: vec![subscription],
        }
    }

    /// The recents list has focus, or the first-launch actions when there is
    /// nothing to list.
    pub(crate) fn focus_initial(&self, first_launch: bool, window: &mut Window, cx: &mut App) {
        if first_launch {
            self.actions_focus.focus(window, cx);
        } else {
            self.list_focus.focus(window, cx);
        }
    }

    /// Keep a subscription alive as long as the start screen.
    pub(crate) fn keep(&mut self, subscription: Subscription) {
        self._subscriptions.push(subscription);
    }

    pub(crate) fn clamp_selection(&mut self, rows: usize) {
        self.selected = self.selected.min(rows.saturating_sub(1));
    }

    pub(crate) fn cleared(&mut self) {
        self.confirm_clear = false;
        self.selected = 0;
        self.show_all = false;
    }

    /// Opening runs off the UI thread; after 300 ms the row shows a spinner.
    pub(crate) fn start_opening(&mut self, row: Row, cx: &mut Context<Entry>) {
        self.opening = Some(row.clone());
        self.opening_spinner = false;
        cx.spawn(async move |entry, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let _ = entry.update(cx, |entry, cx| {
                if entry.view.opening.as_ref() == Some(&row) {
                    entry.view.opening_spinner = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn still_unavailable(&mut self, root: PathBuf, cx: &mut Context<Entry>) {
        self.still = Some(root.clone());
        cx.spawn(async move |entry, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = entry.update(cx, |entry, cx| {
                if entry.view.still.as_ref() == Some(&root) {
                    entry.view.still = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A vault left the list: a 6 s toast offers Undo.
    pub(crate) fn removed(&mut self, root: PathBuf, index: usize, cx: &mut Context<Entry>) {
        let name = vault_name::shown(&root, cx);
        let Some(window) = cx.global::<Startup>().entry_window() else {
            return;
        };
        let entry = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                let notification = Notification::new()
                    .message(format!("Removed “{name}” from the list"))
                    .action(move |_, _, _| {
                        let entry = entry.clone();
                        let root = root.clone();
                        Button::new("startup-undo-remove")
                            .small()
                            .label("Undo")
                            .on_click(move |_, _, cx| {
                                let _ = entry.update(cx, |entry, cx| {
                                    entry.undo_remove(root.clone(), index, cx)
                                });
                            })
                    });
                reader_toast::push(notification, Some(Duration::from_secs(6)), window, cx);
            });
        });
    }
}

/// Non-publishing visual QA (#3208): `OKILUM_WELCOME_HARNESS` puts the
/// start screen into a state for a screenshot without input automation.
/// Parts separated by `;`: `query=<text>`, `selected=<n>`, `numbers`,
/// `show_all`, `confirm_clear`, `keyboard`.
#[cfg(feature = "welcome-ui-harness")]
pub(crate) fn apply_harness(entry: &mut Entry, window: &mut Window, cx: &mut Context<Entry>) {
    let Ok(spec) = std::env::var("OKILUM_WELCOME_HARNESS") else {
        return;
    };
    for part in spec.split(';').map(str::trim) {
        match part.split_once('=') {
            Some(("query", text)) => {
                entry
                    .view
                    .filter
                    .update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
                entry.view.query = text.to_owned();
            }
            Some(("selected", number)) => {
                if let Ok(number) = number.parse() {
                    entry.view.selected = number;
                }
            }
            None if part == "numbers" => entry.view.numbers = true,
            None if part == "show_all" => entry.view.show_all = true,
            None if part == "confirm_clear" => entry.view.confirm_clear = true,
            None if part == "keyboard" => entry.view.keyboard = true,
            _ => {}
        }
    }
}

/// How many drafts and vaults are listed, and how many wait behind Show
/// all: at most `LIMIT` rows, at most three of them drafts (#3208).
pub(crate) fn compose(drafts: usize, vaults: usize, show_all: bool) -> (usize, usize, usize) {
    if show_all {
        return (drafts, vaults, 0);
    }
    let limit = super::reader_recent_vaults::LIMIT;
    let shown_drafts = drafts.min(DRAFTS_LISTED).min(limit);
    let shown_vaults = vaults.min(limit - shown_drafts);
    (
        shown_drafts,
        shown_vaults,
        drafts + vaults - shown_drafts - shown_vaults,
    )
}

/// Where a filter query matches a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hit {
    /// In the name.
    Name,
    /// In the path.
    Path,
    /// The letters in order, not together.
    Loose,
}

/// Case-insensitive substring search; the byte range of the first match.
pub(crate) fn find_ci(haystack: &str, needle: &str) -> Option<Range<usize>> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() {
        return None;
    }
    haystack.char_indices().find_map(|(start, _)| {
        let mut chars = haystack[start..].char_indices();
        let mut end = start;
        for wanted in &needle {
            let (at, found) = chars.next()?;
            if !found.to_lowercase().eq(wanted.to_lowercase()) {
                return None;
            }
            end = start + at + found.len_utf8();
        }
        Some(start..end)
    })
}

fn subsequence(haystack: &str, needle: &str) -> bool {
    let mut haystack = haystack.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| haystack.any(|found| found == wanted))
}

/// Substring of the name, else of the path, else the letters in order.
pub(crate) fn hit(query: &str, name: &str, path: &str) -> Option<Hit> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    if find_ci(name, query).is_some() {
        return Some(Hit::Name);
    }
    if find_ci(path, query).is_some() {
        return Some(Hit::Path);
    }
    (subsequence(name, query) || subsequence(path, query)).then_some(Hit::Loose)
}

/// The rows on screen: drafts first, then vaults; filtered by the query (all
/// matches), else capped unless Show all.
pub(crate) fn visible_rows(entry: &Entry, cx: &App) -> Vec<Row> {
    let drafts = &entry.state.listing.drafts;
    let query = entry.view.query.trim();
    if query.is_empty() {
        let (shown_drafts, shown_vaults, _) =
            compose(drafts.len(), entry.roots.len(), entry.view.show_all);
        return drafts
            .iter()
            .take(shown_drafts)
            .map(|draft| Row::Draft(draft.file.clone()))
            .chain(
                entry
                    .roots
                    .iter()
                    .take(shown_vaults)
                    .map(|root| Row::Vault(root.clone())),
            )
            .collect();
    }
    let home = super::reader_untitled::home();
    drafts
        .iter()
        .filter(|draft| hit(query, &draft.label, "").is_some())
        .map(|draft| Row::Draft(draft.file.clone()))
        .chain(
            entry
                .roots
                .iter()
                .filter(|root| {
                    let path = ShownPath::of(root, home.as_deref()).full();
                    hit(query, &vault_name::shown(root, cx), &path).is_some()
                })
                .map(|root| Row::Vault(root.clone())),
        )
        .collect()
}

/// A path as the start screen shows it: an anchor that always stays (`~`,
/// `/Volumes/<name>`, `C:\`, `\\server\share`, `smb://server/share`) and the
/// folders below it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShownPath {
    anchor: String,
    segments: Vec<String>,
    separator: char,
}

impl ShownPath {
    pub(crate) fn of(root: &Path, home: Option<&Path>) -> Self {
        let plain = okilum_core::vault::plain_path(root);
        let separator = std::path::MAIN_SEPARATOR;
        let names = |path: &Path| -> Vec<String> {
            path.components()
                .filter_map(|part| match part {
                    Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect()
        };
        if let Some((mount, _, uri)) = super::reader_recent_vaults::gvfs_share(&plain) {
            return Self {
                anchor: format!("{uri}/"),
                segments: plain.strip_prefix(&mount).map(names).unwrap_or_default(),
                separator: '/',
            };
        }
        if !cfg!(windows) {
            if let Some(rest) = home.and_then(|home| plain.strip_prefix(home).ok()) {
                let segments = names(rest);
                return Self {
                    anchor: if segments.is_empty() {
                        "~".into()
                    } else {
                        format!("~{separator}")
                    },
                    segments,
                    separator,
                };
            }
            let parts = names(&plain);
            if plain.has_root() && parts.len() >= 2 && parts[0] == "Volumes" {
                return Self {
                    anchor: format!("/Volumes/{}/", parts[1]),
                    segments: parts[2..].to_vec(),
                    separator,
                };
            }
        }
        let mut anchor = String::new();
        for part in plain.components() {
            match part {
                Component::Prefix(prefix) => anchor.push_str(&prefix.as_os_str().to_string_lossy()),
                Component::RootDir => anchor.push(separator),
                _ => {}
            }
        }
        Self {
            anchor,
            segments: names(&plain),
            separator,
        }
    }

    fn join(&self, keep: impl Fn(usize) -> bool) -> String {
        let mut text = self.anchor.clone();
        let mut parts: Vec<&str> = Vec::new();
        let mut dropping = false;
        for (index, segment) in self.segments.iter().enumerate() {
            if keep(index) {
                parts.push(segment);
                dropping = false;
            } else if !dropping {
                parts.push("…");
                dropping = true;
            }
        }
        text.push_str(&parts.join(&self.separator.to_string()));
        text
    }

    pub(crate) fn full(&self) -> String {
        self.join(|_| true)
    }

    /// The segment that holds `query`, kept while filtering.
    pub(crate) fn segment_with(&self, query: &str) -> Option<usize> {
        let query = query.trim();
        if query.is_empty() {
            return None;
        }
        self.segments
            .iter()
            .position(|segment| find_ci(segment, query).is_some())
    }

    /// The longest spelling that fits: whole middle segments are dropped
    /// starting next to the anchor, so the deepest folders stay longest; the
    /// last segment and `keep` always stay. When even anchor + … + last does
    /// not fit, that is returned and the element middle-ellipsizes it.
    pub(crate) fn shortened(&self, keep: Option<usize>, fits: impl Fn(&str) -> bool) -> String {
        let full = self.full();
        let count = self.segments.len();
        if count <= 1 || fits(&full) {
            return full;
        }
        let last = count - 1;
        let mut text = full;
        for dropped in 1..count {
            text = self.join(|index| index >= dropped || index == last || Some(index) == keep);
            if fits(&text) {
                break;
            }
        }
        text
    }
}

/// «Today», «Yesterday», a weekday within a week, «Nov 8» this year, else
/// the date (#3208).
pub(crate) fn relative_time(seconds: u64, now: chrono::DateTime<chrono::Local>) -> String {
    use chrono::Datelike;
    let Some(then) = i64::try_from(seconds)
        .ok()
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
    else {
        return String::new();
    };
    let then = then.with_timezone(&chrono::Local);
    let days = (now.date_naive() - then.date_naive()).num_days();
    match days {
        ..=0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => then.format("%A").to_string(),
        _ if then.year() == now.year() => then.format("%b %-d").to_string(),
        _ => then.format("%Y-%m-%d").to_string(),
    }
}

/// The modifier key as rows and hints spell it.
fn secondary_label() -> &'static str {
    if matches!(Os::CURRENT, Os::Mac) {
        "⌘"
    } else {
        "Ctrl+"
    }
}

fn text_width(text: &str, size: Pixels, weight: FontWeight, window: &Window) -> f32 {
    let mut font = window.text_style().font();
    font.weight = weight;
    let run = TextRun {
        len: text.len(),
        font,
        color: black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    f32::from(
        window
            .text_system()
            .shape_line(text.to_owned().into(), size, &[run], None)
            .width(),
    )
}

/// One action of the launcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Launch {
    OpenFolder,
    OpenFile,
    NewFile,
    NewVault,
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    Sync,
}

impl Launch {
    const ALL: &[Launch] = &[
        Launch::OpenFolder,
        Launch::OpenFile,
        Launch::NewFile,
        Launch::NewVault,
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        Launch::Sync,
    ];

    fn label(self) -> &'static str {
        match self {
            Launch::OpenFolder => "Open folder…",
            Launch::OpenFile => "Open file…",
            Launch::NewFile => "New file",
            Launch::NewVault => "New vault…",
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            Launch::Sync => "Get notes from another computer",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Launch::OpenFolder => "Open a folder of notes as a vault",
            Launch::OpenFile => "Read or edit one file, without a vault",
            Launch::NewFile => "Start writing now, save it anywhere later",
            Launch::NewVault => "Make a new folder for your notes",
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            Launch::Sync => "Pair with Okilum on your other computer",
        }
    }

    fn keystroke(self) -> Option<&'static str> {
        match self {
            Launch::OpenFolder => Some("secondary-shift-o"),
            Launch::OpenFile => Some("secondary-o"),
            Launch::NewFile => Some("secondary-n"),
            _ => None,
        }
    }

    fn icon(self) -> Icon {
        match self {
            Launch::OpenFolder => Icon::new(IconName::FolderOpen),
            Launch::OpenFile => Icon::new(IconName::File),
            Launch::NewFile => Icon::default().path("icons/file-plus.svg"),
            Launch::NewVault => Icon::default().path("icons/folder-plus.svg"),
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            Launch::Sync => Icon::new(IconName::Network),
        }
    }

    fn run(self, window: &mut Window, cx: &mut App) {
        match self {
            Launch::OpenFolder => window.dispatch_action(Box::new(reader_open::OpenFolder), cx),
            Launch::OpenFile => window.dispatch_action(Box::new(reader_open::OpenFile), cx),
            Launch::NewFile => super::reader_startup::new_file(cx),
            Launch::NewVault => window.dispatch_action(Box::new(reader_open::NewVault), cx),
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            Launch::Sync => super::reader_settings::show_sync_pairing(window, cx),
        }
    }
}

/// What a row shows, worked out before its element is built.
struct Shown {
    row: Row,
    name: String,
    name_hit: Option<Range<usize>>,
    second: String,
    second_hit: Option<Range<usize>>,
    tooltip: String,
    time: Option<String>,
    status: Option<Status>,
    color: Option<Hsla>,
    default: bool,
}

impl Entry {
    pub(crate) fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        if query != self.view.query {
            self.view.query = query;
            self.view.selected = 0;
            self.view.confirm_clear = false;
            cx.notify();
        }
    }

    fn clear_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view
            .filter
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.set_query(String::new(), cx);
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.rows(cx).get(self.view.selected).cloned() {
            self.activate(row, window, cx);
        }
    }

    /// ⌘n: the nth visible row, so it also works on a filtered list.
    pub(crate) fn open_nth(&mut self, number: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = number.checked_sub(1) else {
            return;
        };
        if let Some(row) = self.rows(cx).get(index).cloned() {
            self.view.selected = index;
            self.activate(row, window, cx);
        }
    }

    /// Click or Enter on a row: open it; an unavailable vault runs its fix
    /// (Couldn't open tries again).
    fn activate(&mut self, row: Row, window: &mut Window, cx: &mut Context<Self>) {
        if let Row::Vault(root) = &row {
            if let Some(reason) = self.state.unavailable(root) {
                let fix = match reason.action() {
                    Fix::Details => None,
                    fix => Some(fix),
                };
                if let Some(fix) = fix {
                    self.run_fix(root.clone(), fix, window, cx);
                    return;
                }
            }
        }
        self.open_row(row, cx);
    }

    fn run_fix(&mut self, root: PathBuf, fix: Fix, window: &mut Window, cx: &mut Context<Self>) {
        match fix {
            Fix::TryAgain => self.try_again(root, cx),
            Fix::Locate | Fix::AllowAccess => self.locate(root, cx),
            Fix::Details => {
                if let Some(reason) = self.state.status.get(&root).and_then(Status::reason) {
                    reader_toast::error(
                        format!("{}\n{reason}", okilum_core::vault::display_path(&root)),
                        window,
                        cx,
                    );
                }
            }
        }
    }

    fn remove_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.rows(cx).get(self.view.selected).cloned() {
            Some(Row::Vault(root)) => self.remove(root, cx),
            Some(Row::Draft(file)) => self.confirm_discard(file, window, cx),
            None => {}
        }
    }

    fn confirm_discard(&mut self, file: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let label = self
            .state
            .listing
            .drafts
            .iter()
            .find(|draft| draft.file == file)
            .map_or_else(|| "Untitled".to_owned(), |draft| draft.label.clone());
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Discard “{label}”?"),
            Some("Its text is deleted."),
            &["Discard draft", "Cancel"],
            cx,
        );
        cx.spawn(async move |entry, cx| {
            if answer.await.ok() == Some(0) {
                let _ = entry.update(cx, |entry, cx| entry.discard_draft(file, cx));
            }
        })
        .detach();
    }

    fn list_focused(&self, window: &Window) -> bool {
        self.view.list_focus.is_focused(window)
    }

    fn move_selection(
        &mut self,
        to: impl FnOnce(usize, usize) -> usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.view.keyboard = true;
        if self.view.actions_focus.is_focused(window) {
            let count = Launch::ALL.len();
            self.view.action = to(self.view.action, count).min(count - 1);
        } else {
            let count = self.rows(cx).len();
            if count == 0 {
                return;
            }
            self.view.selected = to(self.view.selected, count).min(count - 1);
            self.view.scroll.scroll_to_item(self.view.selected);
        }
        cx.notify();
    }

    fn focus_step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.view.keyboard = true;
        let first_launch = self.first_launch();
        // Action list → filter field → recents list.
        let current = if self.view.actions_focus.is_focused(window) {
            0
        } else if self.view.filter_focused {
            1
        } else {
            2
        };
        let next = if first_launch {
            0
        } else if forward {
            (current + 1) % 3
        } else {
            (current + 2) % 3
        };
        match next {
            0 => self.view.actions_focus.focus(window, cx),
            1 => self
                .view
                .filter
                .update(cx, |input, cx| input.focus(window, cx)),
            _ => self.view.list_focus.focus(window, cx),
        }
        cx.notify();
    }

    /// A printable key typed in the action or recents list goes into the
    /// filter field.
    fn type_to_filter(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.view.filter_focused || self.first_launch() {
            return;
        }
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.control || modifiers.platform || modifiers.alt || modifiers.function {
            return;
        }
        let Some(typed) = keystroke.key_char.as_deref() else {
            return;
        };
        if typed.is_empty() || typed.chars().any(char::is_control) || typed.trim().is_empty() {
            return;
        }
        let query = format!("{}{typed}", self.view.query);
        self.view.filter.update(cx, |input, cx| {
            input.set_value(query.clone(), window, cx);
            input.focus(window, cx);
        });
        self.set_query(query, cx);
        cx.stop_propagation();
    }

    fn modifiers_changed(&mut self, event: &ModifiersChangedEvent, cx: &mut Context<Self>) {
        let modifiers = event.modifiers;
        self.view.modifier_generation += 1;
        let held = modifiers.secondary() && !modifiers.shift && !modifiers.alt;
        if !held {
            if self.view.numbers {
                self.view.numbers = false;
                cx.notify();
            }
            return;
        }
        let generation = self.view.modifier_generation;
        cx.spawn(async move |entry, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = entry.update(cx, |entry, cx| {
                if entry.view.modifier_generation == generation {
                    entry.view.numbers = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn shown_rows(&self, rows: &[Row], path_width: f32, window: &Window, cx: &App) -> Vec<Shown> {
        let home = super::reader_untitled::home();
        let now = chrono::Local::now();
        let query = self.view.query.trim();
        rows.iter()
            .map(|row| match row {
                Row::Draft(file) => {
                    let draft = self
                        .state
                        .listing
                        .drafts
                        .iter()
                        .find(|draft| &draft.file == file);
                    let name =
                        draft.map_or_else(|| "Untitled".to_owned(), |draft| draft.label.clone());
                    Shown {
                        row: row.clone(),
                        name_hit: find_ci(&name, query),
                        name,
                        second: "Unsaved draft · not in a folder yet".into(),
                        second_hit: None,
                        tooltip: "Unsaved draft".into(),
                        time: draft.map(|draft| relative_time(draft.modified, now)),
                        status: None,
                        color: None,
                        default: false,
                    }
                }
                Row::Vault(root) => {
                    let name = vault_name::shown(root, cx);
                    let status = self.state.status.get(root).cloned();
                    let path = ShownPath::of(root, home.as_deref());
                    let keep = path.segment_with(query);
                    let read_only = status.as_ref().is_some_and(Status::read_only);
                    let prefix = if read_only { "Read-only · " } else { "" };
                    let fits = |text: &str| {
                        text_width(
                            &format!("{prefix}{text}"),
                            px(12.),
                            FontWeight::NORMAL,
                            window,
                        ) <= path_width
                    };
                    let shortened = path.shortened(keep, fits);
                    let second = match status.as_ref().and_then(Status::reason) {
                        Some(_) if self.view.still.as_ref() == Some(root) => {
                            "Still not connected".to_owned()
                        }
                        Some(reason) => reason,
                        None => format!("{prefix}{shortened}"),
                    };
                    let second_hit = status
                        .as_ref()
                        .and_then(Status::unavailable)
                        .is_none()
                        .then(|| find_ci(&second, query))
                        .flatten();
                    Shown {
                        row: row.clone(),
                        name_hit: find_ci(&name, query),
                        name,
                        second,
                        second_hit,
                        tooltip: vault_name::path_tooltip(root),
                        time: self
                            .state
                            .listing
                            .last_opened
                            .get(root)
                            .map(|at| relative_time(*at, now)),
                        status,
                        color: crate::reader_ui_state::vault_color(root, cx)
                            .map(|color| color.color(cx)),
                        default: self.state.listing.default_root.as_ref() == Some(root),
                    }
                }
            })
            .collect()
    }
}

impl Render for Entry {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = brand::palette(cx);
        let viewport = f32::from(window.viewport_size().width);
        let narrow = viewport < NARROW;
        let body = if self.first_launch() {
            self.first_launch_page(cx)
        } else if narrow {
            v_flex()
                .id("startup-narrow")
                .size_full()
                .overflow_y_scroll()
                .bg(p.sidebar)
                .child(self.left_pane(true, window, cx))
                .child(self.recents_pane(true, viewport, window, cx))
                .into_any_element()
        } else {
            h_flex()
                .size_full()
                .child(self.left_pane(false, window, cx))
                .child(self.recents_pane(false, viewport - LEFT_PANE - 1., window, cx))
                .into_any_element()
        };
        div()
            .id("startup-entry")
            .key_context(CONTEXT)
            .relative()
            .size_full()
            .bg(p.canvas)
            .text_color(p.text)
            .on_action(cx.listener(|this, _: &EntryNext, window, cx| {
                this.move_selection(|at, _| at + 1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &EntryPrevious, window, cx| {
                this.move_selection(|at, _| at.saturating_sub(1), window, cx)
            }))
            .on_action(cx.listener(|this, _: &EntryFirst, window, cx| {
                this.move_selection(|_, _| 0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &EntryLast, window, cx| {
                this.move_selection(|_, count| count.saturating_sub(1), window, cx)
            }))
            .on_action(cx.listener(|this, _: &EntryPageDown, window, cx| {
                this.move_selection(|at, _| at + PAGE, window, cx)
            }))
            .on_action(cx.listener(|this, _: &EntryPageUp, window, cx| {
                this.move_selection(|at, _| at.saturating_sub(PAGE), window, cx)
            }))
            .on_action(cx.listener(|this, _: &EntryOpen, window, cx| {
                this.view.keyboard = true;
                if this.view.actions_focus.is_focused(window) || this.first_launch() {
                    let action = Launch::ALL[this.view.action.min(Launch::ALL.len() - 1)];
                    action.run(window, cx);
                } else {
                    this.open_selected(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &EntryEscape, window, cx| {
                if !this.view.query.is_empty() {
                    this.clear_query(window, cx);
                } else if this.view.filter_focused {
                    this.view.list_focus.focus(window, cx);
                    cx.notify();
                } else if this.view.confirm_clear {
                    this.view.confirm_clear = false;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &EntryRemove, window, cx| {
                if this.list_focused(window) {
                    this.remove_selected(window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &EntryFocusNext, window, cx| {
                    this.focus_step(true, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &EntryFocusPrevious, window, cx| {
                this.focus_step(false, window, cx)
            }))
            .on_action(cx.listener(|this, action: &OpenRecent, window, cx| {
                this.open_nth(action.0, window, cx)
            }))
            .on_action(
                cx.listener(|_, _: &super::NewUntitled, _, cx| super::reader_startup::new_file(cx)),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.type_to_filter(event, window, cx)
            }))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                this.modifiers_changed(event, cx)
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.view.keyboard = false),
            )
            .child(body)
            // The transparent title strip: traffic lights on macOS, caption
            // buttons on Windows, toolkit controls with Linux CSD.
            .child(
                div().absolute().top_0().left_0().right_0().child(
                    TitleBar::new()
                        .h(px(super::READER_HEADER_HEIGHT))
                        .bg(transparent_black())
                        .border_color(transparent_black())
                        .when(window.is_fullscreen(), |bar| bar.pl(px(10.))),
                ),
            )
            .children(Root::render_notification_layer(window, cx))
    }
}

impl Entry {
    fn version_line() -> String {
        format!(
            "Version {} · {}",
            env!("OKILUM_RELEASE_VERSION"),
            crate::updater::channel()
        )
    }

    fn left_pane(
        &mut self,
        narrow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        let header = if narrow {
            h_flex()
                .gap_2()
                .min_w_0()
                .child(super::brand::logo(30., cx))
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Okilum"),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(12.))
                        .text_color(faint)
                        .child(Self::version_line()),
                )
                .into_any_element()
        } else {
            v_flex()
                .gap_1()
                .child(super::brand::logo(46., cx))
                .child(
                    div()
                        .pt_2()
                        .text_size(px(22.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Okilum"),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(faint)
                        .child(Self::version_line()),
                )
                .into_any_element()
        };
        let actions_focused = self.view.actions_focus.is_focused(window);
        let mut list = v_flex()
            .id("startup-actions")
            .track_focus(&self.view.actions_focus)
            .gap_0p5();
        for (index, action) in Launch::ALL.iter().copied().enumerate() {
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            {
                if action == Launch::Sync {
                    list = list.child(div().my_2().h(px(1.)).bg(p.border_subtle));
                }
            }
            let selected = actions_focused && self.view.keyboard && self.view.action == index;
            list = list.child(self.action_row(index, action, narrow, selected, cx));
        }
        v_flex()
            .when(!narrow, |pane| pane.w(px(LEFT_PANE)).flex_none().h_full())
            .when(narrow, |pane| pane.w_full())
            .bg(p.canvas)
            .px_4()
            .pt(px(
                super::READER_HEADER_HEIGHT + if narrow { 0. } else { 12. }
            ))
            .pb_4()
            .gap(px(if narrow { 12. } else { 20. }))
            .child(header)
            .child(list)
            .when(!narrow, |pane| {
                pane.child(div().flex_1()).child(
                    div()
                        .text_size(px(12.))
                        .text_color(faint)
                        .child("A vault is a folder of Markdown files."),
                )
            })
            .into_any_element()
    }

    fn action_row(
        &self,
        index: usize,
        action: Launch,
        compact: bool,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = brand::palette(cx);
        let r = brand::reader_palette(cx);
        let primary = action == Launch::OpenFolder;
        let tile = if compact { 28. } else { 34. };
        let shortcut = action.keystroke().map(|keys| Os::CURRENT.shortcut(keys));
        h_flex()
            .id(("startup-action", index))
            .debug_selector(move || format!("startup-action-{index}"))
            .w_full()
            .min_w_0()
            .h(px(if compact { 40. } else { 50. }))
            .px_2()
            .gap_3()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|row| row.bg(r.hover))
            .when(selected, |row| {
                row.bg(p.selected).border_2().border_color(p.focus)
            })
            .on_click(move |_, window, cx| action.run(window, cx))
            .when_some(shortcut.clone(), |row, shortcut| {
                row.tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(shortcut.clone()).build(window, cx)
                })
            })
            .child(
                div()
                    .flex_none()
                    .size(px(tile))
                    .rounded(px(8.))
                    .bg(if primary { p.accent } else { p.tile })
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(action.icon().size_4().text_color(if primary {
                        p.on_accent
                    } else {
                        p.accent
                    })),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::MEDIUM)
                            .child(action.label()),
                    )
                    .when(!compact, |text| {
                        text.child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(px(12.))
                                .text_color(p.text_muted)
                                .child(action.description()),
                        )
                    }),
            )
            .children(shortcut.map(|shortcut| {
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(r.text_faint)
                    .child(shortcut)
            }))
            .into_any_element()
    }

    fn recents_pane(
        &mut self,
        narrow: bool,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        let rows = self.rows(cx);
        self.view.clamp_selection(rows.len());
        let show_time = width >= 300.;
        // List and row padding, icon, gaps and the trailing slot.
        let path_width =
            (width - 16. - 24. - 16. - 24. - if show_time { 84. } else { 8. }).max(60.);
        let shown = self.shown_rows(&rows, path_width, window, cx);
        let list_focused = self.list_focused(window);
        let mut list = div()
            .id("startup-recents")
            .track_focus(&self.view.list_focus)
            .w_full()
            .px_2()
            .when(!narrow, |list| {
                list.flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.view.scroll)
            });
        let entity = cx.entity().downgrade();
        for (index, item) in shown.into_iter().enumerate() {
            let selected = index == self.view.selected;
            list = list.child(self.recent_row(
                index,
                item,
                selected,
                selected && list_focused && self.view.keyboard,
                show_time,
                entity.clone(),
                cx,
            ));
        }
        let query = self.view.query.trim().to_owned();
        let (_, _, waiting) = compose(
            self.state.listing.drafts.len(),
            self.roots.len(),
            self.view.show_all,
        );
        if query.is_empty() && waiting > 0 {
            let total = self.state.listing.drafts.len() + self.roots.len();
            list = list.child(
                div().px_3().py_2().child(
                    Button::new("startup-show-all")
                        .ghost()
                        .xsmall()
                        .label(format!("Show all ({total})"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.view.show_all = true;
                            cx.notify();
                        })),
                ),
            );
        }
        if !query.is_empty() && rows.is_empty() {
            list = list.child(
                div()
                    .px_3()
                    .py_4()
                    .text_size(px(13.))
                    .text_color(p.text_muted)
                    .child(format!("No recent vault matches “{query}”")),
            );
        }
        let filter = div().px_3().pb_2().child(
            Input::new(&self.view.filter)
                .small()
                .prefix(Icon::new(IconName::Search).size_3p5().text_color(faint))
                .when(
                    self.view.query.is_empty() && !self.view.filter_focused,
                    |input| {
                        input.suffix(
                            div()
                                .text_size(px(11.))
                                .text_color(faint)
                                .child("type to filter"),
                        )
                    },
                ),
        );
        v_flex()
            .when(!narrow, |pane| {
                pane.flex_1()
                    .min_w_0()
                    .h_full()
                    .border_l_1()
                    .border_color(p.border_subtle)
                    .pt(px(super::READER_HEADER_HEIGHT))
            })
            .when(narrow, |pane| {
                pane.w_full()
                    .pt_2()
                    .border_t_1()
                    .border_color(p.border_subtle)
            })
            .bg(p.sidebar)
            .child(filter)
            .children(self.state.notice.as_ref().map(|notice| {
                div()
                    .px_4()
                    .pb_2()
                    .text_size(px(12.))
                    .text_color(p.text_muted)
                    .child(notice.text.clone())
            }))
            .child(list)
            .child(
                self.footer(
                    width,
                    rows.iter()
                        .filter(|row| matches!(row, Row::Vault(_)))
                        .count(),
                    cx,
                ),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn recent_row(
        &self,
        index: usize,
        item: Shown,
        selected: bool,
        ring: bool,
        show_time: bool,
        entity: WeakEntity<Entry>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = brand::palette(cx);
        let r = brand::reader_palette(cx);
        let unavailable = item.status.as_ref().and_then(Status::unavailable).cloned();
        let read_only = item.status.as_ref().is_some_and(Status::read_only);
        let on = |color: Hsla| if selected { p.on_accent } else { color };
        let group: SharedString = format!("startup-row-{index}").into();
        let icon = match (&item.row, &unavailable) {
            (Row::Draft(_), _) => Icon::new(IconName::FileText),
            (_, Some(Unavailable::VolumeMissing { .. } | Unavailable::NotResponding)) => {
                Icon::new(IconName::HardDrive)
            }
            (_, Some(Unavailable::NoAccess { .. })) => Icon::default().path("icons/lock.svg"),
            (_, Some(Unavailable::Missing | Unavailable::NotAFolder)) => {
                Icon::new(IconName::FolderClosed)
            }
            (_, Some(Unavailable::CantOpen(_))) => Icon::new(IconName::TriangleAlert),
            (_, None) => Icon::new(IconName::Folder),
        };
        let icon_color = if unavailable.is_some() {
            on(p.text_muted)
        } else {
            on(item.color.unwrap_or(p.text_muted))
        };
        let highlight = |text: String, hit: Option<Range<usize>>| {
            let mut styled = StyledText::new(text);
            if let Some(range) = hit {
                styled = styled.with_highlights([(
                    range,
                    HighlightStyle {
                        color: Some(if selected { p.on_accent } else { p.link }),
                        font_weight: Some(FontWeight::SEMIBOLD),
                        underline: selected.then(|| UnderlineStyle {
                            thickness: px(1.),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )]);
            }
            styled
        };
        let tag = |text: &'static str| {
            div()
                .flex_none()
                .px_1p5()
                .rounded(px(4.))
                .bg(if selected {
                    p.on_accent.opacity(0.22)
                } else {
                    p.tile
                })
                .text_size(px(11.))
                .text_color(on(p.text_muted))
                .child(text)
        };
        let name_line = h_flex()
            .gap_1p5()
            .min_w_0()
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(13.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if unavailable.is_some() {
                        on(p.text_muted)
                    } else {
                        on(p.text)
                    })
                    .child(highlight(item.name.clone(), item.name_hit.clone())),
            )
            .when(read_only, |line| {
                line.child(
                    Icon::default()
                        .path("icons/lock.svg")
                        .size_3()
                        .text_color(on(p.text_muted)),
                )
            })
            .when(matches!(item.row, Row::Draft(_)), |line| {
                line.child(tag("Unsaved"))
            })
            .when(item.default, |line| line.child(tag("Default")));
        let second = div()
            .min_w_0()
            .overflow_hidden()
            .text_size(px(12.))
            .text_color(if unavailable.is_some() {
                on(p.warning)
            } else {
                on(p.text_muted)
            })
            .when(unavailable.is_none(), |line| {
                line.whitespace_nowrap().text_ellipsis_middle()
            })
            .child(highlight(item.second.clone(), item.second_hit.clone()));
        let row_for_menu = item.row.clone();
        let menu_entity = entity.clone();
        let available = unavailable.is_none();
        let menu_button = Button::new(("startup-row-menu", index))
            .ghost()
            .xsmall()
            .icon(IconName::Ellipsis)
            .tooltip("More")
            .dropdown_menu(move |menu, _, _| {
                row_menu(menu, menu_entity.clone(), row_for_menu.clone(), available)
            });
        let trailing = if let Some(reason) = &unavailable {
            let fix = reason.action();
            let root = match &item.row {
                Row::Vault(root) => Some(root.clone()),
                Row::Draft(_) => None,
            };
            let fix_entity = entity.clone();
            h_flex()
                .flex_none()
                .gap_1()
                .child(
                    Button::new(("startup-fix", index))
                        .xsmall()
                        .label(fix.label())
                        .on_click(move |_, window, cx| {
                            if let Some(root) = root.clone() {
                                let _ = fix_entity
                                    .update(cx, |entry, cx| entry.run_fix(root, fix, window, cx));
                            }
                        }),
                )
                .child(
                    div()
                        .invisible()
                        .group_hover(group.clone(), |slot| slot.visible())
                        .child(menu_button),
                )
                .into_any_element()
        } else {
            let label = if self.view.numbers && index < 9 {
                Some(format!("{}{}", secondary_label(), index + 1))
            } else {
                item.time.clone().filter(|_| show_time)
            };
            let opening =
                self.view.opening.as_ref() == Some(&item.row) && self.view.opening_spinner;
            div()
                .flex_none()
                .relative()
                .min_w(px(28.))
                .h_full()
                .flex()
                .items_center()
                .justify_end()
                .child(
                    div()
                        .group_hover(group.clone(), |slot| slot.invisible())
                        .text_size(px(11.5))
                        .text_color(on(r.text_faint))
                        .whitespace_nowrap()
                        .map(|slot| {
                            if opening {
                                slot.child(
                                    h_flex()
                                        .gap_1()
                                        .child(Icon::new(IconName::LoaderCircle).size_3())
                                        .child("Opening…"),
                                )
                            } else {
                                slot.children(label)
                            }
                        }),
                )
                .child(
                    div()
                        .absolute()
                        .right_0()
                        .invisible()
                        .group_hover(group.clone(), |slot| slot.visible())
                        .child(menu_button),
                )
                .into_any_element()
        };
        let open_row = item.row.clone();
        let menu_row = item.row.clone();
        let menu_entity = entity.clone();
        let tooltip = item.tooltip.clone();
        h_flex()
            .id(("startup-recent", index))
            .debug_selector(move || format!("startup-recent-{index}"))
            .group(group)
            .w_full()
            .min_w_0()
            .min_h(px(48.))
            .px_3()
            .py_1()
            .gap_3()
            .rounded(px(8.))
            .cursor_pointer()
            .when(selected, |row| row.bg(p.accent))
            .when(!selected, |row| row.hover(|row| row.bg(r.hover)))
            .when(ring, |row| row.border_2().border_color(p.focus))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.view.selected = index;
                this.activate(open_row.clone(), window, cx);
            }))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .context_menu(move |menu, _, _| {
                row_menu(menu, menu_entity.clone(), menu_row.clone(), available)
            })
            .child(icon.size_4().flex_none().text_color(icon_color))
            .child(v_flex().flex_1().min_w_0().child(name_line).child(second))
            .child(trailing)
            .into_any_element()
    }

    fn footer(&self, width: f32, matched: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        let vaults = self.roots.len();
        if self.view.confirm_clear {
            return h_flex()
                .flex_none()
                .flex_wrap()
                .px_3()
                .py_2()
                .gap_2()
                .border_t_1()
                .border_color(p.border_subtle)
                .child(
                    div()
                        .flex_1()
                        .min_w(px(160.))
                        .text_size(px(12.))
                        .text_color(p.text)
                        .child(format!(
                            "Remove all {vaults} vaults from this list? Folders on disk are not touched."
                        )),
                )
                .child(
                    Button::new("startup-clear-confirm")
                        .danger()
                        .xsmall()
                        .label("Clear list")
                        .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
                )
                .child(
                    Button::new("startup-clear-cancel")
                        .ghost()
                        .xsmall()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.view.confirm_clear = false;
                            cx.notify();
                        })),
                )
                .into_any_element();
        }
        let key = secondary_label();
        let hint = if !self.view.query.trim().is_empty() {
            format!("{matched} of {vaults} vaults")
        } else if width < 420. {
            "↵ open".into()
        } else {
            format!("↑↓ select · ↵ open · {key}1–9")
        };
        h_flex()
            .flex_none()
            .h(px(36.))
            .px_3()
            .gap_2()
            .border_t_1()
            .border_color(p.border_subtle)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(11.5))
                    .text_color(faint)
                    .child(hint),
            )
            .when(vaults > 0, |footer| {
                footer.child(
                    Button::new("startup-clear-list")
                        .ghost()
                        .xsmall()
                        .label("Clear list")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.view.confirm_clear = true;
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    fn first_launch_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        let new_file_key = Os::CURRENT.shortcut("secondary-n");
        div()
            .id("startup-first-launch")
            .track_focus(&self.view.actions_focus)
            .size_full()
            .overflow_y_scroll()
            .bg(p.canvas)
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(520.))
                    .mx_auto()
                    .px_4()
                    .pt(px(super::READER_HEADER_HEIGHT + 40.))
                    .pb_8()
                    .gap_4()
                    .items_center()
                    .child(super::brand::logo(44., cx))
                    .child(
                        div()
                            .text_size(px(24.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Welcome to Okilum"),
                    )
                    .child(div().text_center().text_color(p.text_muted).child(
                        "Okilum reads and edits the Markdown files you already have, \
                                 right where they are. Nothing is imported or converted.",
                    ))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .justify_center()
                            .gap_2()
                            .child(
                                Button::new("startup-open-folder")
                                    .primary()
                                    .label("Open folder…")
                                    .on_click(|_, window, cx| Launch::OpenFolder.run(window, cx)),
                            )
                            .child(
                                Button::new("startup-new-vault")
                                    .label("Create a new vault…")
                                    .on_click(|_, window, cx| Launch::NewVault.run(window, cx)),
                            )
                            .child(
                                Button::new("startup-open-file")
                                    .label("Open file…")
                                    .on_click(|_, window, cx| Launch::OpenFile.run(window, cx)),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .justify_center()
                            .gap_1()
                            .text_size(px(13.))
                            .text_color(p.text_muted)
                            .child("Or start writing right away:")
                            .child(
                                Button::new("startup-new-file")
                                    .link()
                                    .label("New file")
                                    .on_click(|_, window, cx| Launch::NewFile.run(window, cx)),
                            )
                            .child(div().text_color(faint).child(new_file_key))
                            .child(", and save it anywhere later."),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .p_4()
                            .gap_1()
                            .rounded(px(10.))
                            .bg(p.tile)
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("What’s a vault?"),
                            )
                            .child(div().text_size(px(13.)).text_color(p.text_muted).child(
                                "Any folder of .md files: your notes folder, an Obsidian \
                                     vault, or an empty folder to start fresh.",
                            )),
                    )
                    .when(
                        cfg!(any(target_os = "linux", target_os = "macos", windows)),
                        |page| {
                            page.child(
                                h_flex()
                                    .w_full()
                                    .flex_wrap()
                                    .p_3()
                                    .gap_1()
                                    .rounded(px(10.))
                                    .border_1()
                                    .border_color(p.border_subtle)
                                    .text_size(px(13.))
                                    .text_color(p.text_muted)
                                    .child("Already use Okilum on another computer?")
                                    .child(
                                        Button::new("startup-sync-pairing")
                                            .debug_selector(|| "startup-sync-pairing".into())
                                            .link()
                                            .label("Get your notes from it →")
                                            .on_click(|_, window, cx| {
                                                #[cfg(any(
                                                    target_os = "linux",
                                                    target_os = "macos",
                                                    windows
                                                ))]
                                                super::reader_settings::show_sync_pairing(
                                                    window, cx,
                                                );
                                                #[cfg(not(any(
                                                    target_os = "linux",
                                                    target_os = "macos",
                                                    windows
                                                )))]
                                                let _ = (window, cx);
                                            }),
                                    ),
                            )
                        },
                    )
                    .children(self.state.notice.as_ref().map(|notice| {
                        div()
                            .text_center()
                            .text_size(px(12.))
                            .text_color(p.text_muted)
                            .child(notice.text.clone())
                    })),
            )
            .into_any_element()
    }
}

/// The row menu (⋯ and right-click): Open, reveal, Copy path, Remove; on a
/// draft Open, Save as…, Discard draft….
fn row_menu(menu: PopupMenu, entry: WeakEntity<Entry>, row: Row, available: bool) -> PopupMenu {
    let open_entry = entry.clone();
    let open_row = row.clone();
    let menu = menu.item(PopupMenuItem::new("Open").on_click(move |_, window, cx| {
        let _ = open_entry.update(cx, |entry, cx| entry.activate(open_row.clone(), window, cx));
    }));
    match row {
        Row::Vault(root) => {
            let reveal = root.clone();
            let copy = okilum_core::vault::display_path(&root);
            menu.item(
                PopupMenuItem::new(Os::CURRENT.reveal())
                    .disabled(!available)
                    .on_click(move |_, window, cx| reader_files::reveal(&reveal, window, cx)),
            )
            .item(PopupMenuItem::new("Copy path").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
            }))
            .separator()
            .item(
                PopupMenuItem::new("Remove from list").on_click(move |_, _, cx| {
                    let _ = entry.update(cx, |entry, cx| entry.remove(root.clone(), cx));
                }),
            )
        }
        Row::Draft(file) => {
            let save_entry = entry.clone();
            let save_file = file.clone();
            menu.item(
                // Save happens in the draft's window: it opens there.
                PopupMenuItem::new("Save as…").on_click(move |_, _, cx| {
                    let _ = save_entry.update(cx, |entry, cx| {
                        entry.open_row(Row::Draft(save_file.clone()), cx)
                    });
                }),
            )
            .separator()
            .item(
                PopupMenuItem::new("Discard draft…").on_click(move |_, window, cx| {
                    let _ = entry.update(cx, |entry, cx| {
                        entry.confirm_discard(file.clone(), window, cx)
                    });
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui::*` brings its own `test`, which `#[gpui::test]` expands into.
    use ::core::prelude::v1::test;

    #[test]
    fn issue3208_compose_lists_three_drafts_and_ten_rows() {
        assert_eq!(compose(0, 4, false), (0, 4, 0));
        assert_eq!(compose(0, 12, false), (0, 10, 2));
        assert_eq!(compose(5, 12, false), (3, 7, 7));
        assert_eq!(compose(1, 3, false), (1, 3, 0));
        assert_eq!(compose(5, 12, true), (5, 12, 0));
    }

    #[test]
    fn issue3208_filter_matches_name_then_path_then_letters() {
        assert_eq!(hit("work", "Work Notes", "~/x"), Some(Hit::Name));
        assert_eq!(hit("WORK", "My work", "~/x"), Some(Hit::Name));
        assert_eq!(hit("work", "Brain", "~/work/brain"), Some(Hit::Path));
        assert_eq!(hit("wnt", "Work Notes", "~/x"), Some(Hit::Loose));
        assert_eq!(hit("zzz", "Work Notes", "~/x"), None);
        assert_eq!(hit("  ", "Work Notes", "~/x"), None);
        // Non-ASCII: byte ranges stay on character boundaries.
        assert_eq!(find_ci("Заметки Работа", "работа"), Some(15..27));
        assert_eq!(find_ci("Ünïcode", "ÜNÏ"), Some(0..5));
    }

    #[test]
    fn issue3208_paths_shorten_by_whole_segments_from_the_anchor() {
        let path = ShownPath {
            anchor: "/Volumes/Maestro/".into(),
            segments: ["src", "_DropBox", "okilum", "theme-showcase"]
                .map(String::from)
                .to_vec(),
            separator: '/',
        };
        assert_eq!(
            path.full(),
            "/Volumes/Maestro/src/_DropBox/okilum/theme-showcase"
        );
        let fits = |limit: usize| move |text: &str| text.chars().count() <= limit;
        assert_eq!(path.shortened(None, fits(100)), path.full());
        assert_eq!(
            path.shortened(None, fits(42)),
            "/Volumes/Maestro/…/okilum/theme-showcase"
        );
        assert_eq!(
            path.shortened(None, fits(35)),
            "/Volumes/Maestro/…/theme-showcase"
        );
        // Nothing fits: anchor + … + last, for the element to ellipsize.
        assert_eq!(
            path.shortened(None, fits(5)),
            "/Volumes/Maestro/…/theme-showcase"
        );
        // While filtering, the matching segment stays.
        let home = ShownPath {
            anchor: "~/".into(),
            segments: ["Developer", "work", "runs", "2026-11-10", "oncall-kt"]
                .map(String::from)
                .to_vec(),
            separator: '/',
        };
        let keep = home.segment_with("work");
        assert_eq!(keep, Some(1));
        assert_eq!(home.shortened(keep, fits(20)), "~/…/work/…/oncall-kt");
    }

    #[cfg(unix)]
    #[test]
    fn issue3208_path_anchors() {
        let home = Path::new("/Users/o");
        let shown = |path: &str| ShownPath::of(Path::new(path), Some(home)).full();
        assert_eq!(shown("/Users/o/vaults/brain"), "~/vaults/brain");
        assert_eq!(shown("/Users/o"), "~");
        assert_eq!(shown("/Volumes/Maestro/src/x"), "/Volumes/Maestro/src/x");
        assert_eq!(
            ShownPath::of(Path::new("/Volumes/Maestro/src/x"), Some(home)).anchor,
            "/Volumes/Maestro/"
        );
        assert_eq!(shown("/srv/notes"), "/srv/notes");
        assert_eq!(
            shown("/run/user/1000/gvfs/smb-share:server=nas,share=notes/Work"),
            "smb://nas/notes/Work"
        );
    }

    #[cfg(windows)]
    #[test]
    fn issue3208_windows_paths_are_plain_without_tilde() {
        let home = Path::new(r"C:\Users\o");
        let shown = |path: &str| ShownPath::of(Path::new(path), Some(home)).full();
        assert_eq!(shown(r"\\?\C:\Users\o\Notes"), r"C:\Users\o\Notes");
        assert_eq!(shown(r"\\nas\notes\Work"), r"\\nas\notes\Work");
    }

    #[test]
    fn issue3208_relative_times() {
        use chrono::TimeZone;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 11, 21, 15, 0, 0)
            .unwrap();
        let at = |y, m, d| {
            u64::try_from(
                chrono::Local
                    .with_ymd_and_hms(y, m, d, 9, 0, 0)
                    .unwrap()
                    .timestamp(),
            )
            .unwrap()
        };
        assert_eq!(relative_time(at(2026, 11, 21), now), "Today");
        assert_eq!(relative_time(at(2026, 11, 20), now), "Yesterday");
        assert_eq!(relative_time(at(2026, 11, 17), now), "Tuesday");
        assert_eq!(relative_time(at(2026, 11, 8), now), "Nov 8");
        assert_eq!(relative_time(at(2025, 3, 2), now), "2025-03-02");
    }
}
