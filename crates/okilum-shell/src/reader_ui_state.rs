//! Durable presentation state. Never stored in the vault or a derived cache.
use super::*;
use anyhow::Context as _;
use gpui_component::ThemeMode;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

const VERSION: u32 = 1;
const MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Position {
    pub item: usize,
    pub offset: f32,
}
impl From<ListOffset> for Position {
    fn from(value: ListOffset) -> Self {
        Self {
            item: value.item_ix,
            offset: value.offset_in_item.into(),
        }
    }
}
impl Position {
    pub fn list(&self) -> ListOffset {
        ListOffset {
            item_ix: self.item,
            offset_in_item: px(if self.offset.is_finite() {
                self.offset
            } else {
                0.
            }),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Visit {
    pub note: String,
    pub position: Position,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Layout {
    pub panels: reader_layout::Panels,
    pub widths: reader_layout::Widths,
    pub collapsed: BTreeSet<reader_sidebar::Section>,
    pub properties_collapsed: bool,
    pub show_hidden: bool,
    pub folders: BTreeSet<String>,
    pub tree_cursor: Option<String>,
    pub tree_scroll: f32,
    pub recent_expanded: bool,
    pub properties_open: bool,
    pub hidden_properties: bool,
    pub note: String,
    pub history: Vec<Visit>,
    pub history_index: usize,
    pub position: Position,
    pub source: bool,
    pub live_preview: bool,
    pub source_scroll: [f32; 2],
}
impl Default for Layout {
    fn default() -> Self {
        Self {
            panels: reader_layout::Panels {
                notes: true,
                backlinks: true,
                active: reader_layout::Panel::Notes,
            },
            widths: Default::default(),
            collapsed: Default::default(),
            properties_collapsed: false,
            show_hidden: false,
            folders: Default::default(),
            tree_cursor: None,
            tree_scroll: 0.,
            recent_expanded: false,
            properties_open: false,
            hidden_properties: false,
            note: String::new(),
            history: vec![],
            history_index: 0,
            position: Default::default(),
            source: false,
            live_preview: false,
            source_scroll: [0.; 2],
        }
    }
}
impl Layout {
    /// An unseen vault inherits presentation, never another vault's paths.
    fn inherited(&self) -> Self {
        Self {
            folders: Default::default(),
            tree_cursor: None,
            tree_scroll: 0.,
            note: String::new(),
            history: vec![],
            history_index: 0,
            position: Default::default(),
            source_scroll: [0.; 2],
            ..self.clone()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    version: u32,
    appearance: String,
    theme: String,
    font_size: f32,
    text_zoom: f32,
    reading_width: f32,
    /// The note column spans the whole note pane instead of the reading
    /// width; one app-wide toggle, kept with the reading width.
    full_note_width: bool,
    find_case_sensitive: bool,
    typed_views: okilum_core::typed_view::Preferences,
    toolbar_labels: bool,
    /// File tree only: false shows Markdown stems, true shows note titles (#1834).
    tree_note_titles: bool,
    /// Soft wrap in code files (#998); off means horizontal scroll.
    code_soft_wrap: bool,
    vault_open: String,
    settings_section: String,
    /// Brackets and quotes close themselves as they are typed in a note
    /// (#1636); on by default.
    auto_pair_brackets: bool,
    /// `*`, `_` and `` ` `` close themselves too (#1636); off by default,
    /// as in Typora.
    auto_pair_markdown: bool,
    /// Pasting or dropping a file larger than this many MB asks before
    /// copying it into the vault (#1611); 0 never asks.
    paste_ask_above_mb: u32,
    /// JSON files show their raw text instead of a pretty-printed view (#1131).
    json_raw: bool,
    /// Where log source locations open (#1165): vscode, cursor, zed,
    /// jetbrains or custom, and the custom command line.
    log_editor: String,
    log_editor_command: String,
    /// Per-vault path mappings for log source links (#1165), keyed like
    /// `vaults` by the canonical root.
    source_maps: BTreeMap<PathBuf, Vec<okilum_core::log::source_link::PathMap>>,
    /// Pane sizes of list/detail splits (#1156), by split key.
    split_sizes: BTreeMap<String, f32>,
    /// Recent log queries per log file, newest first (#1158).
    log_queries: BTreeMap<String, Vec<String>>,
    /// Field columns per log file (#1167).
    log_columns: BTreeMap<String, Vec<String>>,
    /// Widths the user dragged, per log file and column (#1167).
    log_column_widths: BTreeMap<String, BTreeMap<String, f32>>,
    /// Saved log filters, for every log (#1169).
    log_saved: okilum_core::log::saved_query::SavedQueries,
    #[serde(default)]
    log_regex: BTreeMap<String, okilum_core::log::regex_text::Definition>,
    delimited_no_header: BTreeSet<PathBuf>,
    vaults: BTreeMap<PathBuf, Layout>,
    /// Per-vault accent preset key (#774), keyed like `vaults` by the
    /// canonical root. Unknown keys from a newer build are kept, not shown.
    vault_colors: BTreeMap<PathBuf, String>,
    /// Vaults whose colour does not tint the title bar (#1050); tinting is
    /// on by default whenever a colour is set.
    untinted_vaults: BTreeSet<PathBuf>,
    /// Vaults the user agreed to open from an external `okilum:` link
    /// (#1049): only the first link to a vault asks.
    link_trusted_vaults: BTreeSet<PathBuf>,
    last_layout: Option<Layout>,
    frames: BTreeMap<String, window_state::Frame>,
    last_frame: Option<window_state::Frame>,
}
impl Default for Saved {
    fn default() -> Self {
        Self {
            version: VERSION,
            appearance: "system".into(),
            theme: brand::ThemeId::default().key().into(),
            font_size: BODY_FONT_SIZE,
            text_zoom: 1.,
            reading_width: READER_MAX_WIDTH,
            full_note_width: false,
            find_case_sensitive: false,
            typed_views: Default::default(),
            toolbar_labels: false,
            tree_note_titles: false,
            code_soft_wrap: false,
            vault_open: String::new(),
            settings_section: String::new(),
            auto_pair_brackets: true,
            auto_pair_markdown: false,
            paste_ask_above_mb: crate::note_paste::files::DEFAULT_ASK_ABOVE_MB,
            json_raw: false,
            log_editor: "vscode".into(),
            log_editor_command: String::new(),
            source_maps: Default::default(),
            split_sizes: Default::default(),
            log_queries: Default::default(),
            log_columns: Default::default(),
            log_column_widths: Default::default(),
            log_saved: Default::default(),
            log_regex: Default::default(),
            delimited_no_header: Default::default(),
            vaults: Default::default(),
            vault_colors: Default::default(),
            untinted_vaults: Default::default(),
            link_trusted_vaults: Default::default(),
            last_layout: None,
            frames: Default::default(),
            last_frame: None,
        }
    }
}
struct Store {
    path: PathBuf,
    saved: Saved,
    changed: BTreeSet<PathBuf>,
    appearance_changed: bool,
    theme_changed: bool,
    font_changed: bool,
    zoom_changed: bool,
    width_changed: bool,
    find_changed: bool,
    typed_views_changed: bool,
    toolbar_labels_changed: bool,
    tree_note_titles_changed: bool,
    code_soft_wrap_changed: bool,
    vault_open_changed: bool,
    settings_section_changed: bool,
    auto_pair_changed: bool,
    paste_ask_changed: bool,
    json_raw_changed: bool,
    log_editor_changed: bool,
    source_maps_changed: BTreeSet<PathBuf>,
    splits_changed: BTreeSet<String>,
    log_queries_changed: BTreeSet<String>,
    log_columns_changed: BTreeSet<String>,
    log_saved_changed: BTreeSet<String>,
    log_regex_changed: BTreeSet<String>,
    colors_changed: BTreeSet<PathBuf>,
    trust_changed: BTreeSet<PathBuf>,
    delimited_changed: BTreeSet<PathBuf>,
    frames_changed: BTreeSet<String>,
    last_changed: bool,
    last_frame_changed: bool,
    readers: Vec<WeakEntity<Reader>>,
    /// The vault each Reader's window shows while that window is the active
    /// one. The sibling check reads this, not the other Readers: reading an
    /// entity while another window draws binds it to that window in GPUI,
    /// and its window callbacks then run there (#1338).
    active_roots: std::collections::HashMap<gpui::EntityId, PathBuf>,
    blocked: bool,
    serial: Arc<AtomicU64>,
    pending: Option<Task<()>>,
}

pub(crate) fn state_directory(cx: &App) -> anyhow::Result<PathBuf> {
    cx.try_global::<Store>()
        .and_then(|state| state.path.parent())
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("Application state is unavailable; sources were not saved"))
}
impl Global for Store {}

fn read(path: &Path) -> anyhow::Result<Saved> {
    if !path.exists() {
        return Ok(Saved::default());
    }
    anyhow::ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "UI state is not a regular file"
    );
    anyhow::ensure!(
        std::fs::metadata(path)?.len() <= MAX_BYTES,
        "UI state exceeds size limit"
    );
    let saved: Saved = serde_json::from_slice(&std::fs::read(path)?)?;
    anyhow::ensure!(saved.version == VERSION, "Unsupported UI state version");
    Ok(saved)
}

pub(crate) fn install(directory: &Path, cx: &mut App) {
    let path = directory.join("reader-ui.json");
    let saved = match read(&path) {
        Ok(mut saved) => {
            if !path.exists() {
                // Legacy appearance is migration input only.
                if let Some(json) = super::appearance_settings_path()
                    .and_then(|path| std::fs::read(path).ok())
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                {
                    if let Some(mode) = json["appearance"].as_str() {
                        saved.appearance = mode.into();
                    }
                    saved.theme = json["theme"]
                        .as_str()
                        .and_then(brand::ThemeId::from_key)
                        .unwrap_or_default()
                        .key()
                        .into();
                }
            }
            saved
        }
        Err(error) => {
            eprintln!("Cannot restore UI state: {error:#}");
            // Preserve unreadable/future data; do not overwrite it with defaults.
            return;
        }
    };
    cx.set_global(Store {
        path: path.clone(),
        saved,
        changed: Default::default(),
        appearance_changed: !path.exists(),
        theme_changed: !path.exists(),
        font_changed: !path.exists(),
        zoom_changed: !path.exists(),
        width_changed: !path.exists(),
        find_changed: false,
        // No migration input: opening a new window must not overwrite a
        // different process's newly saved mappings with our startup defaults.
        typed_views_changed: false,
        toolbar_labels_changed: false,
        tree_note_titles_changed: false,
        code_soft_wrap_changed: false,
        vault_open_changed: false,
        settings_section_changed: false,
        auto_pair_changed: false,
        paste_ask_changed: false,
        json_raw_changed: false,
        log_editor_changed: false,
        source_maps_changed: Default::default(),
        splits_changed: Default::default(),
        log_queries_changed: Default::default(),
        log_columns_changed: Default::default(),
        log_saved_changed: Default::default(),
        log_regex_changed: Default::default(),
        colors_changed: Default::default(),
        trust_changed: Default::default(),
        delimited_changed: Default::default(),
        frames_changed: Default::default(),
        last_changed: false,
        last_frame_changed: false,
        readers: vec![],
        active_roots: Default::default(),
        blocked: false,
        serial: Arc::default(),
        pending: None,
    });
    cx.on_app_quit(|cx| {
        let readers = cx.global::<Store>().readers.clone();
        for reader in readers {
            let _ = reader.update(cx, |reader, cx| {
                reader.record_ui_state(reader.ui_state.active, cx)
            });
        }
        flush(cx);
        async {}
    })
    .detach();
}

pub(crate) fn layout(root: &Path, cx: &App) -> Option<(Layout, bool)> {
    let state = cx.try_global::<Store>()?;
    if let Some(layout) = state.saved.vaults.get(root) {
        return Some((layout.clone(), true));
    }
    state
        .saved
        .last_layout
        .as_ref()
        .map(|layout| (layout.inherited(), false))
}

/// Snapshot on the UI thread so a new window sees even a pending debounced
/// selection. File existence is checked by the opening worker.
pub(crate) fn notes(cx: &App) -> BTreeMap<PathBuf, String> {
    cx.try_global::<Store>()
        .map(|store| {
            store
                .saved
                .vaults
                .iter()
                .filter(|(_, layout)| valid_note(&layout.note))
                .map(|(root, layout)| (root.clone(), layout.note.clone()))
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn appearance(cx: &App) -> Option<AppearancePreference> {
    let state = cx.try_global::<Store>()?;
    Some(AppearancePreference(
        match state.saved.appearance.as_str() {
            "light" => Some(ThemeMode::Light),
            "dark" => Some(ThemeMode::Dark),
            _ => None,
        },
    ))
}
pub(crate) fn theme(cx: &App) -> Option<brand::ThemeId> {
    let state = cx.try_global::<Store>()?;
    Some(brand::ThemeId::from_key(&state.saved.theme).unwrap_or_default())
}
pub(crate) fn set_theme(theme: brand::ThemeId, cx: &mut App) -> bool {
    if cx.try_global::<Store>().is_none() {
        return false;
    }
    let state = cx.global_mut::<Store>();
    state.saved.theme = theme.key().into();
    state.theme_changed = true;
    schedule(cx);
    true
}

pub(crate) fn set_appearance(mode: Option<ThemeMode>, cx: &mut App) -> bool {
    if cx.try_global::<Store>().is_none() {
        return false;
    }
    let state = cx.global_mut::<Store>();
    state.saved.appearance = match mode {
        Some(ThemeMode::Light) => "light",
        Some(ThemeMode::Dark) => "dark",
        None => "system",
    }
    .into();
    state.appearance_changed = true;
    schedule(cx);
    true
}

/// The vault's accent colour (#774); `None` means no line and no dot.
pub(crate) fn vault_color(root: &Path, cx: &App) -> Option<brand::VaultColor> {
    cx.try_global::<Store>()?
        .saved
        .vault_colors
        .get(root)
        .and_then(|key| brand::VaultColor::from_key(key))
}

/// The title bar tint colour for this vault, if a colour is set and the
/// vault did not turn tinting off (#1050).
pub(crate) fn title_tint(root: &Path, cx: &App) -> Option<brand::VaultColor> {
    let state = cx.try_global::<Store>()?;
    if state.saved.untinted_vaults.contains(root) {
        return None;
    }
    vault_color(root, cx)
}

pub(crate) fn set_title_tint(root: &Path, enabled: bool, cx: &mut App) {
    let Some(state) = cx.try_global::<Store>() else {
        return;
    };
    if state.saved.untinted_vaults.contains(root) != enabled {
        return;
    }
    let state = cx.global_mut::<Store>();
    if enabled {
        state.saved.untinted_vaults.remove(root);
    } else {
        state.saved.untinted_vaults.insert(root.to_owned());
    }
    state.colors_changed.insert(root.to_owned());
    schedule(cx);
    cx.refresh_windows();
}

/// The user agreed to open this vault from an external link (#1049).
pub(crate) fn link_trusted(root: &Path, cx: &App) -> bool {
    cx.try_global::<Store>()
        .is_some_and(|state| state.saved.link_trusted_vaults.contains(root))
}

/// Remember that links may open this vault without asking again.
pub(crate) fn trust_for_links(root: &Path, cx: &mut App) {
    let Some(state) = cx.try_global::<Store>() else {
        return;
    };
    if state.saved.link_trusted_vaults.contains(root) {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.link_trusted_vaults.insert(root.to_owned());
    state.trust_changed.insert(root.to_owned());
    schedule(cx);
}

/// Every window of this vault repaints with the new colour at once.
pub(crate) fn set_vault_color(root: &Path, color: Option<brand::VaultColor>, cx: &mut App) {
    // Compare stored keys: «None» must also clear a preset this build
    // does not know (written by a newer one), which reads back as `None`.
    let Some(state) = cx.try_global::<Store>() else {
        return;
    };
    if state.saved.vault_colors.get(root).map(String::as_str) == color.map(brand::VaultColor::key) {
        return;
    }
    let state = cx.global_mut::<Store>();
    match color {
        Some(color) => {
            state
                .saved
                .vault_colors
                .insert(root.to_owned(), color.key().into());
        }
        None => {
            state.saved.vault_colors.remove(root);
            // A new colour later starts tinted again (#1050).
            state.saved.untinted_vaults.remove(root);
        }
    }
    state.colors_changed.insert(root.to_owned());
    schedule(cx);
    cx.refresh_windows();
}

pub(crate) fn record(root: &Path, layout: Layout, active: bool, cx: &mut App) {
    if cx.try_global::<Store>().is_none() {
        return;
    }
    let state = cx.global_mut::<Store>();
    if state.saved.vaults.get(root) == Some(&layout)
        && (!active || state.saved.last_layout.as_ref() == Some(&layout))
    {
        return;
    }
    state.changed.insert(root.to_owned());
    state.saved.vaults.insert(root.to_owned(), layout.clone());
    if active {
        state.last_changed = true;
        state.saved.last_layout = Some(layout);
    }
    schedule(cx);
}

#[derive(Clone)]
struct WriteJob {
    frames_changed: BTreeSet<String>,
    last_changed: bool,
    last_frame_changed: bool,
    path: PathBuf,
    saved: Saved,
    changed: BTreeSet<PathBuf>,
    appearance_changed: bool,
    theme_changed: bool,
    font_changed: bool,
    zoom_changed: bool,
    width_changed: bool,
    find_changed: bool,
    typed_views_changed: bool,
    toolbar_labels_changed: bool,
    tree_note_titles_changed: bool,
    code_soft_wrap_changed: bool,
    vault_open_changed: bool,
    settings_section_changed: bool,
    auto_pair_changed: bool,
    paste_ask_changed: bool,
    json_raw_changed: bool,
    log_editor_changed: bool,
    source_maps_changed: BTreeSet<PathBuf>,
    splits_changed: BTreeSet<String>,
    log_queries_changed: BTreeSet<String>,
    log_columns_changed: BTreeSet<String>,
    log_saved_changed: BTreeSet<String>,
    log_regex_changed: BTreeSet<String>,
    colors_changed: BTreeSet<PathBuf>,
    trust_changed: BTreeSet<PathBuf>,
    delimited_changed: BTreeSet<PathBuf>,
    serial: Arc<AtomicU64>,
    generation: u64,
}
impl WriteJob {
    fn run(&self) -> anyhow::Result<()> {
        let directory = self.path.parent().context("Missing UI state directory")?;
        // Match the existing outside-vault guard, including symlink ancestors.
        for root in self
            .saved
            .vaults
            .keys()
            .chain(self.saved.vault_colors.keys())
            .chain(self.saved.untinted_vaults.iter())
            .chain(self.saved.link_trusted_vaults.iter())
            .chain(self.saved.source_maps.keys())
        {
            anyhow::ensure!(
                outside_vault(root, &self.path),
                "Refusing UI state inside a vault"
            );
        }
        std::fs::create_dir_all(directory)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join("reader-ui.lock"))?;
        lock.lock()?;
        if self.serial.load(Ordering::SeqCst) != self.generation {
            return Ok(());
        }
        // Merge only this process's changed vaults; another process may have
        // recorded a different vault since this window was opened.
        let mut latest = read(&self.path)?;
        for root in &self.changed {
            if let Some(layout) = self.saved.vaults.get(root) {
                latest.vaults.insert(root.clone(), layout.clone());
            }
        }
        if self.appearance_changed {
            latest.appearance = self.saved.appearance.clone();
        }
        if self.theme_changed {
            latest.theme = self.saved.theme.clone();
        }
        if self.zoom_changed {
            latest.text_zoom = self.saved.text_zoom;
        }
        if self.font_changed {
            latest.font_size = self.saved.font_size;
        }
        if self.typed_views_changed {
            latest.typed_views = self.saved.typed_views.clone();
        }
        if self.width_changed {
            latest.reading_width = self.saved.reading_width;
            latest.full_note_width = self.saved.full_note_width;
        }
        if self.find_changed {
            latest.find_case_sensitive = self.saved.find_case_sensitive;
        }
        for path in &self.delimited_changed {
            if self.saved.delimited_no_header.contains(path) {
                latest.delimited_no_header.insert(path.clone());
            } else {
                latest.delimited_no_header.remove(path);
            }
        }
        if self.toolbar_labels_changed {
            latest.toolbar_labels = self.saved.toolbar_labels;
        }
        if self.tree_note_titles_changed {
            latest.tree_note_titles = self.saved.tree_note_titles;
        }
        if self.vault_open_changed {
            latest.vault_open = self.saved.vault_open.clone();
        }
        if self.settings_section_changed {
            latest.settings_section = self.saved.settings_section.clone();
        }
        if self.code_soft_wrap_changed {
            latest.code_soft_wrap = self.saved.code_soft_wrap;
        }
        if self.auto_pair_changed {
            latest.auto_pair_brackets = self.saved.auto_pair_brackets;
            latest.auto_pair_markdown = self.saved.auto_pair_markdown;
        }
        if self.paste_ask_changed {
            latest.paste_ask_above_mb = self.saved.paste_ask_above_mb;
        }
        if self.json_raw_changed {
            latest.json_raw = self.saved.json_raw;
        }
        for root in &self.source_maps_changed {
            match self.saved.source_maps.get(root) {
                Some(maps) => latest.source_maps.insert(root.clone(), maps.clone()),
                None => latest.source_maps.remove(root),
            };
        }
        if self.log_editor_changed {
            latest.log_editor = self.saved.log_editor.clone();
            latest.log_editor_command = self.saved.log_editor_command.clone();
        }
        for key in &self.splits_changed {
            if let Some(size) = self.saved.split_sizes.get(key) {
                latest.split_sizes.insert(key.clone(), *size);
            }
        }
        for key in &self.log_columns_changed {
            match self.saved.log_columns.get(key) {
                Some(columns) => latest.log_columns.insert(key.clone(), columns.clone()),
                None => latest.log_columns.remove(key),
            };
            match self.saved.log_column_widths.get(key) {
                Some(widths) => latest.log_column_widths.insert(key.clone(), widths.clone()),
                None => latest.log_column_widths.remove(key),
            };
        }
        // By name: a filter saved in another window stays, one deleted here
        // is deleted there.
        for name in &self.log_regex_changed {
            match self.saved.log_regex.get(name) {
                Some(definition) => {
                    latest.log_regex.insert(name.clone(), definition.clone());
                }
                None => {
                    latest.log_regex.remove(name);
                }
            }
        }
        for name in &self.log_saved_changed {
            match self.saved.log_saved.get(name) {
                Some(query) => {
                    latest.log_saved.upsert(query.clone());
                }
                None => {
                    latest.log_saved.remove(name);
                }
            }
        }
        for key in &self.log_queries_changed {
            match self.saved.log_queries.get(key) {
                Some(queries) => latest.log_queries.insert(key.clone(), queries.clone()),
                None => latest.log_queries.remove(key),
            };
        }
        // Per root, like layouts: a colour removed here is removed there.
        for root in &self.colors_changed {
            match self.saved.vault_colors.get(root) {
                Some(color) => {
                    latest.vault_colors.insert(root.clone(), color.clone());
                }
                None => {
                    latest.vault_colors.remove(root);
                }
            }
            if self.saved.untinted_vaults.contains(root) {
                latest.untinted_vaults.insert(root.clone());
            } else {
                latest.untinted_vaults.remove(root);
            }
        }
        for root in &self.trust_changed {
            if self.saved.link_trusted_vaults.contains(root) {
                latest.link_trusted_vaults.insert(root.clone());
            } else {
                latest.link_trusted_vaults.remove(root);
            }
        }
        if self.last_changed {
            latest.last_layout = self.saved.last_layout.clone();
        }
        for key in &self.frames_changed {
            if let Some(frame) = self.saved.frames.get(key) {
                latest.frames.insert(key.clone(), frame.clone());
            }
        }
        if self.last_frame_changed {
            latest.last_frame = self.saved.last_frame.clone();
        }
        let bytes = serde_json::to_vec(&latest)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "UI state exceeds size limit"
        );
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path)?;
        #[cfg(unix)]
        std::fs::File::open(directory)?.sync_all()?;
        Ok(())
    }
}
fn job(cx: &App) -> Option<WriteJob> {
    let state = cx.try_global::<Store>()?;
    if state.blocked
        || (state.changed.is_empty()
            && state.frames_changed.is_empty()
            && !state.appearance_changed
            && !state.theme_changed
            && !state.zoom_changed
            && !state.font_changed
            && !state.width_changed
            && !state.find_changed
            && !state.typed_views_changed
            && !state.toolbar_labels_changed
            && !state.tree_note_titles_changed
            && !state.vault_open_changed
            && !state.settings_section_changed
            && !state.code_soft_wrap_changed
            && !state.auto_pair_changed
            && !state.paste_ask_changed
            && !state.json_raw_changed
            && !state.log_editor_changed
            && state.source_maps_changed.is_empty()
            && state.splits_changed.is_empty()
            && state.log_queries_changed.is_empty()
            && state.log_columns_changed.is_empty()
            && state.log_regex_changed.is_empty()
            && state.log_saved_changed.is_empty()
            && state.colors_changed.is_empty()
            && state.trust_changed.is_empty()
            && state.delimited_changed.is_empty()
            && !state.last_changed
            && !state.last_frame_changed)
    {
        return None;
    }
    Some(WriteJob {
        frames_changed: state.frames_changed.clone(),
        last_changed: state.last_changed,
        last_frame_changed: state.last_frame_changed,
        path: state.path.clone(),
        saved: state.saved.clone(),
        changed: state.changed.clone(),
        appearance_changed: state.appearance_changed,
        theme_changed: state.theme_changed,
        font_changed: state.font_changed,
        zoom_changed: state.zoom_changed,
        width_changed: state.width_changed,
        find_changed: state.find_changed,
        typed_views_changed: state.typed_views_changed,
        toolbar_labels_changed: state.toolbar_labels_changed,
        tree_note_titles_changed: state.tree_note_titles_changed,
        code_soft_wrap_changed: state.code_soft_wrap_changed,
        vault_open_changed: state.vault_open_changed,
        settings_section_changed: state.settings_section_changed,
        auto_pair_changed: state.auto_pair_changed,
        paste_ask_changed: state.paste_ask_changed,
        json_raw_changed: state.json_raw_changed,
        log_editor_changed: state.log_editor_changed,
        source_maps_changed: state.source_maps_changed.clone(),
        splits_changed: state.splits_changed.clone(),
        log_queries_changed: state.log_queries_changed.clone(),
        log_columns_changed: state.log_columns_changed.clone(),
        log_saved_changed: state.log_saved_changed.clone(),
        log_regex_changed: state.log_regex_changed.clone(),
        colors_changed: state.colors_changed.clone(),
        trust_changed: state.trust_changed.clone(),
        delimited_changed: state.delimited_changed.clone(),
        serial: state.serial.clone(),
        generation: state.serial.load(Ordering::SeqCst),
    })
}
fn schedule(cx: &mut App) {
    let state = cx.global_mut::<Store>();
    state.serial.fetch_add(1, Ordering::SeqCst);
    state.pending.take();
    let task = cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(250))
            .await;
        let job = cx.update(|cx| job(cx));
        if let Some(job) = job {
            let generation = job.generation;
            let result = cx
                .background_executor()
                .spawn(async move { job.run() })
                .await;
            match result {
                Ok(()) => cx.update(|cx| mark_saved(generation, cx)),
                Err(error) => eprintln!("Cannot save UI state: {error:#}"),
            }
        }
    });
    cx.global_mut::<Store>().pending = Some(task);
}
#[cfg(any(windows, target_os = "linux", target_os = "macos", test))]
pub(crate) fn prepare_for_restart(cx: &mut App) -> anyhow::Result<()> {
    let Some(store) = cx.try_global::<Store>() else {
        return Ok(());
    };
    anyhow::ensure!(!store.blocked, "Reader state storage is unavailable");
    let readers = store.readers.clone();
    for reader in readers {
        let _ = reader.update(cx, |reader, cx| {
            reader.record_ui_state(reader.ui_state.active, cx)
        });
    }
    cx.global_mut::<Store>().pending.take();
    if let Some(job) = job(cx).filter(|job| job.generation > 0) {
        job.run()?;
        mark_saved(job.generation, cx);
    }
    Ok(())
}

pub(crate) fn flush(cx: &mut App) {
    if cx.try_global::<Store>().is_some() {
        cx.global_mut::<Store>().pending.take();
    }
    if let Some(job) = job(cx).filter(|job| job.generation > 0) {
        match job.run() {
            Ok(()) => mark_saved(job.generation, cx),
            Err(error) => eprintln!("Cannot flush UI state: {error:#}"),
        }
    }
}

#[derive(Default)]
pub(crate) struct Session {
    root: PathBuf,
    last: Option<Layout>,
    active: bool,
    tree: Option<Layout>,
    source: Option<[f32; 2]>,
    pub(super) live_preview: bool,
    /// This window keeps its chosen editing mode across Markdown file choices.
    /// Session restoration still uses the existing saved source/LP fields.
    pub(super) source_mode: bool,
    source_position_pending: bool,
    source_reader_position: Option<ListOffset>,
    pub(crate) source_highlight_pending: bool,
    reader_position_pending: bool,
    pub(crate) ready: bool,
    pub(crate) interacted: bool,
}

impl Reader {
    /// Called once for a newly published canonical root, not for reconciliation.
    pub(crate) fn restore_ui_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ui_state.root == self.vault_root || cx.try_global::<Store>().is_none() {
            return;
        }
        let weak = cx.entity().downgrade();
        let readers = &mut cx.global_mut::<Store>().readers;
        readers
            .retain(|reader| reader.upgrade().is_some() && reader.entity_id() != weak.entity_id());
        readers.push(weak);
        let interacted = self.ui_state.interacted;
        self.ui_state = Session {
            root: self.vault_root.clone(),
            ready: true,
            ..Default::default()
        };
        let Some((saved, known)) = layout(&self.vault_root, cx) else {
            return;
        };
        if !interacted {
            self.panels = saved.panels.clone();
            self.panel_widths
                .set(reader_layout::Panel::Notes, saved.widths.notes);
            self.panel_widths
                .set(reader_layout::Panel::Backlinks, saved.widths.backlinks);
        }
        // Reject legacy asynchronous width publication after this newer restore.
        self.panel_widths_revision = self.panel_widths_revision.wrapping_add(1);
        self.sidebar.collapsed = saved.collapsed.clone();
        self.sidebar.properties_collapsed = saved.properties_collapsed;
        self.sidebar.show_hidden = saved.show_hidden;
        self.recent_expanded = saved.recent_expanded;
        self.properties_open = saved.properties_open;
        self.show_hidden_properties = saved.hidden_properties;
        self.ui_state.tree = Some(saved.clone());
        self.ui_state.live_preview = saved.live_preview;
        if saved.source {
            self.ui_state.source_reader_position =
                (saved.note == self.current_rel).then(|| saved.position.list());
            self.ui_state.source = Some(if saved.note == self.current_rel {
                saved.source_scroll
            } else {
                [0.; 2]
            });
        }
        let explicit = self.loading.as_ref().is_some_and(|load| {
            load.opts.note.is_some()
                || load
                    .opts
                    .open_path
                    .as_ref()
                    .is_some_and(|path| path.extension().is_some())
        });
        if known && saved.note.is_empty() && !explicit {
            self.show_empty_vault(window, cx);
        }
        if known && saved.note == self.current_rel {
            let visits: Vec<_> = saved
                .history
                .iter()
                .filter(|visit| valid_note(&visit.note))
                .cloned()
                .collect();
            if visits.len() == saved.history.len()
                && visits
                    .get(saved.history_index)
                    .is_some_and(|visit| visit.note == self.current_rel)
            {
                self.navigation.history = visits.iter().map(|visit| visit.note.clone()).collect();
                self.navigation.history_positions =
                    visits.iter().map(|visit| visit.position.list()).collect();
                self.navigation.history_ix = saved.history_index;
            }
            if !saved.note.is_empty() && !saved.source {
                self.cancel_pending_landing();
                let positioned = self.content.update(cx, |content, cx| {
                    content.scroll_to_prepared_position(saved.position.list(), cx)
                });
                if !positioned {
                    self.ui_state.reader_position_pending = true;
                    self.scroll_to_position(saved.position.list(), cx);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn restore_ui_tree(&mut self) {
        // Keep expanded unknown folders until complete inventory is published.
        if !self.vault.inventory_scanned {
            return;
        }
        let Some(saved) = self.ui_state.tree.take() else {
            return;
        };
        self.tree.restore_expanded(saved.folders, saved.tree_cursor);
        self.tree_revealed = self.selected_file().to_owned();
        self.inbox_ready_root = Some(self.vault_root.clone());
        let offset = if saved.tree_scroll.is_finite() {
            saved.tree_scroll.min(0.)
        } else {
            0.
        };
        let mut scroll = self.tree_scroll.0.borrow_mut();
        // sync_tree may have queued a reveal before the saved layout arrived.
        // That request runs during layout and would overwrite this offset.
        scroll.deferred_scroll_to_item = None;
        scroll.base_handle.set_offset(point(px(0.), px(offset)));
    }

    pub(crate) fn restore_ui_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The selected document is usable before background search validation
        // finishes. The invisible Markdown preview need not finish laying out.
        if self
            .loading
            .as_ref()
            .is_some_and(|load| load.active && !load.published)
        {
            return;
        }
        let Some(offset) = self.ui_state.source.take() else {
            return;
        };
        if self.editing.is_none() && !self.current_rel.is_empty() && self.file_preview.is_none() {
            self.cancel_pending_landing();
            self.toggle_source(window, cx);
            if self.editing.is_none() {
                if let Some(position) = self.ui_state.source_reader_position.take() {
                    // If draft recovery cannot open, preserve the preview landing
                    // as well as the existing error notice.
                    self.scroll_to_position(position, cx);
                }
            }
            self.ui_state.source_position_pending = self.editing.is_some();
            if self.editing.is_some() {
                self.ui_state.source_highlight_pending = self.source_highlighting_pending(cx);
            }
            self.restore_source_position(offset, window, cx);
            self.land_restored_source(window, cx);
            let reader = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = reader.update(cx, |reader, cx| {
                    reader.ui_state.source_position_pending = false;
                    cx.notify();
                });
            });
        }
    }

    /// A window about to restore its editor opens it for a link instead: at
    /// the top, not at the last session's scroll (#1049). False when the
    /// window is not restoring an editor.
    pub(crate) fn retarget_restored_editor(&mut self) -> bool {
        if self.ui_state.source.is_none() {
            return false;
        }
        self.ui_state.source = Some([0.; 2]);
        self.ui_state.source_reader_position = None;
        true
    }

    pub(crate) fn restoring_source(&self) -> bool {
        self.ui_state.source.is_some()
            || self.ui_state.source_position_pending
            || (self.editing.is_some() && self.ui_state.source_highlight_pending)
    }

    pub(crate) fn restoring_reader(&self) -> bool {
        self.ui_state.reader_position_pending && self.navigation.pending_landing.is_some()
    }

    pub(crate) fn record_ui_state(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.navigation.pending_landing.is_none() {
            self.ui_state.reader_position_pending = false;
        }
        if !self.ui_state.ready || self.ui_state.root != self.vault_root {
            return;
        }
        // A background sibling must not replace the active window's viewport
        // when both show the same vault (including during quit).
        if !active && {
            let store = cx.global::<Store>();
            store.readers.iter().any(|other| {
                other.entity_id() != cx.entity_id()
                    && other.upgrade().is_some()
                    && store.active_roots.get(&other.entity_id()) == Some(&self.vault_root)
            })
        } {
            self.set_ui_active(false, cx);
            return;
        }
        if self.editing.is_none() && self.ui_state.source.is_none() {
            self.ui_state.source_reader_position = None;
        }
        let pending = self.ui_state.tree.as_ref();
        let position = self
            .ui_state
            .source_reader_position
            .or(self.navigation.pending_landing)
            .unwrap_or_else(|| self.content.read(cx).list_state().logical_scroll_top());
        let history = self
            .navigation
            .history
            .iter()
            .enumerate()
            .map(|(index, note)| Visit {
                note: note.clone(),
                position: if index == self.navigation.history_ix {
                    position.into()
                } else {
                    self.navigation
                        .history_positions
                        .get(index)
                        .copied()
                        .unwrap_or(ListOffset {
                            item_ix: 0,
                            offset_in_item: px(0.),
                        })
                        .into()
                },
            })
            .collect();
        let offset = self
            .source_scroll_offset(cx)
            .map(|p| [p.x.into(), p.y.into()])
            .or(self.ui_state.source)
            .unwrap_or([0.; 2]);
        let saved = Layout {
            panels: self.panels.clone(),
            widths: self.panel_widths.clone(),
            collapsed: self.sidebar.collapsed.clone(),
            properties_collapsed: self.sidebar.properties_collapsed,
            show_hidden: self.sidebar.show_hidden,
            folders: pending
                .map(|state| state.folders.clone())
                .unwrap_or_else(|| self.tree.expanded_paths().clone()),
            tree_cursor: pending
                .and_then(|state| state.tree_cursor.clone())
                .or_else(|| self.tree.cursor.clone()),
            tree_scroll: pending
                .map(|state| state.tree_scroll)
                .unwrap_or_else(|| self.tree_scroll.0.borrow().base_handle.offset().y.into()),
            recent_expanded: self.recent_expanded,
            properties_open: self.properties_open,
            hidden_properties: self.show_hidden_properties,
            note: self
                .file_preview
                .as_ref()
                .filter(|p| p.log.is_some() || reader_delimited::editable(&p.rel))
                .map_or_else(|| self.current_rel.clone(), |p| p.rel.clone()),
            history,
            history_index: self.navigation.history_ix,
            position: position.into(),
            source: self.editing.is_some() || self.ui_state.source.is_some(),
            live_preview: if self.editing.is_some() {
                self.source_live_preview()
            } else {
                self.ui_state.live_preview
            },
            source_scroll: offset,
        };
        if self.ui_state.last.as_ref() != Some(&saved) || (active && !self.ui_state.active) {
            self.ui_state.last = Some(saved.clone());
            record(&self.vault_root, saved, active, cx);
        }
        self.set_ui_active(active, cx);
    }

    fn set_ui_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.ui_state.active = active;
        let id = cx.entity_id();
        let root = self.vault_root.clone();
        let roots = &mut cx.global_mut::<Store>().active_roots;
        if active {
            roots.insert(id, root);
        } else {
            roots.remove(&id);
        }
    }
}
fn valid_note(note: &str) -> bool {
    note.is_empty()
        || Path::new(note)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

fn frame_key(key: &str) -> &str {
    if key.starts_with("reader:") {
        key.rsplit_once('#').map_or(key, |(root, _)| root)
    } else {
        key
    }
}
pub(crate) fn window_frame(key: &str, cx: &App) -> Option<window_state::Frame> {
    cx.try_global::<Store>()?
        .saved
        .frames
        .get(frame_key(key))
        .cloned()
}
pub(crate) fn inherited_window_frame(key: &str, cx: &App) -> Option<window_state::Frame> {
    key.starts_with("reader:")
        .then(|| cx.try_global::<Store>()?.saved.last_frame.clone())
        .flatten()
}
pub(crate) fn record_frame(
    key: &str,
    frame: window_state::Frame,
    active: bool,
    cx: &mut App,
) -> bool {
    if cx.try_global::<Store>().is_none() {
        return false;
    }
    let key = frame_key(key);
    let state = cx.global_mut::<Store>();
    if state.saved.frames.get(key) == Some(&frame)
        && (!active || state.saved.last_frame.as_ref() == Some(&frame))
    {
        return true;
    }
    state.frames_changed.insert(key.into());
    state.saved.frames.insert(key.into(), frame.clone());
    if active && key.starts_with("reader:") {
        state.last_frame_changed = true;
        state.saved.last_frame = Some(frame);
    }
    schedule(cx);
    true
}
pub(crate) fn installed(cx: &App) -> bool {
    cx.try_global::<Store>().is_some()
}

// Resolve existing ancestors as well as removed vaults, without allowing a
// symlinked state directory to bypass the outside-vault boundary.
fn resolved_destination(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return None;
    }
    let ancestor = path.ancestors().find(|part| part.exists())?;
    Some(
        ancestor
            .canonicalize()
            .ok()?
            .join(path.strip_prefix(ancestor).ok()?),
    )
}
fn outside_vault(root: &Path, path: &Path) -> bool {
    match (resolved_destination(root), resolved_destination(path)) {
        (Some(root), Some(path)) => !path.starts_with(root),
        _ => false,
    }
}

/// Called before creating a Reader window, so geometry cannot write into a
/// newly selected vault while its background preparation is still pending.
pub(crate) fn guard_window_root(key: &str, cx: &mut App) {
    let Some(root) = key.strip_prefix("reader:") else {
        return;
    };
    let Some(state) = cx.try_global::<Store>() else {
        return;
    };
    if !outside_vault(Path::new(root), &state.path) {
        cx.global_mut::<Store>().blocked = true;
        eprintln!("UI state is unavailable: its directory is inside the selected vault");
    }
}

impl Session {
    pub(crate) fn was_active(&self) -> bool {
        self.active
    }
}
pub(crate) fn capture_next_frame(reader: WeakEntity<Reader>, window: &Window) {
    window.on_next_frame(move |window, cx| {
        let _ = reader.update(cx, |reader, cx| {
            reader.record_ui_state(window.is_window_active(), cx)
        });
    });
}
pub(crate) fn capture_scroll(reader: WeakEntity<Reader>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let reader = reader.clone();
            window.on_mouse_event(move |_: &ScrollWheelEvent, phase, window, _| {
                if phase.capture() {
                    capture_next_frame(reader.clone(), window);
                }
            });
        },
    )
    .absolute()
    .size_full()
}

pub(crate) fn find_case_sensitive(cx: &App) -> bool {
    cx.try_global::<Store>()
        .is_some_and(|state| state.saved.find_case_sensitive)
}

pub(crate) fn set_find_case_sensitive(value: bool, cx: &mut App) {
    if !installed(cx) || find_case_sensitive(cx) == value {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.find_case_sensitive = value;
    state.find_changed = true;
    schedule(cx);
    cx.refresh_windows();
}

pub(crate) fn toolbar_labels(cx: &App) -> bool {
    cx.try_global::<Store>()
        .is_some_and(|state| state.saved.toolbar_labels)
}

pub(crate) fn set_toolbar_labels(value: bool, cx: &mut App) {
    if !installed(cx) || toolbar_labels(cx) == value {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.toolbar_labels = value;
    state.toolbar_labels_changed = true;
    schedule(cx);
    cx.refresh_windows();
}

pub(crate) fn tree_note_titles(cx: &App) -> bool {
    cx.try_global::<Store>()
        .is_some_and(|state| state.saved.tree_note_titles)
}

pub(crate) fn set_tree_note_titles(value: bool, cx: &mut App) {
    if !installed(cx) || tree_note_titles(cx) == value {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.tree_note_titles = value;
    state.tree_note_titles_changed = true;
    schedule(cx);
    cx.refresh_windows();
}

pub(crate) fn code_soft_wrap(cx: &App) -> bool {
    cx.try_global::<Store>()
        .is_some_and(|state| state.saved.code_soft_wrap)
}

pub(crate) fn set_code_soft_wrap(value: bool, cx: &mut App) {
    if !installed(cx) || code_soft_wrap(cx) == value {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.code_soft_wrap = value;
    state.code_soft_wrap_changed = true;
    schedule(cx);
    cx.refresh_windows();
}

/// Settings → Editor (#1636): whether brackets and quotes, and `*` `_` `` ` ``,
/// close themselves as they are typed in a note. Without the store (tests,
/// a failed install) the defaults hold.
pub(crate) fn auto_pair(cx: &App) -> (bool, bool) {
    cx.try_global::<Store>().map_or((true, false), |state| {
        (
            state.saved.auto_pair_brackets,
            state.saved.auto_pair_markdown,
        )
    })
}

pub(crate) fn set_auto_pair(brackets: bool, markdown: bool, cx: &mut App) {
    if !installed(cx) || auto_pair(cx) == (brackets, markdown) {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.auto_pair_brackets = brackets;
    state.saved.auto_pair_markdown = markdown;
    state.auto_pair_changed = true;
    schedule(cx);
    cx.refresh_windows();
}

/// Settings → Editor (#1611): ask before copying a pasted or dropped file
/// larger than this many MB into the vault; 0 never asks.
pub(crate) fn paste_ask_above_mb(cx: &App) -> u32 {
    cx.try_global::<Store>()
        .map_or(crate::note_paste::files::DEFAULT_ASK_ABOVE_MB, |state| {
            state.saved.paste_ask_above_mb
        })
}

pub(crate) fn set_paste_ask_above_mb(value: u32, cx: &mut App) {
    if !installed(cx) || paste_ask_above_mb(cx) == value {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.paste_ask_above_mb = value;
    state.paste_ask_changed = true;
    schedule(cx);
    cx.refresh_windows();
}

/// Pretty-printed JSON is the default (#1131); this remembers Raw.
pub(crate) fn json_raw(cx: &App) -> bool {
    cx.try_global::<Store>()
        .is_some_and(|state| state.saved.json_raw)
}

/// The remembered pane size of a list/detail split (#1156).
pub(crate) fn split_size(key: &str, cx: &App) -> Option<f32> {
    cx.try_global::<Store>()
        .and_then(|state| state.saved.split_sizes.get(key).copied())
        .filter(|size| size.is_finite() && *size > 0.)
}

pub(crate) fn set_split_size(key: &str, size: f32, cx: &mut App) {
    if !installed(cx) || !size.is_finite() || split_size(key, cx) == Some(size) {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.split_sizes.insert(key.into(), size);
    state.splits_changed.insert(key.into());
    schedule(cx);
}

/// Field columns of the log at `path` (#1167).
pub(crate) fn log_columns(path: &str, cx: &App) -> Vec<String> {
    cx.try_global::<Store>()
        .and_then(|state| state.saved.log_columns.get(path).cloned())
        .unwrap_or_default()
}

pub(crate) fn set_log_columns(path: &str, columns: Vec<String>, cx: &mut App) {
    if !installed(cx) || log_columns(path, cx) == columns {
        return;
    }
    let state = cx.global_mut::<Store>();
    if columns.is_empty() {
        state.saved.log_columns.remove(path);
    } else {
        state.saved.log_columns.insert(path.into(), columns);
    }
    state.log_columns_changed.insert(path.into());
    // Small and local, like the query history: past the cap, drop others.
    while state.saved.log_columns.len() > LOG_QUERY_FILES {
        let Some(other) = state
            .saved
            .log_columns
            .keys()
            .find(|key| *key != path)
            .cloned()
        else {
            break;
        };
        state.saved.log_columns.remove(&other);
        state.saved.log_column_widths.remove(&other);
        state.log_columns_changed.insert(other);
    }
    schedule(cx);
    cx.refresh_windows();
}

/// The width the user gave column `column` of the log at `path` (#1167).
pub(crate) fn log_column_width(path: &str, column: &str, cx: &App) -> Option<f32> {
    cx.try_global::<Store>()
        .and_then(|state| {
            state
                .saved
                .log_column_widths
                .get(path)?
                .get(column)
                .copied()
        })
        .filter(|width| width.is_finite() && *width > 0.)
}

pub(crate) fn set_log_column_width(path: &str, column: &str, width: f32, cx: &mut App) {
    if !installed(cx) || !width.is_finite() || log_column_width(path, column, cx) == Some(width) {
        return;
    }
    let state = cx.global_mut::<Store>();
    state
        .saved
        .log_column_widths
        .entry(path.into())
        .or_default()
        .insert(column.into(), width);
    state.log_columns_changed.insert(path.into());
    schedule(cx);
}

/// Queries kept per log file and log files with a history (#1158).
const LOG_QUERIES_PER_FILE: usize = 10;
const LOG_QUERY_FILES: usize = 64;

/// Recent queries for the log at `path`, newest first.
pub(crate) fn log_queries(path: &str, cx: &App) -> Vec<String> {
    cx.try_global::<Store>()
        .and_then(|state| state.saved.log_queries.get(path).cloned())
        .unwrap_or_default()
}

pub(crate) fn remember_log_query(path: &str, query: &str, cx: &mut App) {
    let query = query.trim();
    if !installed(cx)
        || query.is_empty()
        || log_queries(path, cx).first().map(String::as_str) == Some(query)
    {
        return;
    }
    let state = cx.global_mut::<Store>();
    let queries = state.saved.log_queries.entry(path.into()).or_default();
    queries.retain(|known| known != query);
    queries.insert(0, query.into());
    queries.truncate(LOG_QUERIES_PER_FILE);
    state.log_queries_changed.insert(path.into());
    // Small and local: past the cap, drop other files' histories.
    while state.saved.log_queries.len() > LOG_QUERY_FILES {
        let Some(other) = state
            .saved
            .log_queries
            .keys()
            .find(|key| *key != path)
            .cloned()
        else {
            break;
        };
        state.saved.log_queries.remove(&other);
        state.log_queries_changed.insert(other);
    }
    schedule(cx);
}

/// The saved log filters (#1169), empty before the state is installed.
/// Regex definitions share the durable reader settings file with saved filters.
pub(crate) fn log_regex_definitions(cx: &App) -> Vec<okilum_core::log::regex_text::Definition> {
    cx.try_global::<Store>()
        .map(|state| state.saved.log_regex.values().cloned().collect())
        .unwrap_or_default()
}

pub(crate) fn save_log_regex(
    definition: okilum_core::log::regex_text::Definition,
    cx: &mut App,
) -> bool {
    if !installed(cx) || definition.name.trim().is_empty() || definition.compile().is_err() {
        return false;
    }
    let state = cx.global_mut::<Store>();
    let name = definition.name.clone();
    state.saved.log_regex.insert(name.clone(), definition);
    state.log_regex_changed.insert(name);
    schedule(cx);
    true
}

pub(crate) fn delete_log_regex(name: &str, cx: &mut App) {
    if !installed(cx) {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.log_regex.remove(name);
    state.log_regex_changed.insert(name.into());
    schedule(cx);
}

pub(crate) fn saved_log_filters(cx: &App) -> okilum_core::log::saved_query::SavedQueries {
    cx.try_global::<Store>()
        .map(|state| state.saved.log_saved.clone())
        .unwrap_or_default()
}

/// Saves `query` under its name, replacing one of the same name. `false` when
/// nothing was saved: no state to keep it in, an unusable name, or a full list.
pub(crate) fn save_log_filter(
    query: okilum_core::log::saved_query::SavedQuery,
    cx: &mut App,
) -> bool {
    if !installed(cx) {
        return false;
    }
    let name = query.name.clone();
    let state = cx.global_mut::<Store>();
    if !state.saved.log_saved.upsert(query) {
        return false;
    }
    state.log_saved_changed.insert(name);
    schedule(cx);
    true
}

pub(crate) fn delete_log_filter(name: &str, cx: &mut App) {
    if !installed(cx) {
        return;
    }
    let state = cx.global_mut::<Store>();
    if state.saved.log_saved.remove(name).is_some() {
        state.log_saved_changed.insert(name.to_owned());
        schedule(cx);
    }
}

/// The editor log source locations open in (#1165).
pub(crate) fn log_editor(cx: &App) -> okilum_core::log::source_link::Editor {
    use okilum_core::log::source_link::Editor;
    let Some(state) = cx.try_global::<Store>() else {
        return Editor::default();
    };
    match state.saved.log_editor.as_str() {
        "cursor" => Editor::Cursor,
        "zed" => Editor::Zed,
        "jetbrains" => Editor::JetBrains,
        "custom" => Editor::Custom(state.saved.log_editor_command.clone()),
        _ => Editor::VsCode,
    }
}

pub(crate) fn set_log_editor(editor: &okilum_core::log::source_link::Editor, cx: &mut App) {
    use okilum_core::log::source_link::Editor;
    if !installed(cx) || log_editor(cx) == *editor {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.log_editor = match editor {
        Editor::VsCode => "vscode",
        Editor::Cursor => "cursor",
        Editor::Zed => "zed",
        Editor::JetBrains => "jetbrains",
        Editor::Custom(_) => "custom",
    }
    .into();
    if let Editor::Custom(command) = editor {
        state.saved.log_editor_command = command.clone();
    }
    state.log_editor_changed = true;
    schedule(cx);
}

/// Path mappings of the vault at `root` (#1165).
pub(crate) fn source_maps(root: &Path, cx: &App) -> Vec<okilum_core::log::source_link::PathMap> {
    cx.try_global::<Store>()
        .and_then(|state| state.saved.source_maps.get(root).cloned())
        .unwrap_or_default()
}

/// Path mappings for a log at `path`: those of the innermost vault that
/// holds it.
pub(crate) fn source_maps_for(
    path: &Path,
    cx: &App,
) -> Vec<okilum_core::log::source_link::PathMap> {
    cx.try_global::<Store>()
        .and_then(|state| {
            state
                .saved
                .source_maps
                .iter()
                .filter(|(root, _)| path.starts_with(root))
                .max_by_key(|(root, _)| root.components().count())
                .map(|(_, maps)| maps.clone())
        })
        .unwrap_or_default()
}

pub(crate) fn set_source_maps(
    root: &Path,
    maps: Vec<okilum_core::log::source_link::PathMap>,
    cx: &mut App,
) {
    if !installed(cx) || source_maps(root, cx) == maps {
        return;
    }
    let state = cx.global_mut::<Store>();
    if maps.is_empty() {
        state.saved.source_maps.remove(root);
    } else {
        state.saved.source_maps.insert(root.to_owned(), maps);
    }
    state.source_maps_changed.insert(root.to_owned());
    schedule(cx);
    cx.refresh_windows();
}

pub(crate) fn set_json_raw(value: bool, cx: &mut App) {
    if !installed(cx) || json_raw(cx) == value {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.json_raw = value;
    state.json_raw_changed = true;
    schedule(cx);
    cx.refresh_windows();
}

pub(crate) fn delimited_header(path: &Path, cx: &App) -> bool {
    !cx.try_global::<Store>()
        .is_some_and(|state| state.saved.delimited_no_header.contains(path))
}

pub(crate) fn set_delimited_header(path: PathBuf, header: bool, cx: &mut App) {
    if !installed(cx) || delimited_header(&path, cx) == header {
        return;
    }
    let state = cx.global_mut::<Store>();
    if header {
        state.saved.delimited_no_header.remove(&path);
    } else {
        state.saved.delimited_no_header.insert(path.clone());
    }
    state.delimited_changed.insert(path);
    schedule(cx);
    cx.refresh_windows();
}

pub(crate) fn font_size(cx: &App) -> f32 {
    base_font_size(cx) * text_zoom(cx)
}
pub(crate) fn base_font_size(cx: &App) -> f32 {
    cx.try_global::<Store>().map_or(BODY_FONT_SIZE, |state| {
        state.saved.font_size.clamp(12., 24.)
    })
}
pub(crate) fn text_zoom(cx: &App) -> f32 {
    cx.try_global::<Store>().map_or(1., |state| {
        let zoom = state.saved.text_zoom;
        if zoom.is_finite() {
            zoom.clamp(0.5, 3.)
        } else {
            1.
        }
    })
}
pub(crate) fn set_text_zoom(zoom: f32, cx: &mut App) {
    if !zoom.is_finite() || !installed(cx) {
        return;
    }
    let zoom = zoom.clamp(0.5, 3.);
    let state = cx.global_mut::<Store>();
    if state.saved.text_zoom == zoom {
        return;
    }
    state.saved.text_zoom = zoom;
    state.zoom_changed = true;
    schedule(cx);
    cx.refresh_windows();
}

pub(crate) fn reading_width(cx: &App) -> f32 {
    cx.try_global::<Store>().map_or(READER_MAX_WIDTH, |state| {
        state.saved.reading_width.clamp(560., 1200.)
    })
}
pub(crate) fn full_note_width(cx: &App) -> bool {
    cx.try_global::<Store>()
        .is_some_and(|state| state.saved.full_note_width)
}
/// Maximum width of the note column: the reading width scaled by the note
/// text zoom (#1652), or the whole pane when full note width is on (side
/// padding stays inside the column). Callers lay the column out with
/// `w_full().max_w(..)`, so a zoomed column never grows past its pane.
pub(crate) fn note_column_width(cx: &App) -> f32 {
    if full_note_width(cx) {
        100_000.
    } else {
        reading_width(cx) * text_zoom(cx)
    }
}
/// The note column for surfaces whose text does not follow the note zoom.
pub(crate) fn unzoomed_note_column_width(cx: &App) -> f32 {
    if full_note_width(cx) {
        100_000.
    } else {
        reading_width(cx)
    }
}
pub(crate) fn set_full_note_width(value: bool, cx: &mut App) {
    if !installed(cx) || full_note_width(cx) == value {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.full_note_width = value;
    state.width_changed = true;
    schedule(cx);
    cx.refresh_windows();
}
pub(crate) fn set_reading_width(width: f32, cx: &mut App) {
    set_reading(base_font_size(cx), width, cx);
}
pub(crate) fn set_reading(font: f32, width: f32, cx: &mut App) {
    if !font.is_finite() || !width.is_finite() || !installed(cx) {
        return;
    }
    let state = cx.global_mut::<Store>();
    let font = font.clamp(12., 24.);
    let width = width.clamp(560., 1200.);
    state.font_changed |= state.saved.font_size != font;
    state.width_changed |= state.saved.reading_width != width;
    state.saved.font_size = font;
    state.saved.reading_width = width;
    schedule(cx);
    cx.refresh_windows();
}

fn mark_saved(generation: u64, cx: &mut App) {
    if cx.try_global::<Store>().is_none() {
        return;
    }
    let state = cx.global_mut::<Store>();
    if state.serial.load(Ordering::SeqCst) == generation {
        state.changed.clear();
        state.frames_changed.clear();
        state.appearance_changed = false;
        state.theme_changed = false;
        state.font_changed = false;
        state.zoom_changed = false;
        state.width_changed = false;
        state.find_changed = false;
        state.typed_views_changed = false;
        state.toolbar_labels_changed = false;
        state.tree_note_titles_changed = false;
        state.code_soft_wrap_changed = false;
        state.vault_open_changed = false;
        state.settings_section_changed = false;
        state.auto_pair_changed = false;
        state.paste_ask_changed = false;
        state.json_raw_changed = false;
        state.log_editor_changed = false;
        state.source_maps_changed.clear();
        state.splits_changed.clear();
        state.log_queries_changed.clear();
        state.log_columns_changed.clear();
        state.log_saved_changed.clear();
        state.log_regex_changed.clear();
        state.colors_changed.clear();
        state.trust_changed.clear();
        state.delimited_changed.clear();
        state.last_changed = false;
        state.last_frame_changed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn delimited_header_preferences_survive_restart_and_merge_per_file(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().join("state");
        let first = fixture.path().join("one.csv");
        let second = fixture.path().join("two.tsv");
        cx.update(|cx| {
            install(&directory, cx);
            assert!(delimited_header(&first, cx) && delimited_header(&second, cx));
            set_delimited_header(first.clone(), false, cx);
            let mut independent = job(cx).unwrap();
            // A second process's stale snapshot changes only its own file.
            independent.serial = Arc::new(AtomicU64::new(independent.generation));
            independent.saved.delimited_no_header = BTreeSet::from([second.clone()]);
            independent.delimited_changed = BTreeSet::from([second.clone()]);
            flush(cx);
            independent.run().unwrap();
            install(&directory, cx);
            assert!(!delimited_header(&first, cx));
            assert!(!delimited_header(&second, cx));
            set_delimited_header(first.clone(), true, cx);
            flush(cx);
            install(&directory, cx);
            assert!(
                delimited_header(&first, cx),
                "positive control: restoring the header persists too"
            );
            assert!(
                !delimited_header(&second, cx),
                "an unrelated file's preference survives the write"
            );
        });
        assert!(
            !first.exists() && !second.exists(),
            "preferences never create or write the canonical files"
        );
    }
    use ::core::prelude::v1::test;
    use gpui_component::WindowExt;

    #[gpui::test]
    fn tree_label_preference_defaults_to_file_names_and_survives_relaunch(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        std::fs::create_dir(&state).unwrap();
        // Legacy Settings has no tree-label key: the new default is file names.
        std::fs::write(
            state.join("reader-ui.json"),
            format!("{{\"version\":{VERSION}}}"),
        )
        .unwrap();
        cx.update(|cx| {
            install(&state, cx);
            assert!(!tree_note_titles(cx));
            set_tree_note_titles(true, cx);
            flush(cx);
            install(&state, cx);
            assert!(tree_note_titles(cx), "title mode is remembered");
            set_tree_note_titles(false, cx);
            flush(cx);
            install(&state, cx);
            assert!(!tree_note_titles(cx), "switching back is remembered too");
        });
    }

    #[gpui::test]
    fn saved_log_regexes_persist_merge_and_clear_pending_changes(cx: &mut TestAppContext) {
        use okilum_core::log::regex_text::Definition;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reader-ui.json");
        let definition = |name: &str, pattern: &str| Definition {
            name: name.into(),
            pattern: pattern.into(),
        };
        cx.update(|cx| {
            install(directory.path(), cx);
            assert!(save_log_regex(definition("First", "(?P<msg>first)"), cx));
            assert!(!save_log_regex(definition("Bad", "("), cx));
            flush(cx);
            assert!(
                cx.global::<Store>().log_regex_changed.is_empty(),
                "successful writes clear pending changes"
            );
            let mut external = read(&path).unwrap();
            external
                .log_regex
                .insert("Remote".into(), definition("Remote", "(?P<msg>remote)"));
            std::fs::write(&path, serde_json::to_vec(&external).unwrap()).unwrap();
            assert!(save_log_regex(definition("Second", "(?P<msg>second)"), cx));
            delete_log_regex("First", cx);
            flush(cx);
            install(directory.path(), cx);
            let definitions = log_regex_definitions(cx);
            assert_eq!(
                definitions
                    .iter()
                    .map(|d| d.name.as_str())
                    .collect::<Vec<_>>(),
                ["Remote", "Second"]
            );
            assert_eq!(definitions[1].pattern, "(?P<msg>second)");
        });
    }

    /// #1169: saved log filters are global, survive a restart, and merge by
    /// name with another process: one saved there stays, one deleted here is
    /// deleted there, one replaced here is replaced there.
    #[gpui::test]
    fn saved_log_filters_persist_and_merge_by_name(cx: &mut TestAppContext) {
        use okilum_core::log::saved_query::SavedQuery;
        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().join("state");
        let path = directory.join("reader-ui.json");
        let query = |name: &str, text: &str| SavedQuery {
            name: name.into(),
            query: text.into(),
            ..Default::default()
        };
        let names = |cx: &App| -> Vec<String> {
            saved_log_filters(cx)
                .sorted()
                .iter()
                .map(|query| query.name.clone())
                .collect()
        };
        cx.update(|cx| {
            assert!(
                !save_log_filter(query("early", "level>=warn"), cx),
                "no state to keep it in yet"
            );
            install(&directory, cx);
            assert!(saved_log_filters(cx).is_empty());
            assert!(save_log_filter(query("Slow 5xx", "status>=500"), cx));
            assert!(save_log_filter(query("Errors", "level>=error"), cx));
            assert!(
                !save_log_filter(query("   ", "level>=error"), cx),
                "no name"
            );
            flush(cx);
            // Another process saves one filter.
            let mut external = read(&path).unwrap();
            assert!(external.log_saved.upsert(query("Remote", "exists(err)")));
            std::fs::write(&path, serde_json::to_vec(&external).unwrap()).unwrap();
            // This one deletes one (spelled differently) and replaces another.
            delete_log_filter("  errors ", cx);
            assert!(save_log_filter(query("slow  5xx", "status>=502"), cx));
            flush(cx);
            let merged = read(&path).unwrap();
            let kept: Vec<_> = merged
                .log_saved
                .sorted()
                .iter()
                .map(|query| (query.name.clone(), query.query.clone()))
                .collect();
            assert_eq!(
                kept,
                [
                    ("Remote".to_owned(), "exists(err)".to_owned()),
                    ("slow 5xx".to_owned(), "status>=502".to_owned()),
                ]
            );
            install(&directory, cx);
            assert_eq!(names(cx), ["Remote", "slow 5xx"], "after a restart");
        });
    }

    /// #774: colours are per canonical root, survive a restart, and merge
    /// per root so another process's colour for a different vault (and an
    /// unknown future preset) is neither lost nor overwritten.
    #[gpui::test]
    fn vault_colors_persist_per_root_and_merge_with_other_processes(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let other = fixture.path().join("other");
        let future = fixture.path().join("future");
        for dir in [&root, &other, &future] {
            std::fs::create_dir(dir).unwrap();
        }
        let directory = fixture.path().join("state");
        let path = directory.join("reader-ui.json");
        cx.update(|cx| {
            install(&directory, cx);
            assert_eq!(vault_color(&root, cx), None, "default is no colour");
            set_vault_color(&root, Some(brand::VaultColor::Teal), cx);
            set_vault_color(&other, Some(brand::VaultColor::Pink), cx);
            flush(cx);
            // Another process: removes `other`, leaves an unknown preset.
            let mut external = read(&path).unwrap();
            external.vault_colors.remove(&other);
            external
                .vault_colors
                .insert(future.clone(), "future-colour".into());
            std::fs::write(&path, serde_json::to_vec(&external).unwrap()).unwrap();
            set_vault_color(&root, Some(brand::VaultColor::Blue), cx);
            flush(cx);
            let merged = read(&path).unwrap();
            assert_eq!(
                merged.vault_colors.get(&root).map(String::as_str),
                Some("blue")
            );
            assert!(
                !merged.vault_colors.contains_key(&other),
                "unchanged roots come from the latest file"
            );
            assert_eq!(
                merged.vault_colors.get(&future).map(String::as_str),
                Some("future-colour")
            );
            install(&directory, cx);
            assert_eq!(vault_color(&root, cx), Some(brand::VaultColor::Blue));
            assert_eq!(
                vault_color(&future, cx),
                None,
                "unknown preset shows nothing"
            );
            // «None» still clears a preset this build does not know.
            set_vault_color(&future, None, cx);
            flush(cx);
            assert!(!read(&path).unwrap().vault_colors.contains_key(&future));
            // #1050: tinting is on by default and can be turned off per vault.
            assert_eq!(title_tint(&root, cx), Some(brand::VaultColor::Blue));
            set_title_tint(&root, false, cx);
            flush(cx);
            install(&directory, cx);
            assert_eq!(title_tint(&root, cx), None);
            assert_eq!(
                vault_color(&root, cx),
                Some(brand::VaultColor::Blue),
                "colour kept"
            );
            assert!(read(&path).unwrap().untinted_vaults.contains(&root));
            set_title_tint(&root, true, cx);
            flush(cx);
            assert!(!read(&path).unwrap().untinted_vaults.contains(&root));
            set_vault_color(&root, None, cx);
            flush(cx);
            assert!(!read(&path).unwrap().vault_colors.contains_key(&root));
            assert_eq!(title_tint(&root, cx), None, "no colour, no tint");
            // Clearing the colour also clears «don't tint»: a new colour
            // starts tinted (review finding).
            set_vault_color(&root, Some(brand::VaultColor::Blue), cx);
            set_title_tint(&root, false, cx);
            set_vault_color(&root, None, cx);
            set_vault_color(&root, Some(brand::VaultColor::Pink), cx);
            assert_eq!(title_tint(&root, cx), Some(brand::VaultColor::Pink));
            flush(cx);
            assert!(!read(&path).unwrap().untinted_vaults.contains(&root));
            // #1049: a vault a link opened once is remembered; nothing is
            // trusted before the user agrees.
            assert!(
                !link_trusted(&root, cx),
                "positive control: not trusted yet"
            );
            trust_for_links(&root, cx);
            flush(cx);
            install(&directory, cx);
            assert!(link_trusted(&root, cx));
            assert!(read(&path).unwrap().link_trusted_vaults.contains(&root));
            // Positive control: a colour inside a vault is refused, like layouts.
            let inside = root.join("state");
            install(&inside, cx);
            set_vault_color(&root, Some(brand::VaultColor::Red), cx);
            flush(cx);
            assert!(!inside.join("reader-ui.json").exists());
        });
    }

    #[gpui::test]
    fn global_preferences_and_vault_layout_survive_flush_and_new_vault_inherits_no_paths(
        cx: &mut TestAppContext,
    ) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let other = fixture.path().join("other");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&other).unwrap();
        let directory = fixture.path().join("state");
        let mut saved = Layout {
            note: "Notes/one.md".into(),
            tree_cursor: Some("Notes".into()),
            tree_scroll: -240.,
            source: true,
            ..Default::default()
        };
        saved.panels.notes = false;
        saved.widths.notes = 370.;
        saved.collapsed.insert(reader_sidebar::Section::Inbox);
        saved.folders.insert("Notes".into());
        saved.history.push(Visit {
            note: saved.note.clone(),
            position: Position {
                item: 5,
                offset: 14.,
            },
        });
        cx.update(|cx| {
            install(&directory, cx);
            set_appearance(Some(ThemeMode::Dark), cx);
            set_theme(brand::ThemeId::Nord, cx);
            set_reading(19.5, 960., cx);
            assert!(!find_case_sensitive(cx));
            set_find_case_sensitive(true, cx);
            assert!(!toolbar_labels(cx), "toolbar labels are opt-in");
            set_toolbar_labels(true, cx);
            record(&root, saved.clone(), true, cx);
            assert!(
                !directory.join("reader-ui.json").exists(),
                "writes are debounced"
            );
            flush(cx);
            install(&directory, cx);
            assert!(matches!(appearance(cx).unwrap().0, Some(ThemeMode::Dark)));
            assert_eq!(theme(cx), Some(brand::ThemeId::Nord));
            assert!(
                toolbar_labels(cx),
                "toolbar labels survive relaunch globally"
            );
            assert_eq!(font_size(cx), 19.5);
            assert_eq!(reading_width(cx), 960.);
            assert!(
                find_case_sensitive(cx),
                "Match case survives relaunch outside the vault"
            );
            let (restored, known) = layout(&root, cx).unwrap();
            assert!(known);
            assert_eq!(restored, saved);
            let (inherited, known) = layout(&other, cx).unwrap();
            assert!(!known);
            assert_eq!(inherited.panels, saved.panels);
            assert_eq!(inherited.widths, saved.widths);
            assert_eq!(inherited.collapsed, saved.collapsed);
            assert!(inherited.note.is_empty() && inherited.history.is_empty());
            assert!(inherited.folders.is_empty() && inherited.tree_cursor.is_none());
            assert_eq!(inherited.tree_scroll, 0.);
        });
        assert!(!root.join("reader-ui.json").exists());
    }

    #[gpui::test]
    fn vault_destination_defaults_reload_and_independent_merges(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            install(temp.path(), cx);
            assert_eq!(vault_open(cx), "ask");
            for value in ["current", "new", "ask"] {
                set_vault_open(value, cx);
                flush(cx);
                install(temp.path(), cx);
                assert_eq!(vault_open(cx), value);
            }
            set_vault_open("current", cx);
            let pending = job(cx).unwrap();
            let mut independent = read(&pending.path).unwrap();
            independent.toolbar_labels = true;
            std::fs::write(&pending.path, serde_json::to_vec(&independent).unwrap()).unwrap();
            pending.run().unwrap();
            let saved = read(&pending.path).unwrap();
            assert!(
                saved.toolbar_labels,
                "preference write preserves unrelated changes"
            );
            assert_eq!(saved.vault_open, "current");
            cx.global_mut::<Store>().saved.vault_open = "future-value".into();
            assert_eq!(vault_open(cx), "ask");
        });
    }

    #[gpui::test]
    fn stale_writer_cannot_undo_a_newer_snapshot_and_other_vaults_are_merged(
        cx: &mut TestAppContext,
    ) {
        let fixture = tempfile::tempdir().unwrap();
        let first = fixture.path().join("one");
        let second = fixture.path().join("two");
        for root in [&first, &second] {
            std::fs::create_dir(root).unwrap();
        }
        let directory = fixture.path().join("state");
        cx.update(|cx| {
            install(&directory, cx);
            set_toolbar_labels(true, cx);
            set_tree_note_titles(true, cx);
            record(&first, Layout::default(), true, cx);
            let stale = job(cx).unwrap();
            let closed = Layout {
                panels: Default::default(),
                ..Default::default()
            };
            record(&first, closed.clone(), true, cx);
            let current = job(cx).unwrap();
            current.run().unwrap();
            mark_saved(current.generation, cx);
            stale.run().unwrap();
            assert_eq!(read(&current.path).unwrap().vaults[&first], closed);
            // Simulate an independent process that started before the first write.
            let independent = WriteJob {
                changed: BTreeSet::from([second.clone()]),
                saved: Saved {
                    vaults: BTreeMap::from([(second.clone(), Layout::default())]),
                    font_size: 22.,
                    typed_views: okilum_core::typed_view::Preferences {
                        mappings: BTreeMap::from([
                            ("Project".into(), "tasks".into()),
                            ("Future".into(), "optional-view".into()),
                        ]),
                        tasks: okilum_core::typed_view::layout::Defaults {
                            density: okilum_core::typed_view::layout::Density::Comfortable,
                            grouping: okilum_core::typed_view::layout::Grouping::Note,
                        },
                    },
                    ..Default::default()
                },
                serial: Arc::new(AtomicU64::new(1)),
                generation: 1,
                appearance_changed: false,
                theme_changed: false,
                font_changed: true,
                zoom_changed: false,
                width_changed: false,
                find_changed: false,
                typed_views_changed: true,
                toolbar_labels_changed: false,
                tree_note_titles_changed: false,
                code_soft_wrap_changed: false,
                vault_open_changed: false,
                settings_section_changed: false,
                auto_pair_changed: false,
                paste_ask_changed: false,
                json_raw_changed: false,
                log_editor_changed: false,
                source_maps_changed: Default::default(),
                splits_changed: Default::default(),
                log_queries_changed: Default::default(),
                log_columns_changed: Default::default(),
                log_saved_changed: Default::default(),
                log_regex_changed: Default::default(),
                colors_changed: Default::default(),
                trust_changed: Default::default(),
                delimited_changed: Default::default(),
                frames_changed: Default::default(),
                last_changed: false,
                last_frame_changed: false,
                path: current.path.clone(),
            };
            set_find_case_sensitive(true, cx);
            flush(cx);
            independent.run().unwrap();
            let merged = read(&current.path).unwrap();
            assert!(
                merged.toolbar_labels,
                "an unrelated process must not overwrite toolbar preferences"
            );
            assert_eq!(merged.vaults[&first], closed);
            assert!(merged.vaults.contains_key(&second));
            assert!(
                merged.find_case_sensitive,
                "view preferences preserve newer search settings"
            );
            set_find_case_sensitive(false, cx);
            set_appearance(Some(ThemeMode::Dark), cx);
            flush(cx);
            let merged = read(&current.path).unwrap();
            assert_eq!(merged.appearance, "dark");
            assert!(
                merged.tree_note_titles,
                "an unrelated writer preserves the tree-label preference"
            );
            assert!(
                !merged.find_case_sensitive,
                "positive control: the search setting was saved"
            );
            assert_eq!(
                merged.typed_views, independent.saved.typed_views,
                "unrelated writes preserve mappings, defaults and unknown view IDs"
            );
            assert_eq!(
                okilum_core::typed_view::select(
                    "---\ntype: Project\n---\n",
                    &merged.typed_views.mappings
                ),
                okilum_core::typed_view::Selection::Native(okilum_core::typed_view::TASKS)
            );
            assert_eq!(
                okilum_core::typed_view::select(
                    "---\ntype: project\n---\n",
                    &merged.typed_views.mappings
                ),
                okilum_core::typed_view::Selection::Markdown
            );
            assert!(matches!(
                okilum_core::typed_view::select(
                    "---\ntype: Future\n---\n",
                    &merged.typed_views.mappings
                ),
                okilum_core::typed_view::Selection::Fallback(_)
            ));

            assert_eq!(
                merged.font_size, 22.,
                "unrelated global edits merge per field"
            );
        });
    }

    /// Full note width is one app-wide toggle kept across relaunch; the note
    /// column follows it, and the normal-width slider never stores the
    /// zoomed font size as the base size.
    #[gpui::test]
    fn full_note_width_survives_relaunch_and_widens_the_note_column(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().join("state");
        cx.update(|cx| {
            install(&directory, cx);
            assert!(!full_note_width(cx), "normal width is the default");
            assert_eq!(note_column_width(cx), READER_MAX_WIDTH);
            set_text_zoom(1.5, cx);
            let base = base_font_size(cx);
            set_reading_width(900., cx);
            assert_eq!(base_font_size(cx), base, "slider keeps the unzoomed font");
            assert_eq!(unzoomed_note_column_width(cx), 900.);
            // The note column scales with the note text zoom (#1652).
            assert_eq!(note_column_width(cx), 900. * text_zoom(cx));
            set_reading_width(100., cx);
            assert_eq!(reading_width(cx), 560., "normal width is at least 560 px");
            set_reading_width(5000., cx);
            assert_eq!(reading_width(cx), 1200., "normal width is at most 1200 px");
            set_reading_width(900., cx);
            set_full_note_width(true, cx);
            assert!(full_note_width(cx));
            assert!(note_column_width(cx) > 10_000., "full width spans the pane");
            flush(cx);
            install(&directory, cx);
            assert!(full_note_width(cx), "full note width survives relaunch");
            assert_eq!(reading_width(cx), 900.);
            assert_eq!(base_font_size(cx), base);
            set_full_note_width(false, cx);
            assert_eq!(unzoomed_note_column_width(cx), 900.);
            flush(cx);
            install(&directory, cx);
            assert!(!full_note_width(cx), "turning it off is remembered too");
        });
    }

    #[gpui::test]
    fn removed_vault_does_not_block_other_preferences(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("removed");
        let directory = fixture.path().join("state");
        std::fs::create_dir(&root).unwrap();
        cx.update(|cx| {
            install(&directory, cx);
            record(&root, Layout::default(), true, cx);
            flush(cx);
            std::fs::remove_dir(&root).unwrap();
            set_reading(20., 960., cx);
            flush(cx);
            assert_eq!(
                read(&directory.join("reader-ui.json")).unwrap().font_size,
                20.
            );
            assert!(!outside_vault(&root, &root.join("reader-ui.json")));
        });
    }

    #[gpui::test]
    fn state_inside_vault_is_never_created_even_by_early_window_geometry(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let directory = root.join("state");
        cx.update(|cx| {
            install(&directory, cx);
            guard_window_root(&format!("reader:{}", root.display()), cx);
            record(&root, Layout::default(), true, cx);
            flush(cx);
        });
        assert!(!directory.exists());
    }

    #[gpui::test]
    fn restoring_closed_note_keeps_the_vault_window_open(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Note").unwrap();
        let directory = fixture.path().join("state");
        reader_history::ReadingHistory::record_usable_document(&directory, &root, "").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            install(&directory, cx);
            record(&root, Layout::default(), true, cx);
            flush(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root),
                        session_directory: Some(directory),
                        index_dir: Some(fixture.path().join("index")),
                        panel_settings_override: Some(fixture.path().join("legacy-widths.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        reader.unwrap().read_with(visual, |reader, cx| {
            assert!(
                reader.ui_state.ready,
                "positive control: vault state was restored"
            );
            assert!(reader.current_rel.is_empty());
            assert_eq!(
                cx.windows().len(),
                1,
                "empty selection is not a close-window action"
            );
        });
    }

    #[gpui::test]
    fn restored_reader_keeps_panels_sections_folders_history_and_scroll(cx: &mut TestAppContext) {
        restored_document_state(cx, false, false);
    }

    #[gpui::test]
    fn restored_source_hides_preview_until_source_viewport_is_ready(cx: &mut TestAppContext) {
        restored_document_state(cx, true, false);
    }

    #[gpui::test]
    fn restores_live_preview_and_recovered_draft(cx: &mut TestAppContext) {
        restored_document_state(cx, true, true);
    }

    fn restored_document_state(cx: &mut TestAppContext, source: bool, live_preview: bool) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir_all(root.join("Folder")).unwrap();
        std::fs::write(
            root.join("Folder/note.md"),
            (0..40)
                .map(|i| format!("## Heading {i}\n\nParagraph {i}.\n\n"))
                .collect::<String>(),
        )
        .unwrap();
        std::fs::write(root.join("other.md"), "# Other").unwrap();
        for i in 0..100 {
            std::fs::write(root.join(format!("tree-{i:03}.md")), "# Tree entry").unwrap();
        }
        // Legacy history disagrees with the newer UI snapshot (e.g. an early
        // quit while its independent background write was still pending).
        let directory = fixture.path().join("state");
        reader_history::ReadingHistory::record_usable_document(&directory, &root, "other.md")
            .unwrap();
        let saved = Layout {
            panels: reader_layout::Panels {
                notes: true,
                backlinks: false,
                active: reader_layout::Panel::Notes,
            },
            widths: reader_layout::Widths {
                notes: 355.,
                backlinks: 330.,
            },
            collapsed: BTreeSet::from([
                reader_sidebar::Section::Recent,
                reader_sidebar::Section::Pinned,
            ]),
            folders: BTreeSet::new(),
            tree_cursor: Some("Folder".into()),
            tree_scroll: -840.,
            source,
            live_preview,
            source_scroll: [0., -400.],
            note: "Folder/note.md".into(),
            position: Position {
                item: 14,
                offset: 0.,
            },
            history: vec![
                Visit {
                    note: "other.md".into(),
                    position: Position::default(),
                },
                Visit {
                    note: "Folder/note.md".into(),
                    position: Position {
                        item: 14,
                        offset: 0.,
                    },
                },
            ],
            history_index: 1,
            ..Default::default()
        };
        cx.update(|cx| {
            gpui_component::init(cx);
            install(&directory, cx);
            record(&root, saved.clone(), true, cx);
            flush(cx);
            install(&directory, cx);
        });
        if live_preview {
            let path = root.join("Folder/note.md");
            let mut draft =
                okilum_core::file_editor::FileEditor::open(&path, &directory.join("editor-drafts"))
                    .unwrap();
            let text = format!("{}\nUnsaved draft sentinel\n", draft.text());
            draft.set_text(text).unwrap();
            cx.update(|cx| cx.set_global(reader_recovery::RecoveryStartup(true)));
        }
        let (release_search, hold_search) = async_channel::bounded(1);
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: None,
                        preparation_hold: Some(hold_search),
                        session_directory: Some(directory.clone()),
                        index_dir: Some(fixture.path().join("index")),
                        panel_settings_override: Some(fixture.path().join("legacy-widths.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        if !source {
            reader.read_with(visual, |reader, cx| {
                assert!(
                    reader.ui_state.ready,
                    "positive control: publication completed"
                );
                assert_eq!(
                    reader
                        .content
                        .read(cx)
                        .list_state()
                        .logical_scroll_top()
                        .item_ix,
                    14,
                    "saved position must be installed before any landing timer"
                );
            });
        }
        visual.executor().advance_clock(Duration::from_millis(100));
        visual.run_until_parked();
        {
            // Search preparation is deliberately held indefinitely. The
            // published source document must still reach its saved viewport.
            for _ in 0..5 {
                visual.update(|window, cx| {
                    window.simulate_next_frame(cx);
                    window.draw(cx).clear(cx);
                });
                visual.executor().advance_clock(Duration::from_millis(20));
                visual.run_until_parked();
            }
            reader.read_with(visual, |reader, cx| {
                let load = reader.loading.as_ref().unwrap();
                assert!(load.active && load.published, "search remains unfinished");
                if source {
                    assert!(reader.editing.is_some(), "source must not wait for search");
                    assert!(
                        !reader.restoring_source(),
                        "source viewport must be visible"
                    );
                    assert_eq!(reader.source_live_preview(), live_preview);
                    if !live_preview {
                        assert_eq!(reader.source_scroll_offset(cx).unwrap().y, px(-400.));
                    }
                } else {
                    assert_eq!(
                        reader
                            .content
                            .read(cx)
                            .list_state()
                            .logical_scroll_top()
                            .item_ix,
                        14
                    );
                    assert!(
                        reader.navigation.pending_landing.is_none(),
                        "first reader frame is positioned without a timer"
                    );
                }
                assert_eq!(
                    layout(&root, cx).unwrap().0.position.item,
                    14,
                    "unpainted preview position must survive persisted source restoration"
                );
            });
            {
                assert_eq!(
                    visual
                        .debug_bounds("document-header-viewport")
                        .unwrap()
                        .size
                        .height,
                    px(48.),
                    "restoration must keep breadcrumbs visible at a mid-note position"
                );
            }
            release_search.try_send(()).unwrap();
            visual.run_until_parked();
        }
        reader.update(visual, |reader, _| {
            // Native startup can publish state between sync_tree's reveal and
            // the list's first layout. Exercise that ordering explicitly.
            assert!(reader.vault.inventory_scanned);
            reader.ui_state.tree = Some(saved.clone());
            reader.tree_scroll.scroll_to_item(0, ScrollStrategy::Center);
            reader.restore_ui_tree();
        });
        for _ in 0..5 {
            reader.read_with(visual, |reader, _| {
                if source && reader.ui_state.ready && reader.editing.is_none() {
                    assert!(reader.restoring_source(), "preview must remain hidden");
                }
            });
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.executor().advance_clock(Duration::from_millis(20));
            visual.run_until_parked();
        }
        reader.read_with(visual, |reader, cx| {
            assert!(
                reader.document_ready(),
                "positive control: actual document loaded"
            );
            assert_eq!(reader.panels, saved.panels);
            assert_eq!(
                reader.panel_widths, saved.widths,
                "legacy async defaults cannot overwrite saved widths"
            );
            assert_eq!(reader.sidebar.collapsed, saved.collapsed);
            assert_eq!(reader.tree.expanded_paths(), &saved.folders);
            assert_eq!(
                reader.tree_scroll.0.borrow().base_handle.offset().y,
                px(saved.tree_scroll),
                "saved tree scroll must survive the initial selected-note reveal"
            );
            if source {
                assert!(reader.editing.is_some(), "positive control: source mounted");
                assert!(
                    !reader.restoring_source(),
                    "restored source must become visible"
                );
                assert_eq!(reader.source_live_preview(), live_preview);
                if live_preview {
                    let input = reader.editing.as_ref().unwrap().test_input();
                    assert!(input.read(cx).value().contains("Unsaved draft sentinel"));
                    assert!(!std::fs::read_to_string(root.join("Folder/note.md"))
                        .unwrap()
                        .contains("Unsaved draft sentinel"));
                } else {
                    assert_eq!(reader.source_scroll_offset(cx).unwrap().y, px(-400.));
                }
            }
            assert_eq!(reader.navigation.history, ["other.md", "Folder/note.md"]);
            assert_eq!(reader.navigation.history_ix, 1);
            assert_eq!(
                if source {
                    reader.ui_state.source_reader_position.unwrap().item_ix
                } else {
                    reader
                        .content
                        .read(cx)
                        .list_state()
                        .logical_scroll_top()
                        .item_ix
                },
                14
            );
        });
        if live_preview {
            visual.update(|window, cx| {
                assert!(
                    !window.notifications(cx).is_empty(),
                    "positive control: restored draft announces itself"
                );
                // #930: the draft is already applied, so the offer to
                // restore it must not appear and push the notice away.
                assert_eq!(window.notifications(cx).len(), 1, "one recovery notice");
                window.draw(cx).clear(cx);
            });
            assert!(
                visual.debug_bounds("restore-unsaved-edits").is_none(),
                "no «Restore unsaved edits» for an already restored draft"
            );
            // #930: «Unsaved changes restored» stays readable for most of
            // its 4 s lifetime; nothing pushes it away early.
            visual.executor().advance_clock(Duration::from_secs(3));
            visual.run_until_parked();
            visual.update(|window, cx| {
                assert_eq!(
                    window.notifications(cx).len(),
                    1,
                    "the restored notice is still shown after 3 s"
                );
                window.draw(cx).clear(cx);
            });
            assert!(
                visual.debug_bounds("restore-unsaved-edits").is_none(),
                "and it is not the offer to restore"
            );
            visual.executor().advance_clock(Duration::from_secs(2));
            visual.run_until_parked();
            visual.update(|window, cx| {
                assert!(
                    window.notifications(cx).is_empty(),
                    "successful recovery notice must expire even while edits remain unsaved"
                )
            });
            reader.read_with(visual, |reader, cx| {
                assert!(reader.link_notice.is_none());
                assert!(reader.source_is_dirty(cx));
            });
            reader.update_in(visual, |reader, _, cx| {
                reader.open_source_find(cx);
                reader.record_ui_state(true, cx);
                assert!(
                    layout(&root, cx).unwrap().0.live_preview,
                    "temporary Source during Find must not overwrite the saved choice"
                );
            });
        }
        {
            for _ in 0..2 {
                let before = reader.read_with(visual, |reader, cx| {
                    let position = reader.content.read(cx).list_state().logical_scroll_top();
                    (
                        reader.source_scroll_offset(cx),
                        position.item_ix,
                        position.offset_in_item,
                    )
                });
                visual.update(|window, cx| window.draw(cx).clear(cx));
                let body = visual.debug_bounds("reader-document").unwrap();
                visual.simulate_event(ScrollWheelEvent {
                    position: point(body.center().x, body.top() + px(130.)),
                    delta: ScrollDelta::Pixels(point(px(0.), px(-24.))),
                    ..Default::default()
                });
                visual.run_until_parked();
                let after = reader.read_with(visual, |reader, cx| {
                    let position = reader.content.read(cx).list_state().logical_scroll_top();
                    (
                        reader.source_scroll_offset(cx),
                        position.item_ix,
                        position.offset_in_item,
                    )
                });
                assert_ne!(before, after, "positive control: restored content scrolls");
                visual.update(|window, cx| window.draw(cx).clear(cx));
                assert_eq!(
                    visual
                        .debug_bounds("document-header-viewport")
                        .unwrap()
                        .size
                        .height,
                    px(48.),
                    "restored header stays pinned during explicit scrolling"
                );
            }
        }
    }
}

#[cfg(test)]
mod update_restart_tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[cfg(test)]
    #[gpui::test]
    fn update_restart_requires_successful_state_persistence(cx: &mut gpui::TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            install(temp.path(), cx);
            record(Path::new("/fixture"), Layout::default(), true, cx);
            prepare_for_restart(cx).unwrap();
            let path = temp.path().join("reader-ui.json");
            assert!(path.is_file());
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            let layout = Layout {
                note: "Changed.md".into(),
                ..Default::default()
            };
            record(Path::new("/fixture"), layout, true, cx);
            assert!(
                prepare_for_restart(cx).is_err(),
                "Do not quit when Reader state cannot be committed"
            );
        });
    }
}

pub(crate) fn typed_views(cx: &App) -> okilum_core::typed_view::Preferences {
    cx.try_global::<Store>()
        .map(|store| store.saved.typed_views.clone())
        .unwrap_or_default()
}

/// Unknown/future preference values safely ask rather than replacing a vault.
pub(crate) fn vault_open(cx: &App) -> &'static str {
    match cx
        .try_global::<Store>()
        .map(|s| s.saved.vault_open.as_str())
    {
        Some("current") => "current",
        Some("new") => "new",
        _ => "ask",
    }
}
pub(crate) fn set_vault_open(value: &str, cx: &mut App) {
    if !installed(cx) || vault_open(cx) == value {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.vault_open = value.into();
    state.vault_open_changed = true;
    schedule(cx);
    cx.refresh_windows();
}

/// Last explicitly visited Settings pane, shared by every window in this profile.
pub(crate) fn settings_section(cx: &App) -> Option<&str> {
    cx.try_global::<Store>()
        .map(|state| state.saved.settings_section.as_str())
}

pub(crate) fn set_settings_section(value: &str, cx: &mut App) {
    if !installed(cx) || settings_section(cx) == Some(value) {
        return;
    }
    let state = cx.global_mut::<Store>();
    state.saved.settings_section = value.into();
    state.settings_section_changed = true;
    schedule(cx);
}
