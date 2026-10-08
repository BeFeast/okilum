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
    reading_width: f32,
    find_case_sensitive: bool,
    typed_views: tessera_core::typed_view::Preferences,
    toolbar_labels: bool,
    vaults: BTreeMap<PathBuf, Layout>,
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
            reading_width: READER_MAX_WIDTH,
            find_case_sensitive: false,
            typed_views: Default::default(),
            toolbar_labels: false,
            vaults: Default::default(),
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
    width_changed: bool,
    find_changed: bool,
    typed_views_changed: bool,
    toolbar_labels_changed: bool,
    frames_changed: BTreeSet<String>,
    last_changed: bool,
    last_frame_changed: bool,
    readers: Vec<WeakEntity<Reader>>,
    blocked: bool,
    serial: Arc<AtomicU64>,
    pending: Option<Task<()>>,
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
        width_changed: !path.exists(),
        find_changed: false,
        // No migration input: opening a new window must not overwrite a
        // different process's newly saved mappings with our startup defaults.
        typed_views_changed: false,
        toolbar_labels_changed: false,
        frames_changed: Default::default(),
        last_changed: false,
        last_frame_changed: false,
        readers: vec![],
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
    width_changed: bool,
    find_changed: bool,
    typed_views_changed: bool,
    toolbar_labels_changed: bool,
    serial: Arc<AtomicU64>,
    generation: u64,
}
impl WriteJob {
    fn run(&self) -> anyhow::Result<()> {
        let directory = self.path.parent().context("Missing UI state directory")?;
        // Match the existing outside-vault guard, including symlink ancestors.
        for root in self.saved.vaults.keys() {
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
        if self.font_changed {
            latest.font_size = self.saved.font_size;
        }
        if self.typed_views_changed {
            latest.typed_views = self.saved.typed_views.clone();
        }
        if self.width_changed {
            latest.reading_width = self.saved.reading_width;
        }
        if self.find_changed {
            latest.find_case_sensitive = self.saved.find_case_sensitive;
        }
        if self.toolbar_labels_changed {
            latest.toolbar_labels = self.saved.toolbar_labels;
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
            && !state.font_changed
            && !state.width_changed
            && !state.find_changed
            && !state.typed_views_changed
            && !state.toolbar_labels_changed
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
        width_changed: state.width_changed,
        find_changed: state.find_changed,
        typed_views_changed: state.typed_views_changed,
        toolbar_labels_changed: state.toolbar_labels_changed,
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
#[cfg(any(windows, test))]
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
            let reader = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = reader.update(cx, |reader, cx| {
                    reader.ui_state.source_position_pending = false;
                    cx.notify();
                });
            });
        }
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
        if !active
            && cx.global::<Store>().readers.iter().any(|other| {
                other.entity_id() != cx.entity_id()
                    && other.upgrade().is_some_and(|other| {
                        let other = other.read(cx);
                        other.ui_state.active && other.vault_root == self.vault_root
                    })
            })
        {
            self.ui_state.active = false;
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
            note: self.current_rel.clone(),
            history,
            history_index: self.navigation.history_ix,
            position: position.into(),
            source: self.editing.is_some() || self.ui_state.source.is_some(),
            source_scroll: offset,
        };
        if self.ui_state.last.as_ref() != Some(&saved) || (active && !self.ui_state.active) {
            self.ui_state.last = Some(saved.clone());
            record(&self.vault_root, saved, active, cx);
        }
        self.ui_state.active = active;
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

pub(crate) fn font_size(cx: &App) -> f32 {
    cx.try_global::<Store>().map_or(BODY_FONT_SIZE, |state| {
        state.saved.font_size.clamp(12., 24.)
    })
}
pub(crate) fn reading_width(cx: &App) -> f32 {
    cx.try_global::<Store>().map_or(READER_MAX_WIDTH, |state| {
        state.saved.reading_width.clamp(560., 1200.)
    })
}
// Settings owns the production caller in the parallel #623 redesign.
#[cfg_attr(not(test), allow(dead_code))]
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
        state.width_changed = false;
        state.find_changed = false;
        state.typed_views_changed = false;
        state.toolbar_labels_changed = false;
        state.last_changed = false;
        state.last_frame_changed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

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
                    typed_views: tessera_core::typed_view::Preferences {
                        mappings: BTreeMap::from([
                            ("Project".into(), "tasks".into()),
                            ("Future".into(), "optional-view".into()),
                        ]),
                        tasks: tessera_core::typed_view::layout::Defaults {
                            density: tessera_core::typed_view::layout::Density::Comfortable,
                            grouping: tessera_core::typed_view::layout::Grouping::Note,
                        },
                    },
                    ..Default::default()
                },
                serial: Arc::new(AtomicU64::new(1)),
                generation: 1,
                appearance_changed: false,
                theme_changed: false,
                font_changed: true,
                width_changed: false,
                find_changed: false,
                typed_views_changed: true,
                toolbar_labels_changed: false,
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
                !merged.find_case_sensitive,
                "positive control: the search setting was saved"
            );
            assert_eq!(
                merged.typed_views, independent.saved.typed_views,
                "unrelated writes preserve mappings, defaults and unknown view IDs"
            );
            assert_eq!(
                tessera_core::typed_view::select(
                    "---\ntype: Project\n---\n",
                    &merged.typed_views.mappings
                ),
                tessera_core::typed_view::Selection::Native(tessera_core::typed_view::TASKS)
            );
            assert_eq!(
                tessera_core::typed_view::select(
                    "---\ntype: project\n---\n",
                    &merged.typed_views.mappings
                ),
                tessera_core::typed_view::Selection::Markdown
            );
            assert!(matches!(
                tessera_core::typed_view::select(
                    "---\ntype: Future\n---\n",
                    &merged.typed_views.mappings
                ),
                tessera_core::typed_view::Selection::Fallback(_)
            ));

            assert_eq!(
                merged.font_size, 22.,
                "unrelated global edits merge per field"
            );
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
        restored_document_state(cx, false);
    }

    #[gpui::test]
    fn restored_source_hides_preview_until_source_viewport_is_ready(cx: &mut TestAppContext) {
        restored_document_state(cx, true);
    }

    fn restored_document_state(cx: &mut TestAppContext, source: bool) {
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
                    assert_eq!(reader.source_scroll_offset(cx).unwrap().y, px(-400.));
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
                assert_eq!(reader.source_scroll_offset(cx).unwrap().y, px(-400.));
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

pub(crate) fn typed_views(cx: &App) -> tessera_core::typed_view::Preferences {
    cx.try_global::<Store>()
        .map(|store| store.saved.typed_views.clone())
        .unwrap_or_default()
}
