#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! Okilum desktop shell — a reader over `okilum-core`, on GPUI via
//! `gpui-component`.
//!
//! Ported from the framework spike. The probe instrumentation that existed to
//! decide the framework (self-driven scroll sweeps, height dumps, frame
//! timing) is deliberately gone: it measured a decision that has been made.

#[macro_use]
#[path = "../../okilum-core/src/diagnostic_io.rs"]
mod diagnostic_io;

// Keep transient native frame allocations local to the rendering thread.
// The test allocation meter wraps this same backend instead of replacing it.
#[cfg(target_os = "linux")]
pub(crate) const NATIVE_ALLOCATION: mimalloc::MiMalloc = mimalloc::MiMalloc;
#[cfg(all(test, not(target_os = "linux")))]
pub(crate) const NATIVE_ALLOCATION: std::alloc::System = std::alloc::System;
#[cfg(all(target_os = "linux", not(test)))]
#[global_allocator]
static NATIVE_ALLOCATOR: mimalloc::MiMalloc = NATIVE_ALLOCATION;

mod about;
mod app_footprint;
#[cfg(all(target_os = "linux", feature = "appimage-acceptance-harness"))]
mod appimage_acceptance;
#[cfg(all(unix, feature = "brain"))]
use okilum_shell_brain::{brain, connectors, export};
#[cfg(all(test, unix, feature = "brain"))]
mod brain_reader_tests;
mod build_info;
mod desktop_app_menu;
#[cfg(target_os = "macos")]
mod folder_service;
use okilum_shell_logs::{kubernetes_workspace, reader_log_kubernetes, reader_log_sftp};
mod linux_sprint;
mod open_in;
mod prepared_links;
mod project;
mod project_terminal;
mod quick_open;
mod reader_actions;
mod reader_agent_document;
#[cfg(not(all(unix, feature = "brain")))]
mod reader_app_menu;
use okilum_shell_viewers::{
    reader_audio, reader_canvas, reader_drawing, reader_image, reader_media, reader_plain_text,
    reader_segmented, reader_status_line,
};
mod reader_archive;
mod reader_archive_vault;
mod reader_cache;
mod reader_capture;
mod reader_code_file;
#[cfg(any(unix, windows))]
mod reader_confirm;
mod reader_context_menu;
#[cfg(any(unix, windows))]
mod reader_create;
#[cfg(windows)]
mod reader_creation_undo_windows;
mod reader_deep_link;
mod reader_deleted_note;
mod reader_delimited;
mod reader_diagnostics;
mod reader_document;
mod reader_document_menu;
mod reader_document_navigation;
mod reader_drawing_edit;
#[cfg(any(unix, windows))]
mod reader_editor;
mod reader_encoding;
#[cfg(any(unix, windows))]
mod reader_external_changes;
mod reader_feedback;
mod reader_file_copy;
mod reader_files;
mod reader_find_field;
mod reader_fonts;
mod reader_har;
#[cfg(target_os = "linux")]
mod reader_header_cache;
mod reader_hover;
#[cfg(any(target_os = "macos", test))]
mod reader_icloud;
mod reader_image_actions;
mod reader_incremental;
mod reader_instance;
mod reader_layout;
mod reader_link_navigation;
mod reader_loading;
mod reader_log;
#[cfg(any(native_manager, test))]
mod reader_manager_links;
#[cfg(test)]
mod reader_manager_occlusion;
mod reader_manager_panel;
use okilum_shell_project::reader_t3_settings;
mod reader_markdown;
#[cfg(test)]
mod reader_markdown_conformance_tests;
#[cfg(any(unix, windows))]
mod reader_move;
#[cfg(any(unix, windows))]
mod reader_move_picker;
mod reader_navigation;
#[cfg(test)]
mod reader_obsidian_reader_tests;
mod reader_open;
mod reader_open_windows;
mod reader_outline;
mod reader_owned_windows;
mod reader_pane;
#[cfg(target_os = "linux")]
mod reader_panel_resize;
mod reader_panes;
mod reader_path_moves;
mod reader_pdf;
mod reader_perf_probe;
mod reader_pin_row;
mod reader_project_create;
mod reader_project_navigation;
mod reader_properties;
mod reader_publication;
mod reader_read_menu;
mod reader_reading_controls;
#[cfg(any(unix, windows))]
mod reader_recovery;
mod reader_replay;
mod reader_right_panel;
mod reader_service_storage;
mod reader_session;
mod reader_settings;
mod reader_sidebar_view;
mod reader_url;
mod reader_viewer_chrome;
#[cfg(feature = "data-view-native-harness")]
mod reader_viewer_native;
#[cfg(test)]
mod reader_viewer_tests;
mod reader_window_state;
mod reader_window_view;
// One module for Linux and macOS: an `include!` wrapper made every name in
// it macro-expanded on macOS only, so `use super::*` plus an explicit
// `use ::core::prelude::v1::test` was E0659-ambiguous there (#2589).
mod reader_keymap;
mod reader_now_waiting;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod reader_settings_sync;
mod reader_settings_vaults;
mod reader_shortcuts;
mod reader_side_panel;
mod reader_split;
mod reader_stignore;
#[cfg(target_os = "linux")]
mod reader_sync_conflicts;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod reader_sync_merge;
mod reader_table_copy;
mod reader_title_nav;
mod reader_vaults;
mod reader_workbench;
mod t3_sign_in_cli;
mod vault_name;
mod version_info;
use reader_link_navigation::handle_link;
#[cfg(test)]
mod input_anchor_tests;
#[cfg(test)]
mod input_frame_cost_tests;
#[cfg(test)]
mod input_navigation_tests;
#[cfg(test)]
mod input_newline_tests;
#[cfg(test)]
mod issue3073_tests;
#[cfg(test)]
mod reader_agent_imports_tests;
#[cfg(test)]
mod reader_agent_language_tests;
mod reader_agent_search;
mod reader_agents_offer;
mod reader_document_decorations;
mod reader_features;
mod reader_file_access_notice;
#[cfg(test)]
mod test_state_isolation_tests;
#[cfg(test)]
use reader_features::test_init;
mod reader_batch;
#[cfg(any(unix, windows))]
mod reader_daily_notes;
mod reader_recent;
mod reader_recent_clear;
#[cfg(any(unix, windows))]
mod reader_recovery_rows;
mod reader_reminder;
mod reader_reminder_notify;
mod reader_reminder_settings;
#[cfg(test)]
mod reader_scroll_geometry_tests;
#[cfg(any(unix, windows))]
mod reader_source_history;
mod reader_startup;
#[cfg(test)]
mod reader_table_tests;
mod reader_task_actions;
mod reader_tasks;
#[cfg(any(unix, windows))]
mod reader_templates;
mod reader_text_zoom;
#[cfg(any(target_os = "macos", all(test, unix)))]
mod reader_thumbnail;
#[cfg(any(unix, windows))]
mod reader_timeline;
mod reader_toast;
#[cfg(any(unix, windows))]
mod reader_trash;
mod reader_tree;
mod reader_tree_breadcrumbs;
mod reader_tree_follow;
mod reader_tree_preview;
mod reader_tree_rules;
mod reader_typed_view;
mod reader_ui_state;
mod reader_untitled;
mod reader_update_restart;
mod reader_view_query;
mod reader_welcome;
mod search_label;
mod sidebar_view_tree;
mod text_ranges;
mod theme_picker;
mod ui_zoom;
mod ui_zoom_diagnostics;
mod updater;
// Search labels reuse the editor's glyph-cluster selection geometry. Keep this
// adapter unchanged here; #780 owns editor layout and geometry refinements.
#[allow(dead_code)]
#[rustfmt::skip]
#[path = "../../../vendor/gpui-component/crates/base/src/input/bidi_geometry.rs"]
mod vendor_bidi_geometry;
#[cfg(test)]
#[allow(dead_code)]
#[rustfmt::skip]
#[path = "../../../vendor/gpui-component/crates/base/src/input/document_newlines.rs"]
mod vendor_document_newlines;
#[cfg(test)]
#[allow(dead_code)]
#[rustfmt::skip]
#[path = "../../../vendor/gpui-component/crates/base/src/text_boundary.rs"]
mod vendor_text_boundary;
mod window_state;
#[cfg(test)]
mod windows_note_clipboard_reader_tests;
#[cfg(all(unix, feature = "brain"))]
mod workspace;

use reader_actions::*;
#[cfg(test)]
use reader_agent_document::agent_publication_is_current;
use reader_document_navigation::{reader_landing_state, ReaderLanding};
use reader_markdown::{
    backlink_occurrence, markdown_plugins, note_outline, reader_plugins, reader_plugins_at,
    reader_text_style, MarkdownImage, MarkdownLinkHandler, MarkdownScope,
};
#[cfg(test)]
use reader_sidebar_view::{
    reader_item_menu, sidebar_visible_actions, SIDEBAR_ACTION_SLOT, SIDEBAR_NAME_GAP,
    SIDEBAR_NAME_MIN, VAULT_DOT_SLOT,
};
use reader_sidebar_view::{
    sidebar_action_click, SideItem, SidebarAction, SidebarNameItem, TreeKey, HIDDEN_FILES_MENU,
    TREE_ROW_HEIGHT, VAULT_ACCENT_LINE,
};
#[cfg(test)]
use reader_window_view::reader_more_menu;
use reader_window_view::{
    header_controls, note_gutter, preserve_reader_selection, reader_icon_button,
    reader_titlebar_options, READER_HEADER_HEIGHT, READER_SIDE_PADDING, READER_SLIDERS_ICON,
};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use brand::Assets;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_base::ElementExt as _;
use gpui_component::menu::ContextMenuExt as _;
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    text::{markdown_ast, MarkdownNode, SelectionFormat, TextView, TextViewState, TextViewStyle},
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Root, Selectable as _,
    Sizable as _, Theme, TitleBar,
};
// The schemes come from core, not from copies here. The shell only needs to
// recognise what core emitted; two independent definitions are free to drift,
// and the click handler would then quietly stop matching links the renderer
// had produced.
use okilum_core::callout::{self, CalloutHeader, CalloutKind};
use okilum_core::render::{
    split_open_url, AMBIGUOUS_SCHEME, EMBED_LANG, EMBED_MISSING, UNRESOLVED_SCHEME, WIKI_SCHEME,
};
use okilum_core::{Backlink, Searcher, Vault, VaultWatcher};

// Former app modules, now in okilum-shell-kit (#3233); `crate::brand::…`
// paths keep working through these imports.
use okilum_shell_kit::appearance::{appearance_settings_path, AppearancePreference};
#[cfg(test)]
use okilum_shell_kit::reader_layout::READER_MAX_WIDTH;
use okilum_shell_kit::{
    brand, contextual_help, count_label, reader_history, reader_projects_help,
    reader_recent_vaults, reader_sidebar, reader_sync_help,
};
// Former app modules, now in okilum-shell-platform (#3234).
#[cfg(test)]
use okilum_shell_platform::network_write_qa;
#[cfg(any(windows, test))]
use okilum_shell_platform::own_window_capture;
#[cfg(windows)]
use okilum_shell_platform::{markdown_handler, url_protocol};
use okilum_shell_platform::{
    note_paste, note_upload, pdf_engine, platform, reader_ssh_askpass, reader_trash_fs,
};
// Former app modules, now in okilum-shell-text (#3235).
use okilum_shell_text::{
    reader_agent_imports, reader_agent_language, reader_agents_context, reader_code,
    reader_code_language, reader_embeds, reader_mermaid, reader_obsidian, reader_remote_markdown,
    source_presentation,
};

/// Body size for the reader; the heading scale and the sidebar derive from it.
const BODY_FONT_SIZE: f32 = brand::READING_FONT_SIZE;
/// Reader and source editor reserve the same scrollable end space (#419).
fn reader_bottom_space(viewport_height: Pixels) -> Pixels {
    px(120.).max(viewport_height * 0.30)
}

/// `WeakEntity::update_in` for a task spawned with `cx.spawn_in(window, ..)`, in
/// that window (#1314). gpui's own `update_in` resolves the window from the
/// entity's "current window": the last window whose draw read the entity, and
/// none once that window is closed. Settings and About read the Reader while
/// they draw, so closing one left the Reader without a window, and the next
/// `update_in` of every task of the Reader failed: the watcher timer ended for
/// the rest of the session, and no change made outside the app was seen again.
trait UpdateInOwnWindow {
    fn update_in_own_window<R>(
        &self,
        cx: &mut AsyncWindowContext,
        update: impl FnOnce(&mut Reader, &mut Window, &mut Context<Reader>) -> R,
    ) -> anyhow::Result<R>;
}

impl UpdateInOwnWindow for WeakEntity<Reader> {
    fn update_in_own_window<R>(
        &self,
        cx: &mut AsyncWindowContext,
        update: impl FnOnce(&mut Reader, &mut Window, &mut Context<Reader>) -> R,
    ) -> anyhow::Result<R> {
        cx.update(|window, app| self.update(app, |reader, cx| update(reader, window, cx)))?
    }
}

fn appearance_label(cx: &App) -> &'static str {
    match cx.try_global::<AppearancePreference>().and_then(|p| p.0) {
        None => "System",
        Some(gpui_component::ThemeMode::Light) => "Light",
        Some(gpui_component::ThemeMode::Dark) => "Dark",
    }
}
fn sync_appearance(window: &mut Window, cx: &mut App) {
    match cx.try_global::<AppearancePreference>().and_then(|p| p.0) {
        Some(mode) => Theme::change(mode, Some(window), cx),
        None => Theme::sync_system_appearance(Some(window), cx),
    }
    brand::apply_theme(cx);
}
#[cfg(all(unix, feature = "brain"))]
fn cycle_appearance(window: &mut Window, cx: &mut App) {
    let next = match cx.try_global::<AppearancePreference>().and_then(|p| p.0) {
        None => Some(gpui_component::ThemeMode::Light),
        Some(gpui_component::ThemeMode::Light) => Some(gpui_component::ThemeMode::Dark),
        Some(gpui_component::ThemeMode::Dark) => None,
    };
    set_appearance(next, None, window, cx);
}
/// `None` follows the system. The choice is app config, never note content.
fn set_appearance(
    mode: Option<gpui_component::ThemeMode>,
    vault: Option<&Path>,
    window: &mut Window,
    cx: &mut App,
) {
    cx.set_global(AppearancePreference(mode));
    sync_appearance(window, cx);
    window.refresh();
    cx.refresh_windows();
    if reader_ui_state::set_appearance(mode, cx) {
        return;
    }
    save_appearance(vault, cx);
}
/// One app-wide color theme (#349); Light/Dark/System stays a separate choice.
fn set_theme(theme: brand::ThemeId, vault: Option<&Path>, window: &mut Window, cx: &mut App) {
    #[cfg(test)]
    cx.set_global(theme_picker::ThemeActionVault(vault.map(Path::to_path_buf)));
    cx.set_global(brand::ThemeChoice(theme));
    sync_appearance(window, cx);
    window.refresh();
    cx.refresh_windows();
    if reader_ui_state::set_theme(theme, cx) {
        return;
    }
    save_appearance(vault, cx);
}
fn save_appearance(vault: Option<&Path>, cx: &App) {
    #[cfg(test)]
    let _ = (vault, cx);
    #[cfg(not(test))]
    if let Some(path) = appearance_settings_path().filter(|path| {
        // Never write presentation state into the opened canonical tree.
        vault.is_none_or(|vault| reader_layout::settings_path_at(vault, path.clone()).is_some())
    }) {
        let value = match cx.try_global::<AppearancePreference>().and_then(|p| p.0) {
            None => "system",
            Some(gpui_component::ThemeMode::Light) => "light",
            Some(gpui_component::ThemeMode::Dark) => "dark",
        };
        let saved = (|| {
            let parent = path
                .parent()
                .ok_or_else(|| std::io::Error::other("missing settings directory"))?;
            std::fs::create_dir_all(parent)?;
            let temporary = parent.join(format!(".appearance-{}.json", uuid::Uuid::new_v4()));
            std::fs::write(
                &temporary,
                appearance_settings_json(value, brand::theme_id(cx)).to_string(),
            )?;
            std::fs::rename(&temporary, &path).inspect_err(|_| {
                let _ = std::fs::remove_file(&temporary);
            })
        })();
        if let Err(error) = saved {
            diagnostic_error!("Could not save appearance: {error}");
        }
    }
}
fn appearance_settings_json(appearance: &str, theme: brand::ThemeId) -> serde_json::Value {
    serde_json::json!({ "appearance": appearance, "theme": theme.key() })
}
/// Unknown or missing values fall back to System and the default theme; a file
/// written before #349 has no `theme` key.
fn parse_appearance_settings(
    value: &serde_json::Value,
) -> (AppearancePreference, brand::ThemeChoice) {
    let mode = match value["appearance"].as_str() {
        Some("light") => Some(gpui_component::ThemeMode::Light),
        Some("dark") => Some(gpui_component::ThemeMode::Dark),
        _ => None,
    };
    let theme = value["theme"]
        .as_str()
        .and_then(brand::ThemeId::from_key)
        .unwrap_or_default();
    (AppearancePreference(mode), brand::ThemeChoice(theme))
}
/// The saved Light/Dark/System choice and theme. A start after a crash keeps
/// them too (#956): safe startup resets layout (panels stay closed), but two
/// validated preference values cannot cause a crash, and dropping them
/// showed System while the menu and file still said Light.
fn load_appearance(cx: &App) -> (AppearancePreference, brand::ThemeChoice) {
    if let Some(saved) = reader_ui_state::appearance(cx) {
        return (
            saved,
            brand::ThemeChoice(reader_ui_state::theme(cx).unwrap_or_default()),
        );
    }
    appearance_settings_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .map(|value| parse_appearance_settings(&value))
        .unwrap_or_default()
}

#[cfg(test)]
type IndexBuildHook = Arc<dyn Fn(&Path) + Send + Sync>;
#[cfg(test)]
type FirstDocumentRead = Arc<dyn Fn(&Path) -> std::io::Result<Vec<u8>> + Send + Sync>;

#[derive(Default, Clone)]
struct Opts {
    /// Worker-only inventory reuse during a full refresh of the current root.
    publication_vault: Option<Arc<Vault>>,
    /// Opt-in backend client; the ordinary reader remains unchanged.
    brain_endpoint: Option<std::net::SocketAddr>,
    /// Open the managed workspace chooser only by explicit request.
    managed_workspace: bool,
    /// OS document delivery may arrive after launch; never auto-connect while awaiting it.
    #[cfg(all(unix, feature = "brain"))]
    defer_saved_connection: bool,
    /// Vault root. Required: there is no default vault, and guessing one would
    /// be a good way to index the wrong directory.
    vault: Option<PathBuf>,
    open_path: Option<PathBuf>,
    /// Exact window set supplied by the updater, independent of launch defaults.
    restart_windows: Vec<reader_update_restart::Window>,
    /// An explicit CLI/OS open created this empty Markdown file (#1835).
    created_note: bool,
    /// More log files opened with `open_path`, in one view (#1160).
    extra_logs: Vec<PathBuf>,
    single_file: bool,
    /// OS double-click of a Markdown file inside a marked vault opens the
    /// instant quick viewer first instead of focusing an open vault window.
    prefer_quick_viewer: bool,
    defer_loading: bool,
    reusable_roots: Vec<PathBuf>,
    session_directory: Option<PathBuf>,
    exact_restore: bool,
    /// Session replay may supply a note without making it an external intent.
    restore_workspace: bool,
    diagnostics: Option<reader_diagnostics::Trace>,
    /// Explicit manual refresh verifies canonical bytes even with unchanged revisions.
    force_source_read: bool,
    reconcile_recent: Vec<String>,
    #[cfg(test)]
    preparation_hold: Option<async_channel::Receiver<()>>,
    #[cfg(test)]
    validation_hold: Option<async_channel::Receiver<()>>,
    #[cfg(test)]
    panel_preferences_hold: Option<async_channel::Receiver<()>>,
    #[cfg(test)]
    panel_settings_override: Option<PathBuf>,
    #[cfg(test)]
    rest_snapshot_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    index_build_hook: Option<IndexBuildHook>,
    #[cfg(test)]
    first_document_read: Option<FirstDocumentRead>,
    #[cfg(test)]
    search_publish_in_place: bool,
    cache_lease: Option<Arc<reader_cache::Lease>>,
    /// Explicit search index override. Direct opens default to application cache.
    index_dir: Option<PathBuf>,
    #[cfg(test)]
    cache_base_override: Option<PathBuf>,
    use_html: bool,
    note: Option<String>,
    query: Option<String>,
    jump: bool,
    copy_source: bool,
    /// `--no-history`: this process records no recent vaults or files (#3208).
    no_history: bool,
    /// An external `okilum:` link to open (#1049, docs/deep-links.md).
    link: Option<String>,
    /// Where the link lands in `note` once it is open (#1049).
    landing: Option<okilum_core::deep_link::Position>,
    /// An `okilum://logs` link awaiting the vault prompt (#1441): consent
    /// opens the vault's Kibana window with it instead of a Reader.
    remote_logs: Option<okilum_core::remote_log::link::RemoteLink>,
}

struct Reader {
    #[cfg(target_os = "linux")]
    header_cache: Entity<reader_header_cache::Header>,
    #[cfg(target_os = "linux")]
    layout_signal: Entity<()>,
    #[cfg(target_os = "linux")]
    panel_capture: reader_panel_resize::Capture,
    ui_state: reader_ui_state::Session,
    single_file: bool,
    archive_state: okilum_core::vault_descriptor::ArchiveState,
    archive_successor_root: Option<PathBuf>,
    archive_successor_ambiguous: Vec<PathBuf>,
    archive_lookup_notice: Option<reader_archive_vault::ArchiveLookupNotice>,
    quick_viewer_vault: Option<reader_archive_vault::QuickViewerVault>,
    tree_preview: reader_tree_preview::Session,
    tree_sticky: Vec<reader_tree::Row>,
    tree_earlier_roots: Vec<reader_tree::Row>,
    project: Option<Entity<project::ProjectView>>,
    project_open: bool,
    project_navigation: reader_project_navigation::State,
    /// Title-bar switcher and Recent/Pinned menus (#1508).
    title_nav: reader_title_nav::State,
    project_request: u64,
    project_views: std::collections::BTreeMap<(PathBuf, String), Entity<project::ProjectView>>,
    file_menu: Option<(Entity<gpui_component::menu::PopupMenu>, Point<Pixels>)>,
    viewer_save_controls: std::cell::RefCell<Option<Entity<reader_viewer_chrome::SaveControls>>>,
    viewer_header: std::cell::RefCell<Option<Entity<reader_viewer_chrome::Header>>>,
    viewer_save_bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    viewer_find_controls: std::cell::RefCell<Option<Entity<reader_viewer_chrome::FindControls>>>,
    viewer_find_bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    viewer_find_min_x: std::rc::Rc<std::cell::Cell<Pixels>>,
    #[cfg(target_os = "linux")]
    sync_note_conflicts: Option<(PathBuf, String, Vec<PathBuf>, bool)>,
    /// Undo history and selection of the editor last left for the Reader, by
    /// file: Edit resumes it when the file's text is unchanged (#1095).
    retained_edit: Option<(String, gpui_component::input::EditHistory)>,
    #[cfg(any(unix, windows))]
    creation: Option<reader_create::Creation>,
    #[cfg(any(unix, windows))]
    creation_undo: Option<Arc<reader_create::CreatedUndo>>,
    #[cfg(any(unix, windows))]
    reminder_undo: Option<Arc<okilum_core::reminder_append::write::Receipt>>,
    #[cfg(any(unix, windows))]
    reminder_notifier: reader_reminder_notify::Notifier,
    #[cfg(any(unix, windows))]
    reminder_prefs: reader_reminder_settings::Preferences,
    #[cfg(any(unix, windows))]
    reminder_prefs_error: Option<String>,
    /// Daily notes preference per vault root, read on demand (#3182).
    #[cfg(any(unix, windows))]
    daily_notes: Option<(PathBuf, Result<reader_daily_notes::Prefs, String>)>,
    /// Why the last daily notes choice in Settings was not applied.
    #[cfg(any(unix, windows))]
    daily_notes_error: Option<String>,
    /// Templates offered for daily notes, read with the preference.
    #[cfg(any(unix, windows))]
    daily_templates: reader_daily_notes::Templates,
    /// The first-press daily notes dialog, while it is open.
    #[cfg(any(unix, windows))]
    daily_setup: Option<Entity<reader_daily_notes::Setup>>,
    /// Show the reminders note through the native Tasks view (#919).
    #[cfg(any(unix, windows))]
    reminders_view: bool,
    #[cfg(any(unix, windows))]
    renaming: Option<reader_move::Renaming>,
    #[cfg(any(unix, windows))]
    move_picker: reader_move_picker::PickerState,
    batch: reader_batch::State,
    #[cfg(any(unix, windows))]
    note_move_pending: bool,
    #[cfg(any(unix, windows))]
    move_notice_generation: u64,
    #[cfg(any(unix, windows))]
    move_applying: bool,
    #[cfg(any(unix, windows))]
    trash_pending: bool,
    #[cfg(any(unix, windows))]
    trash_undo: reader_trash::UndoHistory,
    #[cfg(any(unix, windows))]
    move_index: Option<Arc<okilum_core::link_rewrite::CandidateIndex>>,
    loading: Option<reader_loading::Loading>,
    queued_edit_note: Option<String>,
    /// An external link opened while editing: once its note is accepted, the
    /// editor returns there with the caret on the link's line (#1049).
    pending_link_edit: Option<reader_deep_link::EditLanding>,
    /// The Notes and «On this page» panels as cached views (#1290).
    side_panels: [Option<Entity<reader_side_panel::SidePanel>>; 2],
    /// The outline's current section as last drawn (#1290).
    outline_section: Option<usize>,
    /// The document's parse state as last seen; see `content_changed`.
    content_status: Option<bool>,
    shared_session: Option<reader_session::Shared>,
    shared_version: u64,
    shared_service_version: u64,
    /// When the current note was requested, for the `note_open_*` phases (#1003).
    open_started: Option<std::time::Instant>,
    /// Watches the current note's parse for `note_open_parsed`; replaced per open.
    note_body_trace: Option<Subscription>,
    session_directory: Option<PathBuf>,
    session_records: Option<async_channel::Sender<reader_loading::SessionRecord>>,
    /// The note part of the native window title; the vault part is added
    /// from its display name, so a rename can retitle the window (#1327).
    title_note: Option<String>,
    index_dir: Option<PathBuf>,
    cache_lease: Option<Arc<reader_cache::Lease>>,
    vault: Arc<Vault>,
    vault_publication_revision: (u64, u64),
    source_publication_epoch: u64,
    searcher: Option<Arc<Searcher>>,
    vault_root: PathBuf,
    /// The watcher lives here so it is dropped with the reader; the poll
    /// loop only borrows it through the entity.
    watcher: Option<VaultWatcher>,
    quick_folders: std::collections::BTreeSet<String>,
    quick_folder_refreshes: std::collections::BTreeMap<String, Arc<()>>,
    watcher_generation: u64,
    /// The independent selected-document watcher owns current canonical source.
    open_note_watch: Option<(PathBuf, String)>,
    watcher_poll_active: bool,
    #[cfg(test)]
    watcher_poll_hold: Option<async_channel::Receiver<()>>,
    deferred_vault_changes: okilum_core::Changes,
    incremental_state: Option<okilum_core::vault::warm::incremental::State>,
    tasks_index: Option<Arc<okilum_core::tasks::Index>>,
    incremental_initializing: bool,
    incremental_active: bool,
    incremental_epoch: u64,
    incremental_cancel: Option<reader_loading::Cancellation>,
    #[cfg(test)]
    incremental_hold: Option<async_channel::Receiver<()>>,
    reader_window: AnyWindowHandle,
    sidebar_search_focus: FocusHandle,
    recent_switcher: reader_recent::Switcher,
    quick_open: quick_open::Palette,
    shortcut_sheet: reader_shortcuts::Sheet,
    /// Transient "− 110% + Reset" overlay after a note text zoom (#1652).
    zoom_hud: reader_text_zoom::Hud,
    link_notice: Option<reader_toast::Notice>,
    displayed_deleted_notice: Option<(String, reader_deleted_note::Disk)>,
    deleted_notice_generation: u64,
    /// «Save the deleted note?» is open; a second navigation does not stack another.
    deleted_note_asking: bool,
    displayed_notice: Option<reader_toast::Notice>,
    displayed_choices: Vec<(String, Option<String>)>,
    displayed_recovery: Option<(String, u64)>,
    /// The «Restore unsaved edits» notice on screen, removed as soon as the
    /// offer no longer applies (#930).
    recovery_toast: Option<reader_toast::ToastKey>,
    /// Bumped on every offer change; a deferred push only shows the offer it
    /// was queued for (A → B → A cannot push A twice).
    recovery_toast_generation: u64,
    /// The link under the pointer, from every link hover event, stamped with
    /// the document generation so a link left behind by navigation is stale:
    /// a right-click there opens only the link's menu (#943).
    pointer_link: Option<(String, u64)>,
    read_press: Option<reader_context_menu::ReadPress>,
    displayed_history_notice: Option<(uuid::Uuid, uuid::Uuid, String)>,
    history_notice_generation: u64,
    notice_generation: u64,
    toast_subscription: Option<Subscription>,
    /// The active document's pane-owned state (#773).
    pane: reader_pane::Surface,
    /// The «Linked from» list renders only the cards in view: a hub note has thousands.
    backlinks_list: ListState,
    /// The Syncthing `.stignore` state of the open vault root (#1248), for Settings → Vault.
    stignore: Option<(PathBuf, okilum_core::syncthing_ignore::Status)>,
    /// Roots the offer was shown for in this launch: once per vault.
    stignore_offered: std::collections::HashSet<PathBuf>,
    stignore_busy: bool,
    /// The «On this page» rows; only the ones in view are built (#1004).
    outline_list: ListState,
    /// Opt-in large-note timing probe (#653); `None` on ordinary launches.
    perf_probe: Option<reader_perf_probe::Probe>,
    sel_format: SelectionFormat,
    /// Focus of the reader root: what receives the key bindings when no
    /// input has focus. Clicking the text hands focus to the TextView, which
    /// sits under the same key context, so the bindings keep working.
    focus_handle: FocusHandle,
    note_encoding_damaged: bool,
    encoding_repair_pending: Option<PathBuf>,
    draft_outline: reader_outline::DraftOutline,
    /// Folder tree over the published inventory (#335). Rebuilt only when the
    /// vault Arc or the current document changes.
    tree: reader_tree::Tree,
    tree_source: Option<Arc<Vault>>,
    tree_rules_source: Option<(std::sync::Weak<Vault>, u64)>,
    tree_rules_running: bool,
    tree_revealed: String,
    /// Settings «Show the open note in the file tree» (#1762), read from
    /// the app preference on every render.
    tree_auto_reveal: bool,
    /// The last wheel scroll over the tree (#1762, executor clock): while
    /// the user scrolls it, auto-reveal waits instead of moving the tree.
    tree_wheel_at: Option<std::time::Instant>,
    /// Auto-reveal is waiting for the tree to go quiet; set by `sync_tree`.
    tree_reveal_waiting: bool,
    tree_reveal_retry: Option<Task<()>>,
    /// A folder's index note opened from its own tree row (#1512): the file
    /// row, not the folder, then stands for it in the tree.
    tree_index_entry: Option<String>,
    tree_focus: FocusHandle,
    tree_scroll: UniformListScrollHandle,
    /// Recent / Pinned stores and the tree's collapse for this root (#369,
    /// #1508), app state.
    sidebar: reader_sidebar::State,
    sidebar_path: Option<PathBuf>,
    agents_offer: reader_agents_offer::Offer,
    agent_document: Option<AgentDocument>,
    agent_language: Option<reader_agent_language::Controller>,
    _agent_language_events: Option<Task<()>>,
    /// Inline marks of the published language matches on the open source
    /// editor, keyed by that editor and the published capture.
    agent_language_marks: Option<AgentLanguageMarks>,
    /// Editor, source revision and language publication the native hover
    /// was last valid for; a change clears its cached popup (#2953).
    hover_revision: Option<(
        EntityId,
        gpui_component::input::projection::SourceStamp,
        Option<u64>,
    )>,
    inbox: Vec<reader_sidebar::InboxItem>,
    projects: Arc<okilum_core::projects::Index>,
    view_query_index: Option<reader_view_query::Index>,
    view_query_generation: u64,
    workbench: Arc<okilum_core::workbench::Index>,
    workbench_ready: Option<Arc<okilum_core::workbench::Index>>,
    workbench_pending: u64,
    workbench_clock_minute: u64,
    workbench_times: std::collections::BTreeMap<u64, String>,
    workbench_scroll: ScrollHandle,
    /// Now's Waiting for me, read from bound manager threads (#2346).
    waiting: reader_now_waiting::Waiting,
    inbox_source: Option<Arc<Vault>>,
    recent_expanded: bool,
    sidebar_save_sequence: Arc<std::sync::atomic::AtomicU64>,
    /// Root whose first Inbox result arrived; the tree re-reveals once then.
    inbox_ready_root: Option<PathBuf>,
    /// Display titles of notes (#381), read from their first bytes. Shared
    /// with the quick-open worker, which matches and labels by them (#645).
    backlink_titles: Arc<std::collections::HashMap<String, String>>,
    panels: reader_layout::Panels,
    embedded_in_workspace: bool,
    panel_widths: reader_layout::Widths,
    panel_settings: Option<PathBuf>,
    panel_preferences_ready: bool,
    panel_widths_revision: u64,
    body_bounds: Bounds<Pixels>,
    // The viewport associated with body_bounds, so resize rendering never uses
    // the previous frame's dock/overlay width for the new viewport.
    body_viewport_width: Pixels,
    resizing_panel: Option<reader_layout::Panel>,
    /// Maximize or a panel hide left focus where it was; if that change stops
    /// drawing the focused element, the focus-lost listener moves focus to
    /// the document (#2957).
    refocus_if_lost: bool,
    /// The second pane, when the window is split (#773).
    split: Option<reader_pane::Split>,
    _subs: Vec<Subscription>,
}

impl Reader {
    fn note_label(&self, path: &str) -> String {
        if let Some(title) = quick_open::drawing_title(path) {
            return title;
        }
        if !self.tree.is_directory(path)
            && Path::new(path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            self.backlink_titles
                .get(path)
                .cloned()
                .unwrap_or_else(|| self.vault.note_title(path))
        } else {
            path.rsplit('/').next().unwrap_or(path).to_owned()
        }
    }

    /// The title shared by attachment breadcrumbs and the native window.
    fn selected_title(&self) -> String {
        if self.pane.file_preview.is_some() {
            self.note_label(self.selected_file())
        } else {
            self.pane.current_title.clone()
        }
    }
}

/// Height of one «On this page» row: the 13 px chrome text line plus 3 px above and below (#1004).
const OUTLINE_ROW_HEIGHT: f32 = 27.;
/// Vertical padding of the «On this page» list, above and below the rows (`py_1`).
const OUTLINE_PADDING_Y: f32 = 4.;
/// Places shown per linking note before «Show N more» (#394).
const BACKLINK_PLACES_SHOWN: usize = 3;

/// A note's display title from its first 16 KiB: first `# ` heading, then a
/// frontmatter `title:`, else `None` (callers fall back to the file name).
fn display_title(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut head = Vec::with_capacity(16 * 1024);
    std::fs::File::open(path)
        .ok()?
        .take(16 * 1024)
        .read_to_end(&mut head)
        .ok()?;
    display_title_bytes(&head)
}

fn display_title_source(source: &str) -> Option<String> {
    // Reconciliation already owns these bytes; do not miss an H1 after a long
    // introduction or perform another filesystem read for sidebar labels.
    display_title_bytes(source.as_bytes())
}

fn display_title_bytes(head: &[u8]) -> Option<String> {
    okilum_core::note_title::from_bytes(head)
}

// Below this width the icon remains; only its shortcut hint is hidden.
fn toolbar_text_width(text: &str, weight: FontWeight, window: &Window) -> f32 {
    let mut font = window.text_style().font();
    font.weight = weight;
    let run = gpui::TextRun {
        len: text.len(),
        font,
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(
            text.to_owned().into(),
            px(brand::READER_CHROME_FONT_SIZE),
            &[run],
            None,
        )
        .width()
        .as_f32()
}

#[cfg(test)]
mod agent_publication_tests;

/// Decoration collection holding the language marks of one source editor.
struct AgentLanguageMarks {
    input: EntityId,
    /// [`reader_agent_language::Controller::publication`] these marks show.
    capture: Option<u64>,
    collection: gpui_component::input::TextDecorationCollection,
}

/// One displayed agent source and the exact inventory publication used by its
/// read-only context. The unique token prevents navigate-away/back ABA results.
struct AgentDocument {
    root: PathBuf,
    path: String,
    source: Arc<str>,
    inventory: u64,
    inventory_source: Arc<Vault>,
    token: Arc<()>,
    imports: reader_agent_imports::LiveImportView,
    expanded_lines: std::collections::BTreeSet<(PathBuf, usize)>,
    expand_all: bool,
    imported_documents: std::collections::BTreeMap<PathBuf, SharedString>,
    notice: Option<String>,
    language_error: Option<String>,
    _preparation: Option<Task<()>>,
}

const USAGE: &str = "\
okilum — a reader for a plain-Markdown vault

USAGE:
    okilum [FILE.md | FOLDER] [--vault <PATH>] [OPTIONS]
    okilum --single-file <FILE.md> [OPTIONS]
    okilum sync <COMMAND>   Pair computers without a window (Linux, macOS; see okilum sync --help)

OPTIONS:
    --managed-workspace       Open the saved Brain/workspace chooser explicitly.
    --brain-endpoint <IP:PORT>  Open the AI Brain POC using a loopback backend.
    --vault <PATH>       Read-only vault root (or set OKILUM_VAULT).
    --single-file        Open the file in standalone single-file mode without a vault index.
    --index-dir <PATH>   Search index location [default: application cache]
    --note <REL>         Note to open, relative to the vault root
    --query <TEXT>       Open full-text search with this query
    --jump               Open the first search hit, scrolled to the match
    --copy-source        Copy selections as Markdown source instead of plain text
    --no-history         Record no recent vaults or files (or set OKILUM_NO_HISTORY=1)
    --html               Render the content pane as HTML instead of Markdown
    --uninstall-data     Remove this user's Okilum data, keeping your notes (Linux)
    --build-info         Print version and build facts for QA tools, then exit
    -h, --help           Print this help
";

fn main() {
    // OpenSSH runs this binary as SSH_ASKPASS for a remote Markdown link
    // (#1610): forward the prompt to the running app and exit before any
    // app, updater or GUI initialization.
    if let Some(code) = okilum_core::ssh_askpass::helper_entry() {
        std::process::exit(code);
    }
    #[cfg(feature = "data-view-native-harness")]
    if reader_viewer_native::entry() {
        return;
    }
    if let Some(code) = reader_service_storage::command_line() {
        std::process::exit(code);
    }
    #[cfg(target_os = "macos")]
    if let Err(error) = initialize_native_sync_qa() {
        diagnostic_error!("Native Sync isolation: {error:#}");
        std::process::exit(1);
    }
    #[cfg(feature = "data-view-native-harness")]
    if reader_delimited::native_harness::entry() {
        return;
    }
    #[cfg(feature = "data-view-native-harness")]
    if reader_code_file::native_harness::entry() {
        return;
    }
    #[cfg(feature = "embed-native-harness")]
    if reader_image_actions::native_harness::entry() {
        return;
    }
    if let Err(error) = linux_sprint::initialize() {
        diagnostic_error!("Linux Sprint isolation: {error:#}");
        std::process::exit(1);
    }
    #[cfg(all(target_os = "linux", feature = "appimage-acceptance-harness"))]
    if let Some(result) = okilum_appimage::acceptance_entry(
        env!("OKILUM_BUILD_VERSION"),
        env!("OKILUM_SOURCE_COMMIT"),
    ) {
        match result {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                diagnostic_error!("AppImage acceptance: {error:#}");
                std::process::exit(1);
            }
        }
    }
    // The clipboard reader must not initialize the updater, application state,
    // background services or a second GUI instance.
    #[cfg(windows)]
    if let Some(code) = platform::exact_windows_clipboard::helper_entry() {
        std::process::exit(code);
    }
    // #1256: glibc gives each allocating thread its own arena. Background
    // classification and layout churn fragmented those arenas, and RSS rose by
    // megabytes per edit cycle with a flat live heap. Two arenas keep it level.
    // Must run before any thread exists.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: mallopt only adjusts allocator parameters; no other thread runs yet.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
    }
    // `okilum sync …` is headless: it exits before the updater, single-instance
    // handoff, AppImage re-exec or any GPUI initialization (#2589).
    #[cfg(unix)]
    if let Some(code) = reader_settings_sync::cli::entry() {
        std::process::exit(code);
    }
    // `okilum t3-sign-in …` is headless too: it renews the T3 MCP token with a
    // pairing code and writes it as this signed app (see t3_sign_in_cli).
    if let Some(code) = t3_sign_in_cli::entry() {
        std::process::exit(code);
    }
    #[cfg(windows)]
    let startup_reservation = reader_instance::reserve_windows_launch();
    #[cfg(windows)]
    velopack::VelopackApp::build()
        .set_auto_apply_on_startup(
            startup_reservation
                .as_ref()
                .is_some_and(|reservation| reservation.allows_auto_apply())
                && updater::apply_pending_at_start(),
        )
        .on_after_install_fast_callback(|_| {
            markdown_handler::install();
            okilum_core::folder_registration::install_current();
            url_protocol::install();
        })
        .on_after_update_fast_callback(|_| {
            markdown_handler::install();
            okilum_core::folder_registration::install_current();
            url_protocol::install();
        })
        .on_before_uninstall_fast_callback(|_| app_footprint::uninstall())
        .run();
    let process_start = std::time::Instant::now();
    std::thread::spawn(app_footprint::sweep_stale_search_sessions);
    // Before any thread exists (see `init_local_offset`).
    reader_properties::init_local_offset();
    let offset_elapsed = process_start.elapsed();
    // Restricted pasteboard-only child must not acquire its parent's GUI lock.
    #[cfg(target_os = "macos")]
    if let Some(code) = platform::exact_macos_clipboard::helper_entry() {
        std::process::exit(code);
    }
    let diagnostics = reader_diagnostics::Trace::new_at(None, None, process_start);
    // A QA binary built with GPUI test-support behaves differently (#1043).
    diagnostics.event(
        "build_info",
        serde_json::json!({ "gpui_test_support": build_info::gpui_test_support() }),
    );
    diagnostics.event(
        "local_offset_init",
        serde_json::json!({ "duration_ms": offset_elapsed.as_secs_f64() * 1000. }),
    );
    let defaults = Opts {
        diagnostics: Some(diagnostics.clone()),
        vault: std::env::var_os("OKILUM_VAULT").map(PathBuf::from),
        index_dir: std::env::var_os("OKILUM_INDEX_DIR").map(PathBuf::from),
        ..Opts::default()
    };
    #[cfg(all(target_os = "linux", feature = "appimage-acceptance-harness"))]
    let arguments = appimage_acceptance::arguments();
    #[cfg(not(all(target_os = "linux", feature = "appimage-acceptance-harness")))]
    let arguments = std::env::args_os().skip(1);
    let opts = match reader_open::parse_args(arguments, defaults) {
        Ok(reader_open::Command::Help) => {
            print!("{USAGE}");
            return;
        }
        Ok(reader_open::Command::BuildInfo) => {
            print!("{}", build_info::report());
            return;
        }
        Ok(reader_open::Command::Launch(opts)) => *opts,
        Ok(reader_open::Command::UninstallData { assume_yes }) => {
            #[cfg(target_os = "linux")]
            std::process::exit(app_footprint::uninstall_data(assume_yes));
            #[cfg(not(target_os = "linux"))]
            {
                let _ = assume_yes;
                diagnostic_error!(
                    "--uninstall-data is for Linux. On Windows use Settings → Apps; on macOS \
                     use Okilum → Settings → Uninstall Okilum…"
                );
                std::process::exit(2);
            }
        }
        Err(error) => {
            diagnostic_error!("{error:#}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    if opts.no_history {
        reader_history::disable_recording();
    }

    #[cfg(windows)]
    let opts = {
        let mut opts = opts;
        // Preserve relative CLI arguments before selecting the packaged HLSL base.
        let cwd = std::env::current_dir().expect("Cannot read launch directory");
        for value in [&mut opts.vault, &mut opts.open_path, &mut opts.index_dir]
            .into_iter()
            .flatten()
            .chain(opts.extra_logs.iter_mut())
        {
            if value.is_relative() {
                *value = cwd.join(&*value);
            }
        }
        let executable = std::env::current_exe().expect("Cannot locate Okilum executable");
        std::env::set_current_dir(executable.parent().unwrap())
            .expect("Cannot open Okilum resource directory");
        opts
    };

    // Establish the per-user executable before instance handoff and updater
    // initialization. The runtime then supplies the canonical APPIMAGE path.
    #[cfg(target_os = "linux")]
    if !linux_sprint::enabled() && opts.brain_endpoint.is_none() && !opts.managed_workspace {
        app_footprint::enter_managed_appimage();
    }

    let (instance_lock, instance_requests) = if opts.brain_endpoint.is_none()
        && !opts.managed_workspace
    {
        #[cfg(windows)]
        let connection = reader_instance::connect_reserved(&opts, startup_reservation);
        #[cfg(not(windows))]
        let connection = reader_instance::connect(&opts);
        match connection {
            Ok(reader_instance::Instance::Forwarded) => return,
            Ok(reader_instance::Instance::Primary(lock, requests)) => (Some(lock), Some(requests)),
            Err(error) => {
                diagnostic_error!("Cannot start Reader: {error:#}");
                std::process::exit(1);
            }
        }
    } else {
        (None, None)
    };
    let _instance_lock = instance_lock;
    // Only the instance that owns the profile answers paired computers (#2589).
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    let serves_sync = _instance_lock.is_some();
    // A forwarded launch never races the active Reader's apply operation. The
    // primary repairs interrupted decisions before any path validation, cache
    // load, indexing, or managed Sync initialization can observe missing notes.
    #[cfg(target_os = "linux")]
    let conflict_startup_messages = if _instance_lock.is_some() {
        reader_sync_conflicts::recover_before_open(&opts)
    } else {
        Vec::new()
    };
    #[cfg(target_os = "linux")]
    if _instance_lock.is_some() {
        app_footprint::install_appimage_desktop();
    }
    #[cfg(target_os = "macos")]
    if _instance_lock.is_some() {
        if let Err(error) = okilum_shell_kit::macos_cli::install() {
            diagnostic_error!("Cannot install the Okilum terminal command: {error}");
        }
    }

    let platform_phase = diagnostics.phase("platform_application");
    let clipboard = platform::managed_clipboard();
    // Shaped lines outlive GPUI's one-frame cache (#1312); on Windows the
    // right-to-left glyph positions are repaired under that cache (#1121).
    let app = platform::text_platform::application().with_assets(Assets);
    drop(platform_phase);
    // Register before launch: macOS can deliver files before the launch callback.
    let (open_tx, open_rx) = async_channel::unbounded();
    #[cfg(target_os = "macos")]
    folder_service::install(open_tx.clone());
    app.on_open_urls(move |urls| {
        let _ = open_tx.try_send(urls);
    });
    app.on_reopen(reader_startup::reopen);
    app.run(move |cx| {
        #[cfg(target_os = "linux")]
        cx.set_global(reader_sync_conflicts::StartupNotice(
            conflict_startup_messages,
        ));
        diagnostics.event("app_run_callback", serde_json::json!({}));
        // Editor icons for «Open in ▸» load on their own thread (#1400).
        open_in::warm_icons();
        reader_reminder_notify::install(cx);
        cx.set_global(reader_diagnostics::LaunchTrace(diagnostics.clone()));
        diagnostics.record_exits(cx);
        reader_features::install_defaults(cx);
        let recovery_phase = diagnostics.phase("recovery_and_window_state");
        if let Ok(directory) = opts
            .session_directory
            .clone()
            .map(Ok)
            .unwrap_or_else(reader_history::state_directory)
        {
            let config_directory = reader_layout::config_base().map(|base| base.join("okilum"));
            reader_recovery::install(&directory, config_directory.as_deref(), cx);
            reader_features::install(&directory, cx);
            reader_ui_state::install(&directory, cx);
            reader_open_windows::install(directory.clone(), cx);
            window_state::install(directory, cx);
        }
        drop(recovery_phase);
        cx.set_global(platform::ManagedClipboard(clipboard));
        // Notes take images and rich text where the platform can read them (#1602).
        if let Some(clipboard) = note_paste::platform_note_clipboard() {
            cx.set_global(note_paste::ManagedNoteClipboard(clipboard));
        }
        note_paste::install_menu(cx);
        let components_phase = diagnostics.phase("components_init");
        gpui_component::init(cx);
        reader_toast::init(cx);
        okilum_shell_platform::hooks::install(
            env!("OKILUM_RELEASE_VERSION"),
            reader_toast::error,
            reader_files::checked_path,
            open_in::prepare_open_note,
            reader_shortcuts::shortcut,
            reader_shortcuts::hint,
            cx,
        );
        okilum_shell_text::hooks::install(
            reader_diagnostics::window_close_requested,
            |image, logical_size| {
                reader_image::ReaderImage::new(image.into())
                    .with_logical_size(logical_size)
                    .into_any_element()
            },
            reader_text_zoom::document_style,
            reader_fonts::note,
            cx,
        );
        okilum_shell_viewers::hooks::install(
            okilum_shell_viewers::hooks::ViewerHooks {
                code_font: reader_fonts::code,
                note_font: reader_fonts::note,
                toast_error: |message, window, cx| reader_toast::error(message, window, cx),
                toast_transient: |message, window, cx| reader_toast::transient(message, window, cx),
                toast_duckdb_error: |message, window, cx| {
                    reader_toast::duckdb_error(message, window, cx)
                },
                open_file: |root, rel, window, cx| {
                    reader_files::run(reader_files::FileAction::Open, root, rel, window, cx)
                },
                image_source: reader_files::image_source,
                #[cfg(target_os = "macos")]
                start_thumbnail: Some(reader_thumbnail::native::start),
                #[cfg(not(target_os = "macos"))]
                start_thumbnail: None,
            },
            cx,
        );
        okilum_shell_logs::hooks::install(
            okilum_shell_logs::hooks::LogsHooks {
                release_version: env!("OKILUM_RELEASE_VERSION"),
                source_commit: env!("OKILUM_SOURCE_COMMIT"),
                toast_error: |message, window, cx| reader_toast::error(message, window, cx),
                toast_transient: |message, window, cx| reader_toast::transient(message, window, cx),
                diagnostics_trace: |cx| {
                    reader_diagnostics::trace(cx).map(|trace| {
                        Arc::new(move |phase: &'static str, details: serde_json::Value| {
                            trace.event(phase, details);
                        }) as okilum_shell_logs::hooks::DiagnosticsEventFn
                    })
                },
            },
            cx,
        );
        okilum_shell_project::hooks::install(
            okilum_shell_project::hooks::ProjectHooks {
                show_t3: reader_settings::show_t3,
                agents_enabled: |cx| reader_features::enabled(reader_features::Feature::Agents, cx),
                agents_access: |cx| reader_features::access(reader_features::Feature::Agents, cx),
                set_agents_enabled: |enabled, cx| {
                    reader_features::set_enabled(reader_features::Feature::Agents, enabled, cx)
                },
                code_font: reader_fonts::code,
                toast_error: |message, window, cx| reader_toast::error(message, window, cx),
                toast_transient: |message, window, cx| reader_toast::transient(message, window, cx),
                diagnostics_trace: |cx| {
                    reader_diagnostics::trace(cx).map(|trace| {
                        Arc::new(move |phase: &'static str, details: serde_json::Value| {
                            trace.event(phase, details);
                        })
                            as okilum_shell_project::hooks::DiagnosticsEventFn
                    })
                },
            },
            cx,
        );
        #[cfg(all(unix, feature = "brain"))]
        okilum_shell_brain::hooks::install(
            okilum_shell_brain::hooks::BrainHooks {
                build_menus: |cx| {
                    vec![
                        Menu {
                            name: "Okilum".into(),
                            items: updater::menu_items(MenuItem::action(
                                "Quit Okilum",
                                brain::app_quit::Quit,
                            )),
                            disabled: false,
                        },
                        reader_open::file_menu(),
                        note_paste::edit_menu(),
                        reader_editor::format::format_menu(),
                        view_menu(cx),
                        version_info::help_menu(),
                    ]
                },
                confirm_deleted_for_quit: reader_editor::confirm_deleted_for_quit,
                save_all_for_quit: reader_editor::save_all_for_quit,
                save_window: reader_editor::save_window,
                window_close_requested: reader_diagnostics::window_close_requested,
                show_features: reader_settings::show_features,
                text_menu: reader_code_file::text_menu,
                second_brain_enabled: |cx| {
                    reader_features::enabled(reader_features::Feature::SecondBrain, cx)
                },
                second_brain_access: |cx| {
                    reader_features::access(reader_features::Feature::SecondBrain, cx)
                },
            },
            cx,
        );
        source_presentation::code_languages::install();
        reader_mermaid::register();
        drop(components_phase);
        let services_phase = diagnostics.phase("menus_and_updater");
        bind_keys(cx);
        reader_open::install(cx);
        reader_open::receive_events(open_rx, cx);
        if let Some(requests) = instance_requests {
            reader_instance::receive(requests, cx);
        }
        #[cfg(all(unix, feature = "brain"))]
        {
            brain::bind_keys(cx);
            brain::source_trace::install(cx);
        }
        updater::install(cx);
        // Paired computers reach this app without Settings → Sync (#2589).
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        if serves_sync {
            reader_settings_sync::serve_paired(cx);
        }
        #[cfg(all(target_os = "linux", feature = "appimage-acceptance-harness"))]
        appimage_acceptance::install(cx);
        version_info::install(cx);
        #[cfg(all(unix, feature = "brain"))]
        brain::app_quit::install(cx);
        #[cfg(not(all(unix, feature = "brain")))]
        reader_app_menu::install(cx);
        drop(services_phase);
        let fonts_phase = diagnostics.phase("fonts_load");
        if let Err(error) = brand::load_fonts(cx) {
            diagnostic_error!("Embedded typography unavailable: {error}");
        }
        reader_fonts::install(cx);
        reader_keymap::install(cx);
        drop(fonts_phase);
        let appearance_phase = diagnostics.phase("appearance_load");
        let (appearance, theme) = load_appearance(cx);
        cx.set_global(appearance);
        cx.set_global(theme);
        // Native window construction must already see the persisted palette.
        // The per-window callback still handles system appearance updates.
        match cx.global::<AppearancePreference>().0 {
            Some(mode) => Theme::change(mode, None, cx),
            None => Theme::sync_system_appearance(None, cx),
        }

        brand::apply_theme(cx);
        #[cfg(feature = "project-ui-harness")]
        if project::open_harness(cx) {
            return;
        }
        #[cfg(feature = "kubernetes-ui-harness")]
        if kubernetes_workspace::harness::open(cx) {
            return;
        }
        // A theme can swap the fonts behind a run; shaped lines start over.
        cx.observe_global::<Theme>(|_| {
            platform::shaped_line_cache::ShapedLineCache::clear_installed()
        })
        .detach();
        cx.activate(true);
        drop(appearance_phase);

        // A macOS reopen with no window starts like a plain launch,
        // whatever this launch opened (#1137).
        cx.set_global(reader_startup::ReopenBase(Opts {
            session_directory: opts.session_directory.clone(),
            index_dir: opts.index_dir.clone(),
            diagnostics: opts.diagnostics.clone(),
            ..Opts::default()
        }));
        #[cfg(target_os = "macos")]
        let opts = reader_update_restart::with_pending(opts);
        if !opts.restart_windows.is_empty() {
            reader_update_restart::restore(opts, cx).detach();
            return;
        }
        if let Some(link) = &opts.link {
            reader_open::open_deep_link(link, cx);
            return;
        }
        if (opts.vault.is_some() || opts.open_path.is_some()) && opts.brain_endpoint.is_none() {
            if let Err(error) = reader_open::open_window(opts, cx) {
                diagnostic_error!("Cannot open Reader: {error:#}");
            }
            return;
        }
        if opts.brain_endpoint.is_none() && !opts.managed_workspace {
            reader_startup::launch(opts, cx);
            #[cfg(all(unix, feature = "brain"))]
            return;
        }
        #[cfg(all(unix, feature = "brain"))]
        {
            if !reader_features::enabled(reader_features::Feature::SecondBrain, cx) {
                let mut opts = opts;
                opts.brain_endpoint = None;
                opts.managed_workspace = false;
                reader_startup::launch(opts, cx);
                reader_settings::show_features(cx);
                return;
            }
            let bounds = window_state::default_bounds(size(px(1500.), px(1000.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(600.), px(400.))),
                kind: WindowKind::Normal,
                // Wayland app id. Also the handle test tooling keys on, so it
                // is part of the contract, not decoration.
                app_id: Some(linux_sprint::app_id().into()),
                #[cfg(target_os = "linux")]
                window_background: WindowBackgroundAppearance::Opaque,
                ..TitleBar::window_options()
            };
            let (options, frame_key) = window_state::prepare(options, "managed", cx);
            cx.open_window(options, |window, cx| {
                window.set_window_title("Okilum");
                // Follow the desktop's light/dark preference; `init` forces Light.
                sync_appearance(window, cx);
                // ...and keep following it while the app runs (#37). On Linux
                // gpui sources this from the xdg-desktop-portal setting
                // `org.freedesktop.appearance color-scheme` and re-fires the
                // window callback on every change. `Theme::change` already
                // pushes the Base theme and the TextView defaults; `sync_base`
                // stays as a belt-and-braces call in case a vendor rebase moves
                // that out of `change`. The font overrides set above survive
                // the flip because the stock theme configs carry no font fields.
                window
                    .observe_window_appearance(|window, cx| {
                        sync_appearance(window, cx);
                        window.refresh();
                    })
                    .detach();
                let root = if let Some(endpoint) = opts.brain_endpoint {
                    window.set_window_title("Okilum — AI Brain");
                    let view = cx.new(|cx| brain::BrainView::new(endpoint, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                } else {
                    #[cfg(target_os = "macos")]
                    let opts = Opts {
                        defer_saved_connection: true,
                        ..opts
                    };
                    let workspace = cx.new(|cx| workspace::Workspace::new(opts, window, cx));
                    cx.new(|cx| Root::new(workspace, window, cx))
                };
                window_state::track(&root, frame_key, window, cx);
                root
            })
            .expect("failed to open window");
        }
    });
}

#[cfg(test)]
mod hidden_files_shortcut_tests;

#[cfg(test)]
mod document_link_landing_tests;

#[cfg(test)]
mod reader_scroll_key_tests;

/// Explicit acceptance scope before state, threads or services are opened. A
/// Reader profile alone never redirects the production Sync connection.
#[cfg(target_os = "macos")]
fn initialize_native_sync_qa() -> anyhow::Result<()> {
    use okilum_sync_controller::sidecar::macos::{account_state_directory, isolation};
    let Some(root) = std::env::var_os("OKILUM_SYNC_QA_ROOT") else {
        return Ok(());
    };
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("QA HOME missing"))?;
    let state = reader_history::isolated_state_directory()
        .ok_or_else(|| anyhow::anyhow!("QA requires explicit absolute OKILUM_STATE_DIR"))?;
    isolation::select(isolation::Profile::prepare(
        std::path::Path::new(&root),
        std::path::Path::new(&home),
        &state,
        &account_state_directory()?,
    )?)
}

#[cfg(test)]
mod issue2821_tests;

#[cfg(test)]
mod issue2825_stack_tests;

#[cfg(test)]
mod issue2957_tests;

#[cfg(test)]
mod issue2957_focus_kept_tests;

#[cfg(test)]
mod issue1669_test_teardown_tests;

#[cfg(test)]
mod action_inventory_tests;

#[cfg(test)]
mod brand_reader_style_tests;
